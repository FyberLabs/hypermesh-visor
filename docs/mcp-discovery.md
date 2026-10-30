# MCP configuration and trust

Each session attaches its requested `mcp_profile`, or the active user profile
when none is specified. Missing configuration yields no attached servers.
Treat MCP configuration as executable configuration: a stdio entry starts a
process with the configured arguments/environment, and an HTTP entry connects
to its endpoint with configured headers. Review the complete file before use.

## Configuration sources and precedence

1. `HYPERMESH_CONFIG_DIR` selects the user configuration directory, followed by
   `$XDG_CONFIG_HOME/hypermesh`, `$HOME/.config/hypermesh`, then `.config/hypermesh`.
2. User `mcp.json` defines servers; `mcp-profiles.json` selects profile membership,
   the active profile, and per-profile overrides.
3. An optional `HYPERMESH_MCP_PROJECT` imports additional server definitions.
   It must be an absolute file path or an absolute directory containing
   `mcp.json`. Unset means no project import. Empty, relative, missing, unreadable,
   or malformed selections fail attachment rather than falling back to discovery.
4. User server definitions win duplicate IDs, including disabled entries. A
   project server does not attach unless the selected user profile names it.
   Profile overrides apply after server selection; disabled servers stay omitted.
5. A `gateway: true` profile attaches only `docker-gateway`, using the configured
   definition or the existing `docker mcp gateway run` default.

The daemon never walks the working directory or its parents for `.hypermesh`
or `.cursor` configuration. Merely opening or cloning a project is not consent
to execute its MCP commands. For a reviewed project configuration, launch visor
with an explicit selection, for example:

```sh
HYPERMESH_MCP_PROJECT=/absolute/reviewed/project/.cursor/mcp.json hypermesh-visor
```

This is a launch-time selection, not a content-pinned approval: changes to the
selected file can affect later session attachments. Keep it in a directory you
control; copy reviewed definitions into user configuration when the original
project is writable by others. Do not put credentials in issue reports or logs.
Vault MCP retrieval remains separate and uses named servers from user `mcp.json`;
it does not import project config or require session profile membership.

## Current matching and transport inventory

- Stdio and HTTP/SSE transports initialize and load tools on attachment. Closing
  a session terminates its stdio children and drops HTTP clients.
- `GET /session/{id}/mcp` reports attached servers, health, and tools.
- `mcp-bindings.json` matches focused identity fields, not window titles, against
  attached healthy server IDs. The first matching healthy binding wins; all
  populated selectors must match. Class/app-ID comparisons retain case-insensitive compatibility. Executable
  paths match exactly, including case; use the resolved absolute path.
- X11 uses `WM_CLASS`; Wayland probes Hyprland, Sway, and KWin on a best-effort
  basis. Hyprland and Sway resolve the reported positive PID through `/proc/<pid>/exe`
  for the same user; missing, inaccessible, deleted, or foreign-user processes
  contribute no executable. Titles and command lines are never executable
  identities. These are routing hints, not authenticated application identities.
- Bindings are cached with a one-second metadata refresh; focus has its own
  short cache. Expired focus is withheld while refreshing, so a stalled probe
  cannot preserve a stale match. Failure to find a healthy match retains desktop mode. Input
  authorization and physical-user pause still apply.

Issue #29 remains open for a reviewable discovery/provenance UI, native stable
identity coverage beyond the current Linux probes, capability negotiation, background change subscriptions, automatic reconnect
policy, and native Windows/macOS discovery. This change establishes
explicit project trust without claiming those discovery features are complete.

## Match diagnostics

`GET /companion` includes `session.mcp_match` alongside the existing `mcp`,
`mode`, `prefer_mcp`, and `focused` fields. It does not attach or start servers.

- `reason`: `focus_unavailable`, `no_bindings`, `identity_mismatch`,
  `server_unavailable`, or `matched`.
- `binding_index`: zero-based index in `mcp-bindings.json` of the winning binding,
  present only for a match. Reorder bindings to set explicit precedence.
- `identity_matches`: number of bindings matching every populated selector.
- `healthy_matches`: number of those bindings naming an attached healthy server.
  More than one exposes overlapping rules; the first healthy binding wins.

`server_unavailable` means the identity matched but none of its servers is
attached and healthy. `no_bindings` means no usable bindings were loaded; missing
or malformed files currently both yield an empty set. `focus_unavailable` also
covers an expired sample while its refresh is pending. Counters describe the
current attachment health snapshot, not a new network probe, permanent app
capability, or authorization. HTTP health reflects successful attachment/refresh until an observed transport
failure; it is not a background liveness probe. Explicit reconnect is available
below. Diagnostics
contain no raw config, environment, headers, command arguments, or titles.

Compositor field references: [Sway IPC](https://github.com/swaywm/sway/blob/master/sway/sway-ipc.7.scd)
and [Hyprland JSON output](https://github.com/hyprwm/Hyprland/blob/v0.52.0/src/debug/HyprCtl.cpp).

## Pagination, refresh, and reconnect

Both stdio and the existing HTTP POST/SSE-response transport follow opaque
`nextCursor` values until absent. Each discovery pass starts without a cursor;
cursors are never carried across refreshes or connections. Duplicate tool names,
repeated/invalid cursors, malformed pages, and limits fail the whole refresh.
Limits are 32 pages, 4,096 tools, 8 MiB of catalog metadata, 1,024-byte tool names,
and 4,096-byte cursors. Each response is capped at 8 MiB. Stdio responses and HTTP
requests have a 15-second deadline; discovery stops starting new pages after
30 seconds (a final outstanding request can take another 15 seconds).

Use the authenticated local control API, or opted-in bearer-authenticated TCP:

```sh
curl --unix-socket "$CONTROL_SOCKET" -X POST http://localhost/session/SESSION_ID/mcp/refresh \
  -H 'Content-Type: application/json' \
  -d '{"server":"editor","action":"refresh"}'
```

- `refresh` fetches the complete catalog on the current connection and replaces
  it atomically. It does not execute tools. Removed tools stop being callable
  through the session API. A failed refresh clears the catalog and disconnects.
- `reconnect` closes the old connection/child, initializes a new one from the
  session's originally selected server configuration, and discovers its tools.
  It can restart the configured stdio process, but never replays a tool call or
  loads newly edited project/user configuration. Open a new session to adopt
  configuration changes. Environment references use the daemon's environment.

Success returns 204; a concurrent refresh for the same server returns 409;
a missing/closed session returns 404; transport/discovery failures return 502.
While I/O is pending, `GET /session/{id}/mcp` reports `refreshing: true`, no tools,
and `healthy: false` for that server, so focused-app matching falls back.
Polling and session closure remain available. If the session closes during I/O,
the result is discarded and any new child is terminated when the bounded
operation finishes. Refresh actions are audited as `@refresh`/`@reconnect`
without raw config or arguments.

Tool calls are never automatically retried. A lost reply may mean the tool
already ran; inspect the application's state before deciding on another call.
Transport failures clear stale tools and health. Stdio health excludes exited
zombie children; HTTP failures invalidate the attached client. A valid
application/tool error is not itself a transport failure.

Responses must correlate to their request ID. Interleaved notifications do not
satisfy requests; an SSE notification after a response cannot replace that
response. This release uses explicit refresh: it does not maintain a background
notification subscription or automatically reconnect. The HTTP transport remains
the existing POST/SSE-response subset, not a legacy SSE endpoint-discovery or
full modern Streamable HTTP session implementation.

Pagination follows the [MCP cursor contract](https://modelcontextprotocol.io/specification/2024-11-05/server/utilities/pagination).
