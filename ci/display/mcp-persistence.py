#!/usr/bin/env python3
"""Phased real-daemon restart qualification; the shell owns process lifecycle."""
import json
import os
from pathlib import Path
import runpy
import stat
import sys

UnixHTTP = runpy.run_path(str(Path(__file__).with_name('mcp-discovery.py')))['UnixHTTP']
phase, runtime, config, scratch = sys.argv[1:]
runtime, config, scratch = map(Path, (runtime, config, scratch))
profile = 'display-persistence'
source = scratch / 'selected.json'
marker = scratch / 'events'
profiles = config / 'mcp-profiles.json'
backup = scratch / 'profiles-backup.json'
sentinel = 'MCP_PERSISTENCE_PRIVATE_SENTINEL'


def request(companion, method, endpoint, payload=None, expected=200):
    connection = UnixHTTP(runtime / ('companion.sock' if companion else 'control.sock'))
    try:
        connection.request(method, endpoint, None if payload is None else json.dumps(payload),
                           {'Content-Type': 'application/json'})
        response = connection.getresponse()
        data = response.read()
        assert response.status == expected, f'{method} {endpoint}: {response.status}, expected {expected}'
        assert sentinel.encode() not in data, 'private value in API response'
        return json.loads(data) if data else None
    finally:
        connection.close()


def events():
    return marker.read_text() if marker.exists() else ''


def approve():
    before = events()
    preview = request(True, 'POST', '/mcp/discovery', {'profile': profile, 'project_path': str(source)})
    request(True, 'POST', '/mcp/trust', {'action': 'approve', 'review_id': preview['review_id']}, 204)
    assert events() == before, 'review or approval started a server'


def attach(expected=201):
    before = events()
    result = request(False, 'POST', '/session', {'purpose': 'restart qualification',
        'mcp_profile': profile, 'recipes': [], 'agent': {'name': 'display', 'instructions': ''}, 'skills': []}, expected)
    if expected != 201:
        assert events() == before, 'denied attachment started a server'
        return
    sid = result['id']
    try:
        catalog = request(False, 'GET', f'/session/{sid}/mcp')
        assert [tool['name'] for tool in catalog['servers'][0]['tools']] == ['ping', 'crash']
        assert events().count('start ') == before.count('start ') + 1
    finally:
        request(False, 'DELETE', f'/session/{sid}', expected=204)


if phase == 'prepare':
    backup.write_text(json.dumps(profiles.read_text() if profiles.exists() else None))
    profiles.write_text(json.dumps({'active': 'default', 'profiles': {
        'default': {'servers': []}, profile: {'servers': ['fixture']}}}))
    source.write_text(json.dumps({'mcpServers': {'fixture': {'command': sys.executable,
        'args': [str(Path(__file__).with_name('mcp-discovery.py').resolve()), '--server', str(marker)],
        'env': {'PRIVATE_TEST_VALUE': sentinel}}}}))
    approve()
    attach()
elif phase == 'unchanged':
    # Fresh daemon must reconstruct the approval from private on-disk receipts.
    attach()
    with source.open('a') as stream:
        stream.write('\n// changed original bytes; semantic configuration unchanged\n')
elif phase == 'changed':
    attach(403)
    approve()
    attach()
    request(True, 'POST', '/mcp/trust', {'action': 'revoke', 'profile': profile}, 204)
elif phase == 'revoked':
    attach(403)
    assert events().count('start ') == 3, 'unexpected startup across restarts'
    assert 'call ' not in events(), 'qualification unexpectedly executed a tool'
    # No credentials are copied into the durable ledger; Unix ownership stays private.
    receipt = config / 'mcp-trust' / 'approvals.json'
    metadata = receipt.stat()
    assert stat.S_IMODE(metadata.st_mode) == 0o600 and metadata.st_uid == os.getuid()
    assert sentinel not in receipt.read_text(), 'private value persisted in trust receipt'
    print('PASS persistence unchanged-restore changed-source-denied reapproval durable-revocation private-receipt', flush=True)
elif phase == 'cleanup':
    if backup.exists():
        original = json.loads(backup.read_text())
        if original is None:
            profiles.unlink(missing_ok=True)
        else:
            profiles.write_text(original)
else:
    raise SystemExit('unknown persistence phase')
