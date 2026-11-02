#!/usr/bin/env python3
"""Abrupt daemon death with a pipe-aware stdio call in flight (Linux only)."""
import http.client
import json
import os
from pathlib import Path
import runpy
import select
import signal
import sys
import threading
import time


def server(marker):
    with open(marker, 'a') as audit:
        def event(value):
            audit.write(value + '\n')
            audit.flush()
        event(f'start {os.getpid()}')
        for line in sys.stdin:
            req = json.loads(line)
            if 'id' not in req:
                continue
            method = req['method']
            if method == 'initialize':
                result = {'protocolVersion': '2025-11-25', 'capabilities': {'tools': {}},
                          'serverInfo': {'name': 'crash-fixture', 'version': '1'}}
            elif method == 'tools/list':
                result = {'tools': [{'name': 'hold', 'inputSchema': {'type': 'object'}}]}
            elif method == 'tools/call':
                assert req['params']['name'] == 'hold'
                event('call hold')  # observable side effect, before any response
                # This fixture exits on pipe closure; it does not model a server
                # ignoring EOF or independently daemonizing descendants.
                assert sys.stdin.read() == ''
                event('eof')
                return
            else:
                raise AssertionError('unexpected method')
            print(json.dumps({'jsonrpc': '2.0', 'id': req['id'], 'result': result}), flush=True)


if sys.argv[1] == '--server':
    server(sys.argv[2])
    raise SystemExit(0)

UnixHTTP = runpy.run_path(str(Path(__file__).with_name('mcp-discovery.py')))['UnixHTTP']
phase = sys.argv[1]
runtime, config, scratch = map(Path, sys.argv[2:5])
profile = 'display-daemon-crash'
profiles = config / 'mcp-profiles.json'
backup = scratch / 'profiles-backup.json'
source = scratch / 'selected.json'
marker = scratch / 'events'


def request(companion, method, endpoint, payload=None, expected=200):
    conn = UnixHTTP(runtime / ('companion.sock' if companion else 'control.sock'))
    try:
        conn.request(method, endpoint, None if payload is None else json.dumps(payload),
                     {'Content-Type': 'application/json'})
        response = conn.getresponse()
        data = response.read()
        assert response.status == expected, f'{method} {endpoint}: {response.status}, expected {expected}'
        return json.loads(data) if data else None
    finally:
        conn.close()


def attach():
    return request(False, 'POST', '/session', {'purpose': 'daemon crash qualification',
        'mcp_profile': profile, 'recipes': [], 'agent': {'name': 'display', 'instructions': ''}, 'skills': []}, 201)['id']


def events():
    return marker.read_text() if marker.exists() else ''


def until(predicate):
    deadline = time.monotonic() + 8
    while time.monotonic() < deadline:
        if predicate():
            return
        time.sleep(0.02)
    raise AssertionError('expected crash fixture progress was not observed')


if phase == 'before':
    backup.write_text(json.dumps(profiles.read_text() if profiles.exists() else None))
    profiles.write_text(json.dumps({'active': 'default', 'profiles': {
        'default': {'servers': []}, profile: {'servers': ['fixture']}}}))
    source.write_text(json.dumps({'mcpServers': {'fixture': {'command': sys.executable,
        'args': [str(Path(__file__).resolve()), '--server', str(marker)]}}}))
    preview = request(True, 'POST', '/mcp/discovery', {'profile': profile, 'project_path': str(source)})
    request(True, 'POST', '/mcp/trust', {'action': 'approve', 'review_id': preview['review_id']}, 204)
    assert not marker.exists()
    sid = attach()
    (scratch / 'old-session').write_text(sid)
    assert events().count('start ') == 1
    child_pid = int(events().splitlines()[0].split()[1])
    daemon_pid, daemon_exe = int(sys.argv[5]), Path(sys.argv[6]).resolve()
    # pidfds pin the exact processes across observation, SIGKILL and exit checks.
    daemon_fd = os.pidfd_open(daemon_pid)
    child_fd = os.pidfd_open(child_pid)
    try:
        assert Path(f'/proc/{daemon_pid}/exe').resolve() == daemon_exe
        assert Path(f'/proc/{daemon_pid}').stat().st_uid == os.getuid()
        outcome = []
        def call():
            try:
                request(False, 'POST', f'/session/{sid}/mcp/call', {
                    'server': 'fixture', 'tool': 'hold', 'decision': 'approve', 'arguments': {}})
                outcome.append('unexpected reply')
            except (OSError, http.client.HTTPException):
                outcome.append('connection lost')
        thread = threading.Thread(target=call, daemon=True)
        thread.start()
        until(lambda: 'call hold\n' in events())
        signal.pidfd_send_signal(daemon_fd, signal.SIGKILL)
        thread.join(8)
        assert outcome == ['connection lost'], 'call did not fail on daemon death'
        for fd in (daemon_fd, child_fd):
            poll = select.poll()
            poll.register(fd, select.POLLIN)
            assert poll.poll(8000), 'daemon or pipe-aware child survived crash'
        assert events().splitlines()[-1] == 'eof'
        assert events().count('call hold') == 1
        print('PASS abrupt-daemon-death in-flight-disconnect pipe-aware-child-exit', flush=True)
    finally:
        os.close(child_fd)
        os.close(daemon_fd)
elif phase == 'after':
    before = events()
    sid = (scratch / 'old-session').read_text()
    request(False, 'GET', f'/session/{sid}/mcp', expected=404)
    assert request(True, 'GET', '/mcp/status')['session'] is None
    assert events() == before, 'restart restored a session or replayed a call'
    # A new explicit session restores unchanged trust, not old session state.
    new_sid = attach()
    try:
        assert new_sid != sid
        catalog = request(False, 'GET', f'/session/{new_sid}/mcp')
        assert catalog['servers'][0]['healthy'] is True
        assert events().count('start ') == 2
        assert events().count('call hold') == 1, 'in-flight side effect was replayed'
    finally:
        request(False, 'DELETE', f'/session/{new_sid}', expected=204)
    request(True, 'POST', '/mcp/trust', {'action': 'revoke', 'profile': profile}, 204)
    print('PASS crash-restart old-session-absent no-replay explicit-new-session trust-restored', flush=True)
elif phase == 'cleanup':
    if backup.exists():
        original = json.loads(backup.read_text())
        if original is None:
            profiles.unlink(missing_ok=True)
        else:
            profiles.write_text(original)
else:
    raise SystemExit('unknown crash phase')
