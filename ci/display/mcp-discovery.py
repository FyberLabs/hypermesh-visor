#!/usr/bin/env python3
"""Live X11 discovery -> trust -> matching -> disconnect/reconnect acceptance.

A local Tk app and stdio MCP fixture run entirely inside the harness. Nothing
opens a TCP listener, reads user configuration, or downloads/executes packages.
"""
import http.client
import json
import os
from pathlib import Path
import socket
import subprocess
import sys
import tempfile
import time


def server(marker):
    with open(marker, "a") as audit:
        audit.write(f"start {os.getpid()}\n")
        audit.flush()
        for line in sys.stdin:
            request = json.loads(line)
            method = request.get("method")
            if "id" not in request:
                continue
            if method == "initialize":
                result = {"protocolVersion": "2025-11-25", "capabilities": {"tools": {}},
                          "serverInfo": {"name": "discovery-fixture", "version": "1"}}
            elif method == "tools/list":
                if "cursor" in request.get("params", {}):
                    assert request["params"]["cursor"] == "second page"
                    result = {"tools": [{"name": "crash", "inputSchema": {"type": "object"}}]}
                else:
                    result = {"tools": [{"name": "ping", "inputSchema": {"type": "object"}}],
                              "nextCursor": "second page"}
            elif method == "tools/call":
                name = request["params"]["name"]
                audit.write(f"call {name}\n")
                audit.flush()
                if name == "crash":
                    return  # lose the reply, without replaying this side effect
                result = {"content": [{"type": "text", "text": "pong"}]}
            else:
                raise AssertionError("unexpected MCP request")
            print(json.dumps({"jsonrpc": "2.0", "id": request["id"], "result": result}), flush=True)


class UnixHTTP(http.client.HTTPConnection):
    def __init__(self, path):
        super().__init__("visor", timeout=20)
        self.path = str(path)

    def connect(self):
        self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.sock.settimeout(self.timeout)
        self.sock.connect(self.path)


