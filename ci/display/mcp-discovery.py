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
import shutil
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
    from Xlib import Xatom, display
    control = runtime / "control.sock"
    companion = runtime / "companion.sock"
    sid = None
    approved = False
    profile = "display-discovery"
    profiles = config / "mcp-profiles.json"
    bindings = config / "mcp-bindings.json"
    backups = {path: path.read_bytes() if path.exists() else None for path in (profiles, bindings)}
    windows = []

    def stop_window(process):
        if process.poll() is None:
            process.terminate()
            try:
                process.wait(timeout=3)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait(timeout=3)

    def focus(window, pid):
        for name, kind, value in [("_NET_WM_PID", "32c", str(pid)),
                                  ("WM_CLIENT_MACHINE", "8s", socket.gethostname())]:
            subprocess.run(["xprop", "-id", window, "-f", name, kind, "-set", name, value], check=True)
        x = display.Display()
        try:
            x.screen().root.change_property(x.intern_atom("_NET_ACTIVE_WINDOW"), Xatom.WINDOW, 32, [int(window)])
            x.sync()
        finally:
            x.close()
        subprocess.run(["xdotool", "windowfocus", "--sync", window], check=True)

    def start_window(directory, number):
        ready = directory / f"window-{number}"
        process = subprocess.Popen([sys.executable, str(Path(__file__).resolve()), "--window", str(ready)])
        windows.append(process)
        until(lambda: ready.exists() and ready.read_text().strip(), "fixture window not ready")
        assert process.poll() is None
        window = ready.read_text().strip()
        focus(window, process.pid)
        return process, window

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
            if predicate():
                return
            time.sleep(0.05)
        current = status() or {}
        diagnostic = {"session_matches": current.get("id") == sid,
                      "prefer_mcp": current.get("prefer_mcp"),
                      "reason": current.get("mcp_match", {}).get("reason"),
                      "has_executable": bool((current.get("focused") or {}).get("executable"))}
        raise AssertionError(f"{description}: {diagnostic}")

    def status():
        return request(companion, "GET", "/mcp/status")["session"]

    try:
        with tempfile.TemporaryDirectory(prefix="discovery-", dir=scratch) as directory:
            directory = Path(directory)
            marker = directory / "server-events"
            source = directory / "selected.json"
            secondary_marker = directory / "secondary-events"
            def definition(events):
                return {"command": sys.executable,
                    "args": [str(Path(__file__).resolve()), "--server", str(events)],
                    "env": {"PRIVATE_TEST_VALUE": "MCP_DISCOVERY_PRIVATE_SENTINEL"}}
            source.write_text(json.dumps({"mcpServers": {
                "fixture": definition(marker), "fixture_second": definition(secondary_marker)}}))
            # Isolated harness configuration only; restore it in finally.
            profiles.write_text(json.dumps({"active": "default", "profiles": {
                "default": {"servers": []}, profile: {"servers": ["fixture", "fixture_second"]}}}))
            bindings.write_text(json.dumps({"bindings": [
                {"server": "fixture", "executable": str(Path(sys.executable).resolve())}]}))
            app, window = start_window(directory, 1)
            preview = request(companion, "POST", "/mcp/discovery", {"profile": profile, "project_path": str(source)})
            assert not marker.exists() and not secondary_marker.exists(), "preview started a server"
            assert "MCP_DISCOVERY_PRIVATE_SENTINEL" not in json.dumps(preview)
            request(companion, "POST", "/mcp/trust", {"action": "approve", "review_id": preview["review_id"]}, 204)
            approved = True
            assert not marker.exists() and not secondary_marker.exists(), "approval started a server"
            session_request = {"purpose": "live MCP discovery", "mcp_profile": profile,
                "recipes": [], "agent": {"name": "display", "instructions": ""}, "skills": []}
            session = request(control, "POST", "/session", session_request, 201)
            sid = session["id"]
            endpoint = f"/session/{sid}/mcp"
            catalog = request(control, "GET", endpoint)
            assert [tool["name"] for tool in catalog["servers"][0]["tools"]] == ["ping", "crash"]
            until(lambda: status()["prefer_mcp"] is True, "real executable identity did not select MCP")
            matched = status()
            assert matched["mcp_match"]["reason"] == "matched"
            assert matched["focused"]["executable"] == str(Path(sys.executable).resolve())
            assert "MCP_DISCOVERY_PRIVATE_SENTINEL" not in json.dumps(matched)
            # Same title, different executable: title alone must not retain MCP routing.
            xmessage = shutil.which("xmessage")
            assert xmessage is not None
            other = subprocess.Popen([xmessage, "-title", "Visor MCP discovery acceptance", "Unrelated app"])
            windows.append(other)
            other_window = []
            def find_other():
                result = subprocess.run(["xdotool", "search", "--onlyvisible", "--name",
                    "^Visor MCP discovery acceptance$"], capture_output=True, text=True)
                other_window[:] = [w for w in result.stdout.splitlines() if w != window]
                return bool(other_window)
            until(find_other, "same-title app did not map")
            focus(other_window[0], other.pid)
            until(lambda: (status().get("focused") or {}).get("executable") == str(Path(xmessage).resolve()),
                  "unrelated app executable not observed")
            assert status()["prefer_mcp"] is False, "same title matched another executable"
            assert request(control, "GET", endpoint)["servers"][0]["healthy"] is True
            stop_window(other)
            focus(window, app.pid)
            until(lambda: status()["prefer_mcp"] is True, "bound app did not regain routing")
            old_pid = app.pid
            stop_window(app)
            # Leave the destroyed window hint in place to exercise stale-focus clearing.
            until(lambda: status()["prefer_mcp"] is False, "destroyed app retained routing")
            app, window = start_window(directory, 2)
            assert app.pid != old_pid
            until(lambda: status()["prefer_mcp"] is True, "restarted app did not regain routing")
            assert marker.read_text().count("start ") == 1, "focus transitions restarted MCP"
            print("PASS identity same-title-rejected app-exit-clears-routing app-restart-recovers healthy-attachment-retained", flush=True)
            # Qualify ordering and all-selector behavior through the real daemon.
            executable = str(Path(sys.executable).resolve())
            def rule(server):
                return {"server": server, "executable": executable}
            def set_rules(rules, winner, index, identities, healthy):
                bindings.write_text(json.dumps({"bindings": rules}))
                def selected():
                    current = status()
                    match = current["mcp_match"]
                    return (current.get("mcp") == winner and current["prefer_mcp"] is True
                            and match.get("binding_index") == index
                            and match["identity_matches"] == identities
                            and match["healthy_matches"] == healthy)
                until(selected, "binding precedence or selector result did not update")
            set_rules([rule("fixture_second"), rule("fixture")], "fixture_second", 0, 2, 2)
            set_rules([rule("fixture"), rule("fixture_second")], "fixture", 0, 2, 2)
            set_rules([rule("unavailable"), rule("fixture_second"), rule("fixture")],
                      "fixture_second", 1, 3, 2)
            wm_class = status()["focused"]["wm_class"]
            assert wm_class
            # A matching class cannot compensate for a different executable.
            set_rules([{"server": "fixture_second", "wm_class": wm_class,
                        "executable": str(Path(xmessage).resolve())}, rule("fixture")],
                      "fixture", 1, 1, 1)
            set_rules([rule("fixture")], "fixture", 0, 1, 1)
            assert marker.read_text().count("start ") == 1
            assert secondary_marker.read_text().count("start ") == 1
            assert "call " not in secondary_marker.read_text()
            print("PASS bindings ordered-healthy-winner unavailable-skipped all-selectors-required no-restart-no-call", flush=True)
            # Read-only catalog refresh must retain routing and not restart the process.
            request(control, "POST", endpoint + "/refresh", {"server": "fixture", "action": "refresh"}, 204)
            until(lambda: status()["prefer_mcp"] is True, "catalog refresh did not restore matching")
            assert [tool["name"] for tool in request(control, "GET", endpoint)["servers"][0]["tools"]] == ["ping", "crash"]
            assert marker.read_text().count("start ") == 1
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
            bindings.write_text(json.dumps({"bindings": [
                {"server": "fixture", "executable": str(Path(sys.executable).resolve())}]}))
            until(lambda: status()["prefer_mcp"] is True, "corrected binding did not recover matching")
            # Revocation blocks future sessions, while the existing attachment
            # retains the explicitly documented session snapshot.
            request(companion, "POST", "/mcp/trust", {"action": "revoke", "profile": profile}, 204)
            approved = False
            events = marker.read_text()
            request(control, "POST", "/session", session_request, 403)
            assert marker.read_text() == events, "revoked profile started a process"
            request(control, "POST", endpoint + "/call", {"server": "fixture", "tool": "ping", "decision": "approve", "arguments": {}})
            assert marker.read_text().count("call ping") == 1
            assert marker.read_text().count("call crash") == 1
            request(control, "DELETE", f"/session/{sid}", expected=204)
            sid = None
            print("PASS discovery trust pagination executable-match refresh disconnect reconnect no-replay binding-fallback recovery revocation", flush=True)
    finally:
        try:
            if sid is not None:
                request(control, "DELETE", f"/session/{sid}", expected=204)
            if approved:
                request(companion, "POST", "/mcp/trust", {"action": "revoke", "profile": profile}, 204)
        finally:
            for process in reversed(windows):
                stop_window(process)
            subprocess.run(["xprop", "-root", "-remove", "_NET_ACTIVE_WINDOW"], check=False)
            for path, contents in backups.items():
                if contents is None:
                    path.unlink(missing_ok=True)
                else:
                    path.write_bytes(contents)



if __name__ == "__main__":
    if sys.argv[1] == "--server":
        server(sys.argv[2])
    elif sys.argv[1] == "--window":
        import tkinter as tk
        root = tk.Tk(className="VisorMcpDiscovery")
        root.title("Visor MCP discovery acceptance")
        root.geometry("300x100+100+100")
        tk.Label(root, text="Local MCP discovery fixture").pack()
        root.update()
        window = subprocess.check_output(["xdotool", "search", "--onlyvisible", "--name",
            "^Visor MCP discovery acceptance$"], text=True).splitlines()[0]
        Path(sys.argv[2]).write_text(window)
        root.mainloop()
    else:
        main(*(Path(arg) for arg in sys.argv[1:4]))
