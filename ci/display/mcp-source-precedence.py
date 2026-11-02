#!/usr/bin/env python3
"""Live source precedence, disabled-entry and review-cancellation acceptance."""
import json
from pathlib import Path
import runpy
import sys
import tempfile

UnixHTTP = runpy.run_path(str(Path(__file__).with_name('mcp-discovery.py')))['UnixHTTP']
runtime, config, scratch = map(Path, sys.argv[1:4])
profile = 'display-source-precedence'
user = config / 'mcp.json'
profiles = config / 'mcp-profiles.json'
backups = {p: p.read_bytes() if p.exists() else None for p in (user, profiles)}
sid = None
approved = False
sentinel = 'MCP_SOURCE_PRECEDENCE_PRIVATE_SENTINEL'


def request(companion, method, endpoint, payload=None, expected=200):
    connection = UnixHTTP(runtime / ('companion.sock' if companion else 'control.sock'))
    try:
        connection.request(method, endpoint, None if payload is None else json.dumps(payload),
                           {'Content-Type': 'application/json'})
        response = connection.getresponse()
        data = response.read()
        assert response.status == expected, f'{method} {endpoint}: {response.status}, expected {expected}'
        assert sentinel.encode() not in data, 'private fixture value exposed in API'
        return json.loads(data) if data else None
    finally:
        connection.close()


try:
    with tempfile.TemporaryDirectory(prefix='source-precedence-', dir=scratch) as directory:
        directory = Path(directory)
        markers = {}
        def definition(label, enabled=True):
            marker = directory / label
            markers[label] = marker
            return {'command': sys.executable, 'args': [
                str(Path(__file__).with_name('mcp-discovery.py').resolve()), '--server', str(marker)],
                'env': {'PRIVATE_TEST_VALUE': sentinel}, 'enabled': enabled}
        user.write_text(json.dumps({'mcpServers': {
            'duplicate': definition('user-winner'),
            'disabled': definition('user-disabled', False)}}))
        source = directory / 'selected.json'
        source.write_text(json.dumps({'mcpServers': {
            'duplicate': definition('selected-shadowed'),
            'disabled': definition('selected-enabled-shadowed'),
            'selected_only': definition('selected-winner'),
            'excluded': definition('selected-excluded')}}))
        profiles.write_text(json.dumps({'active': 'default', 'profiles': {
            'default': {'servers': []}, profile: {'servers': ['duplicate', 'disabled', 'selected_only']}}}))
        def preview():
            result = request(True, 'POST', '/mcp/discovery', {'profile': profile, 'project_path': str(source)})
            rows = {(s['source'], s['name']): s['status'] for s in result['servers']}
            assert rows == {
                ('user', 'duplicate'): 'selected', ('user', 'disabled'): 'disabled',
                ('selected_project', 'duplicate'): 'shadowed',
                ('selected_project', 'disabled'): 'shadowed',
                ('selected_project', 'selected_only'): 'selected',
                ('selected_project', 'excluded'): 'not_in_profile'}, 'unexpected source provenance'
            assert not any(p.exists() for p in markers.values()), 'preview started a process'
            return result['review_id']
        cancelled = preview()
        request(True, 'POST', '/mcp/trust', {'action': 'cancel', 'review_id': cancelled}, 204)
        request(True, 'POST', '/mcp/trust', {'action': 'approve', 'review_id': cancelled}, 409)
        changed = preview()
        with source.open('a') as stream:
            stream.write('\n// source changed after review\n')
        request(True, 'POST', '/mcp/trust', {'action': 'approve', 'review_id': changed}, 409)
        fresh = preview()
        request(True, 'POST', '/mcp/trust', {'action': 'approve', 'review_id': fresh}, 204)
        approved = True
        assert not any(p.exists() for p in markers.values()), 'approval started a process'
        session = request(False, 'POST', '/session', {'purpose': 'source qualification',
            'mcp_profile': profile, 'recipes': [], 'agent': {'name': 'display', 'instructions': ''}, 'skills': []}, 201)
        sid = session['id']
        catalog = request(False, 'GET', f'/session/{sid}/mcp')
        assert sorted(server['name'] for server in catalog['servers']) == ['duplicate', 'selected_only']
        assert all(server['healthy'] for server in catalog['servers'])
        assert {name for name, path in markers.items() if path.exists()} == {'user-winner', 'selected-winner'}
        for label in ('user-winner', 'selected-winner'):
            events = markers[label].read_text().splitlines()
            assert len(events) == 1 and events[0].startswith('start '), 'unexpected startup or tool call'
        request(False, 'DELETE', f'/session/{sid}', expected=204)
        sid = None
        print('PASS source-precedence user-wins disabled-not-resurrected profile-exclusion selected-only-attaches', flush=True)
        print('PASS source-review cancel-denied changed-snapshot-denied no-start-before-attach private-values-redacted', flush=True)
finally:
    try:
        if sid is not None:
            request(False, 'DELETE', f'/session/{sid}', expected=204)
        if approved:
            request(True, 'POST', '/mcp/trust', {'action': 'revoke', 'profile': profile}, 204)
    finally:
        for path, contents in backups.items():
            if contents is None:
                path.unlink(missing_ok=True)
            else:
                path.write_bytes(contents)
