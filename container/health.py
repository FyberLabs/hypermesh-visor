#!/usr/bin/env python3
"""Readiness requires the private control API and supervised components."""
import http.client
import json
import os
import socket

class Connection(http.client.HTTPConnection):
    def __init__(self):
        super().__init__('visor', timeout=2)
    def connect(self):
        self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.sock.settimeout(2)
        self.sock.connect('/run/visor/hypermesh/control.sock')

try:
    connection = Connection()
    connection.request('GET', '/capabilities')
    response = connection.getresponse()
    data = json.loads(response.read())
    connection.close()
    if response.status != 200:
        raise RuntimeError('API unavailable')
    # The supervisor writes readiness only after all essential processes start.
    if not os.path.isfile('/run/visor/ready'):
        raise RuntimeError('startup incomplete')
except Exception:
    raise SystemExit(1)
