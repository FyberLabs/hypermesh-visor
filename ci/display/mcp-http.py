#!/usr/bin/env python3
"""Live loopback HTTP MCP negotiation, overrides, refusal and cleanup checks."""
import http.server
import json
from pathlib import Path
import runpy
import sys
import threading

UnixHTTP = runpy.run_path(str(Path(__file__).with_name('mcp-discovery.py')))['UnixHTTP']
runtime, config = map(Path, sys.argv[1:3])
profile = 'display-http'
private = 'MCP_HTTP_PRIVATE_SENTINEL'
path = '/mcp/' + private
protocol = '2025-06-18'
condition = threading.Condition()
events = []
failures = []
initialized = 0
sid = None
approved = False
profiles = config / 'mcp-profiles.json'
source = runtime / 'http-source.json'
backups = {p: p.read_bytes() if p.exists() else None for p in (profiles, source)}


def record(event):
    with condition:
        events.append(event)
        condition.notify_all()


def wait_for(predicate, message):
    with condition:
        assert condition.wait_for(lambda: failures or predicate(), timeout=8), message
        assert not failures, 'HTTP fixture contract failed: ' + ', '.join(failures)


class Handler(http.server.BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass  # Never print credential-bearing paths or headers.

    def reply(self, status, body=None, headers=None):
        data = b'' if body is None else json.dumps(body).encode()
        self.send_response(status)
        self.send_header('Content-Type', 'application/json')
        self.send_header('Content-Length', str(len(data)))
        for key, value in (headers or {}).items():
            self.send_header(key, value)
        self.end_headers()
        self.wfile.write(data)

    def handle_request(self):
        global initialized
        assert self.path == path, 'base URL or redirect target contacted'
        assert self.headers.get('Authorization') == 'Bearer ' + private, 'wrong effective authorization'
        assert self.headers.get('X-Fixture') == 'base-retained', 'base header lost'
        assert self.headers.get('Last-Event-ID') is None, 'configured event ID forwarded'
        if self.command == 'POST':
            size = int(self.headers['Content-Length'])
            assert 0 < size < 65536, 'invalid request size'
            request = json.loads(self.rfile.read(size))
            method = request['method']
        else:
            request, method = {}, self.command
        if method == 'initialize':
            assert self.headers.get('Mcp-Session-Id') is None, 'configured session ID forwarded'
            assert self.headers.get('MCP-Protocol-Version') is None, 'configured protocol forwarded'
            assert request['params']['protocolVersion'] == '2025-11-25', 'unexpected client version'
            initialized += 1
            connection = str(initialized)
            record(('initialize', connection))
            self.reply(200, {'jsonrpc': '2.0', 'id': request['id'], 'result': {
                'protocolVersion': protocol, 'capabilities': {'tools': {'listChanged': True}},
                'serverInfo': {'name': 'http-fixture', 'version': '1'}}}, {'Mcp-Session-Id': connection})
            return
        connection = self.headers.get('Mcp-Session-Id')
        assert connection in ('1', '2'), 'negotiated session ID missing'
        assert self.headers.get('MCP-Protocol-Version') == protocol, 'negotiated version missing'
        if method == 'notifications/initialized':
            record(('initialized', connection))
            self.reply(202)
        elif method == 'GET':
            record(('subscription-unsupported', connection))
            self.reply(405)
        elif method == 'DELETE':
            record(('delete', connection))
            self.reply(405 if connection == '1' else 404)
        elif method == 'tools/list':
            cursor = request.get('params', {}).get('cursor')
            assert cursor in (None, 'page-two'), 'wrong cursor'
            record(('list-first' if cursor is None else 'list-second', connection))
            tool = 'ping' if cursor is None else 'redirect'
            result = {'tools': [{'name': tool, 'inputSchema': {'type': 'object'}}]}
            if cursor is None:
                result['nextCursor'] = 'page-two'
            self.reply(200, {'jsonrpc': '2.0', 'id': request['id'], 'result': result})
        elif method == 'tools/call':
            tool = request['params']['name']
            assert tool in ('ping', 'redirect'), 'unknown tool'
            record(('call-' + tool, connection))
            if tool == 'redirect':
                self.reply(307, headers={'Location': '/must-not-follow'})
            else:
                self.reply(200, {'jsonrpc': '2.0', 'id': request['id'],
                                'result': {'content': [{'type': 'text', 'text': 'pong'}]}})
        else:
            raise AssertionError('unexpected method')

    def guarded(self):
        try:
            self.handle_request()
        except Exception:
            # Safe constant only: exceptions may include endpoint/header values.
            with condition:
                failures.append('request validation')
                condition.notify_all()
            self.reply(500)

    do_GET = do_POST = do_DELETE = guarded


server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Handler)
worker = threading.Thread(target=server.serve_forever)
worker.start()


