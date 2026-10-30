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

## Review discovery sources in the companion

Open the bug menu and choose **MCP trust**, or run `hypermesh-companion --mcp-trust`
in an interactive terminal. The companion opens its trust screen in the installed
terminal emulator, alongside the existing MCP profile and doctor actions.

1. Choose **Review a profile** and enter its name, or leave it blank for the active
   user profile. The profile must already exist in `mcp-profiles.json`; its server
   list remains the selection boundary. Use the existing profile tools or edit
   that file to change membership.
2. Optionally enter an absolute MCP JSON file or directory containing `mcp.json`.
   This previews the file without setting `HYPERMESH_MCP_PROJECT`, editing user
   configuration, starting a command, or connecting to an endpoint. Blank uses
   the daemon's existing configuration selection. No project scan occurs.
3. Review each server's source path and kind, transport, executable, endpoint
   origin, enabled state, working directory, argument count, environment/header names, and profile override fields.
   The profile file supplying overrides is shown separately. Status explains
   whether an entry is selected, disabled, outside the profile, or shadowed by a
   user definition. An implicit Docker gateway is identified as `gateway_default`.
4. Review the complete files privately. Argument values, environment/header
   values, URL credentials, paths, and queries are hidden in this screen. Type
   **TRUST** to approve the snapshot. Any other response, cancellation, or EOF
   cancels; merely opening the screen never grants trust or starts a server.

Approval is single-use, expires after five minutes, and checks that all source
file contents still match the preview. A changed, removed, or newly created
source requires another review. Future sessions selecting that profile (including
when it remains the active profile) attach the captured effective configuration,
not newly edited file contents. `GET /session/{id}/mcp` includes `provenance` and
`trust_review` so a session's actual attachment source remains visible after edits.
Existing explicitly configured user/environment selections retain their previous
behavior when no reviewed snapshot overrides the profile.

**Revoke a profile** removes its snapshot and blocks new sessions for that profile
until another review is approved. It does not terminate existing sessions or their
reconnects; close those sessions separately. An attachment already in progress is
discarded if trust changes before it can be published as a session. Approval does
not pin executables, working directories, referenced files, or environment values
resolved when a process starts. Keep those resources under your control.

Snapshots, revocations, and pending reviews are daemon-local and disappear on
restart; they do not change `mcp.json` or profiles on disk. Restart restores the
normal file/environment configuration behavior. Vault MCP secret retrieval is a
separate explicit operation and does not use these session-profile grants.

The UI uses `POST /mcp/discovery`, `GET /mcp/trust`, and `POST /mcp/trust` with
`approve`, `cancel`, or `revoke`. These routes require the same-user companion
socket; control-socket callers and TCP callers, including valid bearer holders,
receive 403. This is a local account boundary, not protection against malicious
processes already running as the same OS user. Trust actions are recorded in the
MCP audit without config values or arguments. Configuration text is escaped before
terminal display. Reviews allow at most 128 candidates, 256 KiB per source file,
200 KiB of preview JSON, eight pending reviews, and eight approved/revoked profiles.

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

Issue #29 remains open for additional documented discovery sources, richer graphical
profile editing/persistent approvals, native stable
identity coverage beyond the current Linux probes, automatic server reconnect
policy, and native Windows/macOS discovery. The implemented protocol surface and
remaining compatibility limits are described below.

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
and 4,096-byte cursors. Each response is capped at 8 MiB. Stdio writes and response waits each have a 15-second deadline; HTTP
requests have a 15-second I/O deadline; discovery stops starting new pages after
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

## Background catalog updates and protocol compatibility

The client proposes MCP `2025-11-25` and accepts server-selected `2025-06-18`,
`2025-03-26`, or `2024-11-05`. Missing/unsupported versions and malformed
capabilities fail attachment. Servers without a `tools` capability attach with
an empty catalog; no `tools/list` requests are sent. The client advertises no
sampling, elicitation, roots, or task capabilities. It answers server pings and
returns method-not-found for unsupported client requests.

When a server advertises `tools.listChanged`, idle stdio notifications and
Streamable HTTP GET/SSE notifications invalidate its catalog. A per-session
worker coalesces notifications and refreshes at most once per second per server
(with longer intervals while another refresh is pending). Invalidated catalogs
are not healthy routing candidates or callable through the session API. Complete
catalogs replace old ones atomically; a notification during pagination remains
pending for another pass. Automatic catalog reads are audited as
`@subscription_refresh`. No automatic action executes a tool or starts a new
server process. A failed reader/refresh requires explicit reconnect.

HTTP initialization captures `Mcp-Session-Id`; subsequent requests include it
and the negotiated `MCP-Protocol-Version`. These connection-owned headers,
Content-Type, and Accept override configured values. Redirects are disabled.
A session-expired HTTP 404 invalidates the attachment without replaying the
request. JSON and bounded incremental SSE responses are supported; a matching
response completes without waiting for EOF. Notifications cannot satisfy a
request. Each SSE event is limited to 8 MiB, and session/event IDs to 1,024 bytes.

Background HTTP GET is optional: 405 leaves the attachment usable with manual
refresh. For accepted streams, EOF/read timeout reconnects only the read-only
GET, sends the last event ID when present, respects `retry` with a minimum
one-second delay, and invalidates the catalog to reconcile delivery gaps.
Each HTTP request/stream has a 15-second I/O deadline, so an idle subscription
periodically renews. Other GET failures or malformed events invalidate health.
Closing a session stops new subscription work; an outstanding HTTP read can
finish within its deadline. Stdio reader shutdown is joined and its child reaped.
As with explicit refresh, an in-flight catalog operation finishes within its
bounds before a closed session's resources are discarded.

The `sse` configuration value remains an alias for the POST-based transport.
Legacy HTTP+SSE endpoint discovery, resumable POST responses, remote HTTP
session DELETE, OAuth negotiation, and non-tool MCP features are not implemented.
A server selecting `2024-11-05` over HTTP retains the existing POST/JSON/SSE
response compatibility path, without a GET subscription. This is not a claim
of complete MCP conformance. Additional discovery sources and broader platform
acceptance remain tracked in #29.

Protocol references: [lifecycle and capabilities](https://modelcontextprotocol.io/specification/2025-11-25/basic/lifecycle)
and [Streamable HTTP transport](https://modelcontextprotocol.io/specification/2025-11-25/basic/transports).

Pagination follows the [MCP cursor contract](https://modelcontextprotocol.io/specification/2024-11-05/server/utilities/pagination).
