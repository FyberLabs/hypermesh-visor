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

mode = sys.argv[2] if len(sys.argv) > 2 else "profiles"
title = {"profiles": "MCP profiles", "servers": "MCP servers", "trust": "MCP trust"}[mode]
option = {"profiles": "--mcp-profiles", "servers": "--mcp-servers", "trust": "--mcp-trust-gui"}[mode]

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
                payload = {
                    "revision": "fixture",
                    "active": "default",
                    "profiles": [{
                        "name": "default",
                        "servers": ["editor"],
                        "gateway": False,
                        "has_overrides": True,
                    }],
                }
                if mode == "servers":
                    payload = {"revision": "fixture", "servers": [{"name": "editor", "transport": "stdio", "enabled": True}]}
                elif mode == "trust":
                    payload = []
                body = json.dumps(payload).encode()
                try:
                    conn.sendall(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: " +
                                 str(len(body)).encode() + b"\r\nConnection: close\r\n\r\n" + body)
                except OSError as exc:
                    errors.append(str(exc))

    server = threading.Thread(target=serve)
    server.start()
    env = dict(os.environ, HYPERMESH_RUNTIME_DIR=runtime, GDK_BACKEND="x11", GSK_RENDERER="cairo")
    process = subprocess.Popen([sys.argv[1], option], env=env)
    try:
        deadline = time.monotonic() + 15
        window = None
        while time.monotonic() < deadline:
            found = subprocess.run(
                ["xdotool", "search", "--onlyvisible", "--name", f"^{title}$"],
                capture_output=True, text=True)
            if found.returncode == 0 and found.stdout.strip():
                window = found.stdout.splitlines()[0]
                break
            if process.poll() is not None:
                raise RuntimeError("profile editor exited before rendering")
            time.sleep(0.1)
        if not window:
            print("requests:", calls, flush=True)
            subprocess.run(["xwininfo", "-root", "-tree"], check=False)
            subprocess.run(["ps", "-o", "pid,ppid,args", "-C", "zenity"], check=False)
        assert window, "graphical profile chooser did not render"
        subprocess.run(["xdotool", "windowfocus", "--sync", window], check=True)
        subprocess.run(["xdotool", "key", "Escape"], check=True)
        assert process.wait(timeout=5) == 0, "cancel did not exit cleanly"
        assert calls == [f"GET /mcp/{mode} HTTP/1.1".encode()], calls
        assert not errors, errors
        print(f"{title} GTK chooser rendered; cancel submitted no writes")
    finally:
        if process.poll() is None:
            process.terminate()
            process.wait(timeout=5)
        stop.set()
        server.join(timeout=3)
        listener.close()
