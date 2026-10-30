#!/usr/bin/env python3
"""Live GTK profile chooser: render, cancel, and prove no mutation was sent."""
import json
import os
import socket
import subprocess
import sys
import tempfile
import threading
import time

with tempfile.TemporaryDirectory(prefix="visor-profile-gui-") as runtime:
    listener = socket.socket(socket.AF_UNIX)
    listener.bind(runtime + "/companion.sock")
    listener.listen()
    listener.settimeout(0.2)
    stop = threading.Event()
    calls = []
    errors = []

    def serve():
        while not stop.is_set():
            try:
                conn, _ = listener.accept()
            except socket.timeout:
                continue
            with conn:
                conn.settimeout(2)
                data = b""
                while b"\r\n\r\n" not in data:
                    part = conn.recv(4096)
                    if not part:
                        break
                    data += part
                calls.append(data.split(b"\r\n", 1)[0])
                body = json.dumps({
                    "revision": "fixture",
                    "active": "default",
                    "profiles": [{
                        "name": "default",
                        "servers": ["editor"],
                        "gateway": False,
                        "has_overrides": True,
                    }],
                }).encode()
                try:
                    conn.sendall(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: " +
                                 str(len(body)).encode() + b"\r\nConnection: close\r\n\r\n" + body)
                except OSError as exc:
                    errors.append(str(exc))

    server = threading.Thread(target=serve)
    server.start()
    env = dict(os.environ, HYPERMESH_RUNTIME_DIR=runtime, GDK_BACKEND="x11")
    process = subprocess.Popen([sys.argv[1], "--mcp-profiles"], env=env)
    try:
        deadline = time.monotonic() + 15
        window = None
        while time.monotonic() < deadline:
            found = subprocess.run(
                ["xdotool", "search", "--onlyvisible", "--name", "^MCP profiles$"],
                capture_output=True, text=True)
            if found.returncode == 0 and found.stdout.strip():
                window = found.stdout.splitlines()[0]
                break
            if process.poll() is not None:
                raise RuntimeError("profile editor exited before rendering")
            time.sleep(0.1)
        assert window, "graphical profile chooser did not render"
        subprocess.run(["xdotool", "windowfocus", "--sync", window], check=True)
        subprocess.run(["xdotool", "key", "Escape"], check=True)
        assert process.wait(timeout=5) == 0, "cancel did not exit cleanly"
        assert calls == [b"GET /mcp/profiles HTTP/1.1"], calls
        assert not errors, errors
        print("MCP profile GTK chooser rendered; cancel submitted no writes")
    finally:
        if process.poll() is None:
            process.terminate()
            process.wait(timeout=5)
        stop.set()
        server.join(timeout=3)
        listener.close()
