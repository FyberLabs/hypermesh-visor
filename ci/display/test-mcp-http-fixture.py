#!/usr/bin/env python3
"""The real SSE fixture must not mistake DELETE or teardown for client closure."""
import http.client
import http.server
import importlib.util
from pathlib import Path
import threading

spec = importlib.util.spec_from_file_location('http_fixture', Path(__file__).with_name('mcp-http.py'))
fixture = importlib.util.module_from_spec(spec)
spec.loader.exec_module(fixture)
server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), fixture.Handler)
worker = threading.Thread(target=server.serve_forever)
worker.start()
headers = {'Authorization': 'Bearer ' + fixture.private, 'X-Fixture': 'base-retained',
           'Mcp-Session-Id': '3', 'MCP-Protocol-Version': fixture.protocol,
           'Last-Event-ID': 'fixture-cursor'}
connections = []

def stream():
    client = http.client.HTTPConnection('127.0.0.1', server.server_port, timeout=3)
    connections.append(client)
    client.request('GET', fixture.path, headers=headers)
    response = client.getresponse()
    assert response.status == 200
    fixture.wait_for(lambda: ('sse-resumed', '3') in fixture.events, 'GET not observed')
    return client, response

try:
    client, response = stream()
    delete = http.client.HTTPConnection('127.0.0.1', server.server_port, timeout=3)
    connections.append(delete)
    delete.request('DELETE', fixture.path, headers=headers)
    deleted = delete.getresponse()
    assert deleted.status == 404
    deleted.read()
    fixture.wait_for(lambda: ('delete', '3') in fixture.events, 'DELETE not observed')
    with fixture.condition:
        assert not fixture.condition.wait_for(
            lambda: any(e[0] in ('sse-ended', 'sse-client-closed-resumed') for e in fixture.events),
            timeout=0.3), 'DELETE fabricated stream closure while client remained connected'
    response.close()
    client.close()
    fixture.wait_for(lambda: ('sse-client-closed-resumed', '3') in fixture.events,
                     'actual client disconnect not observed')
    fixture.wait_for(lambda: ('sse-ended', '3') in fixture.events, 'handler did not finish')
    fixture.events.clear()
    client, response = stream()
    fixture.teardown.set()
    fixture.wait_for(lambda: ('sse-ended', '3') in fixture.events, 'teardown did not stop handler')
    assert ('sse-client-closed-resumed', '3') not in fixture.events, 'teardown fabricated client closure'
    response.close()
    assert not fixture.failures
    print('PASS DELETE leaves SSE open; client EOF observed independently; teardown is not client EOF')
finally:
    fixture.teardown.set()
    for connection in connections:
        connection.close()
    server.shutdown()
    server.server_close()
    worker.join(timeout=3)
    assert not worker.is_alive()
