#!/usr/bin/env python3
"""Live source precedence, disabled-entry and review-cancellation acceptance."""
import json
import os
from pathlib import Path
import runpy
import sys
import tempfile

# Probe the effective child environment/cwd without returning private values.
if sys.argv[1] in ('--base-probe', '--override-probe'):
    override = sys.argv[1] == '--override-probe'
    expected = 'MCP_PROFILE_OVERRIDE_PRIVATE_SENTINEL' if override else 'MCP_SOURCE_PRECEDENCE_PRIVATE_SENTINEL'
    assert os.environ.get('PRIVATE_TEST_VALUE') == expected, 'wrong effective environment'
    if override:
        assert Path.cwd() == Path(sys.argv[2]).parent / 'override cwd', 'wrong effective cwd'
    runpy.run_path(str(Path(__file__).with_name('mcp-discovery.py')))['server'](sys.argv[2])
    raise SystemExit(0)

UnixHTTP = runpy.run_path(str(Path(__file__).with_name('mcp-discovery.py')))['UnixHTTP']
runtime, config, scratch = map(Path, sys.argv[1:4])
profile = 'display-source-precedence'
user = config / 'mcp.json'
profiles = config / 'mcp-profiles.json'
# run.sh launches the daemon from harness-owned runtime storage.
ambient = runtime / 'mcp.json'
backups = {p: p.read_bytes() if p.exists() else None for p in (user, profiles, ambient)}
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
        assert all(value.encode() not in data for value in (sentinel, 'MCP_PROFILE_OVERRIDE_PRIVATE_SENTINEL')), 'private fixture value exposed in API'
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
                str(Path(__file__).resolve()), '--base-probe', str(marker)],
                'env': {'PRIVATE_TEST_VALUE': sentinel}, 'enabled': enabled}
        ambient.write_text(json.dumps({'mcpServers': {'ambient': definition('ambient-unselected')}}))
        overridden = definition('user-winner')
        overridden['command'] = str(directory / 'must-not-execute')
        overridden['args'][1] = '--override-probe'
        overridden['cwd'] = str(directory)
        override_cwd = directory / 'override cwd'
        override_cwd.mkdir()
        user.write_text(json.dumps({'mcpServers': {
            'duplicate': overridden,
            'disabled': definition('user-disabled', False)}}))
        source = directory / 'selected.json'
        source.write_text(json.dumps({'mcpServers': {
            'duplicate': definition('selected-shadowed'),
            'disabled': definition('selected-enabled-shadowed'),
            'selected_only': definition('selected-winner'),
            'excluded': definition('selected-excluded')}}))
        profile_data = {'active': 'default', 'profiles': {
            'default': {'servers': []}, profile: {
                'servers': ['duplicate', 'disabled', 'selected_only'],
                'config': {
                    'duplicate': {'command': sys.executable, 'cwd': str(override_cwd),
                                  'env.PRIVATE_TEST_VALUE': 'MCP_PROFILE_OVERRIDE_PRIVATE_SENTINEL'},
                    'disabled': {'env.PRIVATE_TEST_VALUE': 'MCP_PROFILE_OVERRIDE_PRIVATE_SENTINEL'}}}}}
        profiles.write_text(json.dumps(profile_data))
        def preview():
            result = request(True, 'POST', '/mcp/discovery', {'profile': profile, 'project_path': str(source)})
            rows = {(s['source'], s['name']): s['status'] for s in result['servers']}
            assert rows == {
                ('user', 'duplicate'): 'selected', ('user', 'disabled'): 'disabled',
                ('selected_project', 'duplicate'): 'shadowed',
                ('selected_project', 'disabled'): 'shadowed',
                ('selected_project', 'selected_only'): 'selected',
                ('selected_project', 'excluded'): 'not_in_profile'}, 'unexpected source provenance'
            for candidate in result['servers']:
                expected_overrides = []
                if candidate['source'] == 'user':
                    expected_overrides = ['env.PRIVATE_TEST_VALUE'] if candidate['name'] == 'disabled' else ['command', 'cwd', 'env.PRIVATE_TEST_VALUE']
                assert sorted(candidate['overrides']) == expected_overrides, 'override provenance applied to wrong source'
            assert not any(p.exists() for p in markers.values()), 'preview started a process'
            return result['review_id']
        cancelled = preview()
        request(True, 'POST', '/mcp/trust', {'action': 'cancel', 'review_id': cancelled}, 204)
        request(True, 'POST', '/mcp/trust', {'action': 'approve', 'review_id': cancelled}, 409)
        changed = preview()
        with source.open('a') as stream:
            stream.write('\n// source changed after review\n')
        request(True, 'POST', '/mcp/trust', {'action': 'approve', 'review_id': changed}, 409)
        profile_changed = preview()
        original_profiles = profiles.read_bytes()
        profiles.write_text(json.dumps({'active': 'default', 'profiles': {'default': {'servers': []}}}))
        request(True, 'POST', '/mcp/trust', {'action': 'approve', 'review_id': profile_changed}, 409)
        profiles.write_bytes(original_profiles)
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
        # Omit project selection after revoking the previous explicit grant.
        # A named profile member does not authorize scanning the daemon cwd.
        request(True, 'POST', '/mcp/trust', {'action': 'revoke', 'profile': profile}, 204)
        approved = False
        profile_data['profiles'][profile]['servers'] = ['duplicate', 'disabled', 'ambient']
        profiles.write_text(json.dumps(profile_data))
        request(True, 'POST', '/mcp/discovery', {'profile': profile}, 502)
        assert not markers['ambient-unselected'].exists(), 'unknown member caused execution'
        profile_data['profiles'][profile]['servers'] = ['duplicate', 'disabled']
        profiles.write_text(json.dumps(profile_data))
        unselected = request(True, 'POST', '/mcp/discovery', {'profile': profile})
        assert {(s['source'], s['name']) for s in unselected['servers']} == {
            ('user', 'duplicate'), ('user', 'disabled')}, 'unselected project imported'
        request(True, 'POST', '/mcp/trust', {'action': 'approve', 'review_id': unselected['review_id']}, 204)
        approved = True
        session = request(False, 'POST', '/session', {'purpose': 'project opt-in qualification',
            'mcp_profile': profile, 'recipes': [], 'agent': {'name': 'display', 'instructions': ''}, 'skills': []}, 201)
        sid = session['id']
        catalog = request(False, 'GET', f'/session/{sid}/mcp')
        assert [server['name'] for server in catalog['servers']] == ['duplicate']
        assert catalog['servers'][0]['healthy']
        assert not markers['ambient-unselected'].exists(), 'ambient project executed'
        assert len(markers['user-winner'].read_text().splitlines()) == 2
        assert len(markers['selected-winner'].read_text().splitlines()) == 1, 'old selection retained'
        request(False, 'DELETE', f'/session/{sid}', expected=204)
        sid = None
        print('PASS project-opt-in cwd-not-imported named-member-not-authorized previous-selection-not-retained user-still-attaches', flush=True)
        print('PASS profile-overrides command cwd environment provenance disabled-stays-disabled unchanged-source-isolated stale-profile-review-denied', flush=True)
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
