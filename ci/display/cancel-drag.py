"""Pause a real drag after observing Button1 down, without shell startup latency."""
import json
import re
import socket
import sys
import threading
import time
from Xlib import display, X

control, companion, session, to_x, to_y = sys.argv[1:]
x = display.Display()
root = x.screen().root


def connect(path):
    stream = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    stream.settimeout(5)
    stream.connect(path)
    return stream


def request(stream, path, body):
    data = json.dumps(body).encode()
    stream.sendall((f"POST {path} HTTP/1.1\r\nHost: hypermesh\r\n"
                    f"Content-Type: application/json\r\nContent-Length: {len(data)}\r\n"
                    "Connection: close\r\n\r\n").encode() + data)
    response = b""
    while chunk := stream.recv(4096):
        response += chunk
    stream.close()
    return response


# Both HTTP connections and the X connection are ready before injection starts.
with connect(control) as drive, connect(companion) as pause:
    assert not root.query_pointer().mask & X.Button1Mask, "button already held"
    result = []
    errors = []

    def drag():
        try:
            result.append(request(drive, f"/session/{session}/mouse", {
                "action": "drag", "x": 0, "y": 0,
                "to_x": int(to_x), "to_y": int(to_y), "button": "left"}))
        except Exception as error:
            errors.append(error)

    worker = threading.Thread(target=drag)
    worker.start()
    deadline = time.monotonic() + 5
    observed = False
    try:
        while worker.is_alive() and time.monotonic() < deadline:
            if root.query_pointer().mask & X.Button1Mask:
                observed = True
                reply = request(pause, "/companion/automation", {"action": "pause"})
                assert reply.startswith(b"HTTP/1.1 204"), reply
                break
    finally:
        worker.join(timeout=6)
    assert not worker.is_alive(), "drag request did not finish"
    assert not errors, errors
    assert observed, "drag completed before Button1 down was observed"
    response = result[0]
    print(response.decode(errors="replace"), flush=True)
    assert response.startswith(b"HTTP/1.1 409"), response
    sent = re.search(rb"after (\d+) pointer steps", response)
    total = (max(int(to_x), int(to_y)) + 63) // 64 + 3
    assert sent and 2 <= int(sent[1]) < total, "not cancelled while button held"
    assert not root.query_pointer().mask & X.Button1Mask, "cancel left Button1 down"
x.close()
