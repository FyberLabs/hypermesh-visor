#!/usr/bin/env python3
"""Fast companion-socket client for the display harness.

Python startup is too slow to pause a drag between pointer batches. This
process stays up. The harness is the only writer on stdin. There is no
listener. Each line is one action (start, pause, resume, stop) and opens
its own HTTP/1.1 connection on the companion Unix socket.

A closed stdin is the same as quit.
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
    print("ready", flush=True)
    for raw in sys.stdin:
        action = raw.strip()
        if not action:
            continue
        if action == "quit":
            print("ok", flush=True)
            return 0
        if action not in ACTIONS:
            print("err unknown", flush=True)
            continue
        code, detail = post(path, action)
        if code == 204:
            print("ok 204", flush=True)
        else:
            print("err %s %s" % (code, detail), flush=True)
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except OSError as err:
        print("socket helper: %s" % err, file=sys.stderr)
        sys.exit(1)