def main(runtime, config, scratch):
    import tkinter as tk
    control = runtime / "control.sock"
    companion = runtime / "companion.sock"
    sid = None
    approved = False
    profile = "display-discovery"
    profiles = config / "mcp-profiles.json"
    bindings = config / "mcp-bindings.json"
    backups = {path: path.read_bytes() if path.exists() else None for path in (profiles, bindings)}
    root = tk.Tk(className="VisorMcpDiscovery")
    root.title("Visor MCP discovery acceptance")
    root.geometry("300x100+100+100")
    tk.Label(root, text="Local MCP discovery fixture").pack()

    def request(path, method, endpoint, payload=None, expected=200):
        connection = UnixHTTP(path)
        try:
            connection.request(method, endpoint,
                               None if payload is None else json.dumps(payload),
                               {"Content-Type": "application/json"})
            response = connection.getresponse()
            data = response.read()
            assert response.status == expected, f"{method} {endpoint}: status {response.status}, expected {expected}"
            return json.loads(data) if data else None
        finally:
            connection.close()

    def until(predicate, description):
        deadline = time.monotonic() + 8
        while time.monotonic() < deadline:
            root.update()
            if predicate():
                return
            time.sleep(0.05)
        raise AssertionError(description)

    def status():
        return request(companion, "GET", "/mcp/status")["session"]

    try:
        with tempfile.TemporaryDirectory(prefix="discovery-", dir=scratch) as directory:
            directory = Path(directory)
            marker = directory / "server-events"
            source = directory / "selected.json"
            source.write_text(json.dumps({"mcpServers": {"fixture": {
                "command": sys.executable, "args": [str(Path(__file__).resolve()), "--server", str(marker)],
                "env": {"PRIVATE_TEST_VALUE": "MCP_DISCOVERY_PRIVATE_SENTINEL"}}}}))
            # Isolated harness configuration only; restore it in finally.
            profiles.write_text(json.dumps({"active": "default", "profiles": {
                "default": {"servers": []}, profile: {"servers": ["fixture"]}}}))
            bindings.write_text(json.dumps({"bindings": [
                {"server": "fixture", "executable": str(Path(sys.executable).resolve())}]}))
            root.update()
            window = subprocess.check_output(["xdotool", "search", "--name", "^Visor MCP discovery acceptance$"], text=True).splitlines()[0]
            for name, kind, value in [("_NET_WM_PID", "32c", str(os.getpid())),
                                      ("WM_CLIENT_MACHINE", "8s", socket.gethostname())]:
                subprocess.run(["xprop", "-id", window, "-f", name, kind, "-set", name, value], check=True)
            subprocess.run(["xprop", "-root", "-f", "_NET_ACTIVE_WINDOW", "32x", "-set", "_NET_ACTIVE_WINDOW", window], check=True)
            subprocess.run(["xdotool", "windowfocus", "--sync", window], check=True)
            preview = request(companion, "POST", "/mcp/discovery", {"profile": profile, "project_path": str(source)})
            assert not marker.exists(), "preview started a server"
            assert "MCP_DISCOVERY_PRIVATE_SENTINEL" not in json.dumps(preview)
            request(companion, "POST", "/mcp/trust", {"action": "approve", "review_id": preview["review_id"]}, 204)
            approved = True
            assert not marker.exists(), "approval started a server"
            session = request(control, "POST", "/session", {"purpose": "live MCP discovery", "mcp_profile": profile,
                "recipes": [], "agent": {"name": "display", "instructions": ""}, "skills": []}, 201)
            sid = session["id"]
            endpoint = f"/session/{sid}/mcp"
            catalog = request(control, "GET", endpoint)
            assert [tool["name"] for tool in catalog["servers"][0]["tools"]] == ["ping", "crash"]
            until(lambda: status()["prefer_mcp"] is True, "real executable identity did not select MCP")
            matched = status()
            assert matched["mcp_match"]["reason"] == "matched"
            assert matched["focused"]["executable"] == str(Path(sys.executable).resolve())
            assert "MCP_DISCOVERY_PRIVATE_SENTINEL" not in json.dumps(matched)
            first_start = marker.read_text()
            request(control, "POST", endpoint + "/call", {"server": "fixture", "tool": "ping", "decision": "deny", "arguments": {}})
            assert marker.read_text() == first_start, "denied call reached server"
            request(control, "POST", endpoint + "/call", {"server": "fixture", "tool": "crash", "decision": "approve", "arguments": {}}, 502)
            until(lambda: status()["prefer_mcp"] is False, "dead attachment retained MCP preference")
            assert request(control, "GET", endpoint)["servers"][0]["tools"] == []
            assert marker.read_text().count("call crash") == 1
            request(control, "POST", endpoint + "/refresh", {"server": "fixture", "action": "reconnect"}, 204)
            until(lambda: status()["prefer_mcp"] is True, "reconnect did not restore matching")
            assert marker.read_text().count("start ") == 2
            assert marker.read_text().count("call crash") == 1, "lost call was replayed"
            # A changed rule must invalidate a healthy match without detaching.
            bindings.write_text('{"bindings":[{"server":"fixture","executable_path":"private"}]}')
            until(lambda: status()["mcp_match"]["reason"] == "bindings_unavailable", "bad binding retained a match")
            assert request(control, "GET", endpoint)["servers"][0]["healthy"] is True
            print("PASS discovery trust pagination executable-match disconnect reconnect no-replay binding-fallback", flush=True)
    finally:
        try:
            if sid is not None:
                request(control, "DELETE", f"/session/{sid}", expected=204)
            if approved:
                request(companion, "POST", "/mcp/trust", {"action": "revoke", "profile": profile}, 204)
        finally:
            root.destroy()
            subprocess.run(["xprop", "-root", "-remove", "_NET_ACTIVE_WINDOW"], check=False)
            for path, contents in backups.items():
                if contents is None:
                    path.unlink(missing_ok=True)
                else:
                    path.write_bytes(contents)



if __name__ == "__main__":
    if sys.argv[1] == "--server":
        server(sys.argv[2])
    else:
        main(*(Path(arg) for arg in sys.argv[1:4]))