def request(companion, method, endpoint, payload=None, expected=200):
    connection = UnixHTTP(runtime / ('companion.sock' if companion else 'control.sock'))
    try:
        connection.request(method, endpoint, None if payload is None else json.dumps(payload),
                           {'Content-Type': 'application/json'})
        response = connection.getresponse()
        data = response.read()
        assert response.status == expected, f'{method}: status {response.status}, expected {expected}'
        assert private.encode() not in data, 'private HTTP value exposed in API'
        return json.loads(data) if data else None
    finally:
        connection.close()


try:
    base = f'http://127.0.0.1:{server.server_port}'
    source.write_text(json.dumps({'mcpServers': {'http-fixture': {
        'type': 'http', 'url': base + '/must-not-use-base',
        'headers': {'Authorization': 'wrong-base-value', 'X-Fixture': 'base-retained',
                    'mcp-session-id': 'injected', 'mcp-protocol-version': 'injected',
                    'last-event-id': 'injected'}}}}))
    overrides = {'url': base + path, 'headers.Authorization': 'Bearer ' + private}
    profiles.write_text(json.dumps({'active': 'default', 'profiles': {
        'default': {'servers': []}, profile: {'servers': ['http-fixture'],
            'config': {'http-fixture': overrides}}}}))
    preview = request(True, 'POST', '/mcp/discovery', {'profile': profile, 'project_path': str(source)})
    candidate = next(s for s in preview['servers'] if s['name'] == 'http-fixture')
    assert sorted(candidate['overrides']) == sorted(overrides), 'wrong override provenance'
    assert not events, 'preview contacted endpoint'
    request(True, 'POST', '/mcp/trust', {'action': 'approve', 'review_id': preview['review_id']}, 204)
    approved = True
    assert not events, 'approval contacted endpoint'
    session = request(False, 'POST', '/session', {'purpose': 'HTTP qualification',
        'mcp_profile': profile, 'recipes': [], 'agent': {'name': 'display', 'instructions': ''}, 'skills': []}, 201)
    sid = session['id']
    endpoint = f'/session/{sid}/mcp'
    def catalog():
        result = request(False, 'GET', endpoint)['servers'][0]
        assert result['healthy']
        assert [t['name'] for t in result['tools']] == ['ping', 'redirect']
    catalog()
    wait_for(lambda: ('subscription-unsupported', '1') in events, 'GET negotiation not observed')
    request(True, 'GET', '/mcp/status')
    before = list(events)
    request(False, 'POST', endpoint + '/call', {'server': 'http-fixture', 'tool': 'ping', 'decision': 'deny', 'arguments': {}})
    assert not any(e[0].startswith('call-') for e in events), 'denied call reached endpoint'
    request(False, 'POST', endpoint + '/refresh', {'server': 'http-fixture', 'action': 'refresh'}, 204)
    catalog()
    assert initialized == 1, 'manual refresh initialized a replacement'
    assert events.count(('list-first', '1')) > before.count(('list-first', '1'))
    request(False, 'POST', endpoint + '/call', {'server': 'http-fixture', 'tool': 'redirect', 'decision': 'approve', 'arguments': {}}, 502)
    failed = request(False, 'GET', endpoint)['servers'][0]
    assert not failed['healthy'] and failed['tools'] == [], 'redirect retained stale catalog'
    request(False, 'POST', endpoint + '/refresh', {'server': 'http-fixture', 'action': 'reconnect'}, 204)
    catalog()
    wait_for(lambda: ('delete', '1') in events and ('subscription-unsupported', '2') in events, 'replaced session not cleaned up')
    assert initialized == 2
    assert sum(e[0] == 'call-redirect' for e in events) == 1, 'redirected call replayed'
    request(False, 'POST', endpoint + '/call', {'server': 'http-fixture', 'tool': 'ping', 'decision': 'approve', 'arguments': {}})
    assert events.count(('call-ping', '2')) == 1
    request(False, 'DELETE', f'/session/{sid}', expected=204)
    sid = None
    wait_for(lambda: ('delete', '2') in events, 'closed session not cleaned up')
    assert events.count(('delete', '1')) == events.count(('delete', '2')) == 1
    assert not failures
    print('PASS http trust-no-io url-header-overrides private-values-redacted reserved-headers-replaced negotiated-version pagination optional-get-405 manual-refresh', flush=True)
    print('PASS http-lifecycle denied-no-io redirect-not-followed stale-tools-cleared explicit-reconnect no-call-replay negotiated-delete-405-404', flush=True)
finally:
    try:
        if sid is not None:
            request(False, 'DELETE', f'/session/{sid}', expected=204)
        if approved:
            request(True, 'POST', '/mcp/trust', {'action': 'revoke', 'profile': profile}, 204)
    finally:
        server.shutdown()
        server.server_close()
        worker.join(timeout=3)
        assert not worker.is_alive() and server.socket.fileno() == -1, 'HTTP listener did not close'
        for target, contents in backups.items():
            if contents is None:
                target.unlink(missing_ok=True)
            else:
                target.write_bytes(contents)
