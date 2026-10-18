#!/usr/bin/env python3
"""Fast companion-socket client for the display harness.

Python startup is too slow to pause a drag between pointer batches. This
process stays up. Each line is one action (start, pause, resume, stop) and
opens its own HTTP/1.1 connection to the visor socket.
"""

import socket
import sys

ACTIONS = {"start", "pause", "resume", "stop"}


def post(path, action):
    body = '{"action":"%s"}' % action
    request = (
        "POST /companion/automation HTTP/1.1\r\n"
        "host: hypermesh\r\n"
        "content-type: application/json\r\n"
        "content-length: %d\r\n"
        "connection: close\r\n\r\n%s" % (len(body), body)
    )
    stream = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    stream.settimeout(2)
    try:
        stream.connect(path)
        stream.sendall(request.encode())
        data = b""
        while True:
            chunk = stream.recv(4096)
            if not chunk:
                break
            data += chunk
    except OSError as err:
        return 0, str(err)
    finally:
        stream.close()
    text = data.decode(errors="replace")
    line = text.split("\r\n", 1)[0]
    code = 0
    parts = line.split()
    if len(parts) >= 2 and parts[1].isdigit():
        code = int(parts[1])
    detail = text.replace("\r", " ").replace("\n", " ")
    return code, detail


def main():
    path = sys.argv[1]
    server = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    server.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    server.bind(("127.0.0.1", 0))
    server.listen(1)
    print("ready port=%d" % server.getsockname()[1], flush=True)
    conn, _addr = server.accept()
    conn_file = conn.makefile("rwb", buffering=0)
    try:
        while True:
            raw = conn_file.readline()
            if not raw:
                break
            action = raw.decode(errors="replace").strip()
            if action == "quit":
                conn_file.write(b"ok\n")
                break
            if action not in ACTIONS:
                conn_file.write(b"err unknown\n")
                continue
            code, detail = post(path, action)
            if code == 204:
                conn_file.write(b"ok 204\n")
            else:
                conn_file.write(("err %s %s\n" % (code, detail)).encode())
    finally:
        conn.close()
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except OSError as err:
        print("socket helper: %s" % err, file=sys.stderr)
        sys.exit(1)
