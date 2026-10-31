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

## Graphical profile editing

Choose **MCP profiles** in the companion menu, or run
`hypermesh-companion --mcp-profiles` from your desktop session. Linux packages
include the Zenity runtime dependency; source installations need `zenity`
installed. The dialogs create profiles, edit server IDs (one per line), switch
between individual servers and Docker gateway mode, select the active profile,
and delete inactive profiles. Choose another active profile before deleting the
current one. Canceling a dialog before confirmation sends no write.

The editor changes membership and gateway selection, not server definitions or
credentials. Existing private per-server overrides and unknown profile fields
are preserved without being sent to the UI. Server IDs can refer to user config
or a source you will select during trust review. Gateway mode keeps the saved
membership list but attaches only the gateway under the normal resolver rules.
Creating the first profile also makes it active.

Saving membership/gateway changes (even unchanged values) or deleting a profile
persistently blocks that profile before replacing the file. Use **MCP trust** to
review a saved profile before opening new sessions. Saving never approves or
starts a server; activation does not lift an existing block. Running sessions
keep their attachments. A failed save can leave the profile blocked; reopen the
editor, verify the file, and review again. Changes to the shared profiles file
also require revalidation of other receipts after daemon restart.

The companion-only `GET /mcp/profiles` returns metadata and a file revision;
`POST /mcp/profiles` accepts `save`, `activate`, or `delete` with that revision.
Stale revisions are rejected. Cooperative editors serialize with a private lock,
write a mode-0600 temporary file, and atomically replace `mcp-profiles.json`.
External tools should use the API or the same lock; arbitrary writers that ignore
the lock cannot receive an atomic compare-and-swap guarantee. Symlink, hardlink,
foreign-owner, nonregular, malformed, or oversized profile files are refused.
Reads do not create configuration files.

The graphical editor creates at most eight profiles, with at most 128 server IDs
per edited profile. Persistent trust currently retains up to eight profile
decisions, including deleted/revoked names; reuse an existing name when that
limit is reached. These limits and preserved overrides are visible in errors or
the save confirmation. Server definitions are edited in the separate **MCP servers** screen.

## Graphical server editing

Choose **MCP servers** or run `hypermesh-companion --mcp-servers`.
Create a stdio command or HTTP/SSE endpoint, enable/disable a server, change its
working directory, replace arguments/environment/headers, or delete its user
definition. Add its ID to a profile through **MCP profiles**, then review that
profile through **MCP trust**. The editor never starts a server.

The list shows command, working-directory, transport, enabled state, argument
count, environment/header names, and endpoint origin as metadata. Existing
argument, environment/header values, and URL credentials/path/query are never
fetched into the screen. Private fields use explicit replacement dialogs:
arguments are a JSON array of strings, environment/headers are JSON objects of
string values, and endpoints are full HTTP(S) URLs. Replacing a map replaces all
its entries; `{}` clears it, and `[]` clears arguments. Cancel preserves existing
values. Review replacements privately before confirming; they are written to
your private configuration file, not a vault. Prefer environment references
where appropriate. Private replacement payloads do not appear in dialog command arguments,
save confirmations, or API error messages.

Untouched fields, including unknown fields and private values, are preserved.
Changing transport keeps other fields; only the selected transport is used.
Deleting a user definition can expose an explicitly selected project's duplicate
definition or the implicit Docker gateway, so deletion also requires renewed
review.

The companion-only `GET /mcp/servers` returns redacted metadata and a revision.
`POST /mcp/servers` accepts `save` with `name`, `create`, `revision`, and a
`fields` patch, or `delete` with `name` and `revision`. Omitted fields are
preserved; explicit JSON null removes a field. Writes share the profile editor's
lock and atomic mode-0600 replacement. Existing safety checks and revision
conflict handling apply. Creates are limited to 128 user servers and the file
to 256 KiB.

Because `mcp.json` is shared, every server save/delete persistently blocks all
configured, active, and previously reviewed profile names before writing.
A failed write can leave those profiles blocked. Membership is not changed.
The existing eight-decision trust limit can reject an edit before the file is
changed; decisions are updated together. Existing sessions retain their captured
configuration. Pending reviews for those profiles are canceled; review again
after saving. Explicit vault MCP retrieval remains a separate named-server
operation, as described above.

## Review discovery sources in the companion

Open the bug menu and choose **MCP trust**, or run
`hypermesh-companion --mcp-trust-gui` in your desktop session. Choose **Review a
profile**, select its visor profile, and choose daemon configuration, a discovered
editor source, or an explicit configuration file. A read-only provenance dialog
precedes the separate typed **TRUST** confirmation. Canceling or a dialog failure
cancels the staged token; opening the screen grants nothing. Select a listed
decision and type **REVOKE** to block future sessions.

The terminal equivalent remains `hypermesh-companion --mcp-trust` in an
interactive terminal. Both screens use the same content-bound approvals,
redaction, five-minute review tokens, and durable revocations. The detailed
review sequence is:

1. Choose **Review a profile** and enter its name, or leave it blank for the active
   user profile. The profile must already exist in `mcp-profiles.json`; its server
   list remains the selection boundary. Use **MCP profiles** or edit that file to change membership.
2. Select a listed editor source ID, or enter an absolute MCP JSON
   file or directory containing `mcp.json`.
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

The pending review token is single-use, expires after five minutes, and checks that all source
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

Approvals and revocations survive restart. Private receipts in
`<user-config-dir>/mcp-trust/approvals.json` store profile names, review IDs,
source kinds, absolute paths, and SHA-256 content hashes, including absent optional
files. They never copy server definitions, arguments, or secret values. The directory
is mode 0700 and files are mode 0600, owned by the daemon user. Writes use a lock,
revision comparison, and atomic replacement; conflicting updates require a retry.

A running daemon retains its approved in-memory snapshot. After restart, each
receipt reconstructs that snapshot only if every source still matches. Changed,
missing, unreadable, or newly created sources show `needs_review` and block new
sessions until reapproved. Revocations remain blocked. Other daemon processes
observe persisted decisions before opening sessions. Pending reviews do not
survive restart. These decisions do not edit the source files.

Unsafe permissions, malformed receipts, or a missing ledger in an existing trust
directory fail closed. Stop the daemon and restore a known-good private backup
before retrying; do not delete the ledger as a repair. Removal of the entire trust
directory is detected by a running daemon, but a fresh process cannot distinguish
it from a first installation. Back up this directory with the configuration.
Vault MCP secret retrieval remains separate and does not use session-profile grants.

Known locations on Linux:

| Source ID | MCP file |
| --- | --- |
| `zed_user` | `<config>/zed/settings.json` |
| `junie_user` | `~/.junie/mcp/mcp.json` |
| `devin_user` | `~/.config/devin/mcp_config.json` |
| `cursor_global` | `~/.cursor/mcp.json` |
| `vscode_user` | `<config>/Code/User/mcp.json` |
| `vscode_insiders_user` | `<config>/Code - Insiders/User/mcp.json` |
| `vscode_profile:<id>` | `<config>/Code/User/profiles/<id>/mcp.json` |
| `vscode_insiders_profile:<id>` | `<config>/Code - Insiders/User/profiles/<id>/mcp.json` |

`<config>` is absolute `$XDG_CONFIG_HOME`, otherwise `~/.config`.
Listing checks metadata without reading configuration contents. Named profiles
appear only when they contain a concrete MCP file. Their IDs are editor storage
directory IDs, not display names or visor profile names. Visor does not consult
the editor's active-profile registry, inherit default MCP settings, or reproduce
workspace associations; choose the file you intend to approve. The visor profile
still controls server membership.

Enumeration reads one level of each known profiles directory, skips directory
symlinks and IDs outside 1–40 ASCII letters/digits/underscore/hyphen, and returns
at most 16 sorted profiles per editor. More than 128 directory entries, unreadable
roots, or overflow produce a `use_explicit_path` notice. Select an absolute file
path for omitted profiles, portable/custom user-data installations, or workspace
configurations. Projects are never scanned or automatically imported.

External imports accept JSON and JSONC (line/block comments and trailing commas)
with exactly one `mcpServers`, `servers`, or Zed `context_servers` map. JSON5 features such as single quotes,
unquoted keys, and non-JSON numbers are unsupported. User/profile files belonging
to visor itself still require strict JSON. Receipts hash the original external
file bytes, so even comment-only edits require review after restart. Existing
strict-JSON approvals retain their semantics.

Zed imports support custom command/args/env and URL/headers definitions, including
`enabled: false`. Extension-provided definitions and legacy command objects need
an explicit compatible export. Only these unrelated presentation settings may
accompany `context_servers`: `theme`, `icon_theme`, `buffer_font_family`,
`buffer_font_size`, `ui_font_family`, `ui_font_size`, `vim_mode`, `tab_size`,
`soft_wrap`, and `show_whitespaces`. Other root settings (including agent policies,
release-channel overrides, and settings layers) are rejected rather than ignored.
The complete settings file is pinned, including presentation changes.

Devin Local discovery uses its current dedicated user MCP file, not the older
`config.json` layout. Visor does not migrate older files, merge project/local
layers, or inherit editor enterprise policies or OAuth sessions. Its `disabled`
flag is preserved; explicit `transport: "http"` maps to visor's HTTP transport.
Legacy SSE, OAuth options, file substitutions, and conflicting transport fields
are rejected. Supply static compatible server definitions and review their
credentials separately. Neither integration starts or contacts servers during
listing, preview, or approval; attach remains a separate approved-session action.

Junie user configuration is listed from `~/.junie/mcp/mcp.json`, shared by its
IDE plugin and CLI. Supported command/args/env and URL/headers entries use the
existing `mcpServers` importer. Visor does not scan Junie projects, merge its
custom locations, inherit its approvals, or reuse OAuth sessions. Select project
files explicitly. Junie-specific `timeout`, OAuth, and other unsupported fields
require a privately reviewed compatible export; they are never silently dropped.
Visor's request deadlines and explicit trust/profile membership still apply.

External imports accept explicit `type: "stdio"` and `type: "http"`, and normalize
`type: "streamable-http"` to `http`. Omitted types retain command/URL inference.
Legacy `type: "sse"`, empty/null/non-string types, and unknown types are rejected.
This avoids interpreting an editor's legacy SSE transport as visor's historical
POST-based `sse` alias. The alias remains available in visor's own configuration;
this restriction applies to imported files and their restored approvals. Existing
external SSE receipts will need a compatible source and fresh approval after a
restart. Do not merely rename a legacy SSE transport: verify that the endpoint
actually supports Streamable HTTP first. Continue's documented JSON MCP files
can already be selected explicitly; its YAML files and project directories are
not scanned or imported.

Imports preserve visor's user-first precedence and profile membership. Editor
inputs, sandbox settings, env files, OAuth settings, and unsupported variable
expansions remain rejected; supply a privately reviewed compatible export.
Exact `${env:NAME}` references in environment values remain supported. Editor
security settings are never silently discarded. Downgrading to a version without
these source IDs rejects their receipts; keep backups and use a compatible
version to review or revoke them.

Source references: [Junie MCP configuration](https://junie.jetbrains.com/docs/junie-cli-mcp-configuration.html),
[Continue JSON and transport formats](https://github.com/continuedev/continue/blob/main/docs/customize/deep-dives/mcp.mdx),
[Cursor MCP configuration](https://prod.cursor.com/help/customization/mcp),
[VS Code MCP configuration](https://code.visualstudio.com/docs/agents/reference/mcp-configuration),
[profile file layout](https://code.visualstudio.com/docs/configure/settings#_profile-settings),
[Zed custom MCP servers](https://zed.dev/docs/ai/mcp),
[Zed Linux configuration](https://github.com/zed-industries/zed/blob/main/docs/.doc-examples/configuration.md),
[Devin MCP configuration](https://docs.devin.ai/cli/extensibility/mcp/configuration),
and [VS Code profile resource resolution](https://github.com/microsoft/vscode/blob/main/src/vs/platform/userDataProfile/common/userDataProfile.ts).

The UI uses `GET /mcp/sources`, `POST /mcp/discovery` (optional `source_id` or
`project_path`, mutually exclusive), `GET /mcp/trust`, and `POST /mcp/trust` with
`approve`, `cancel`, or `revoke`. These routes require the same-user companion
socket; control-socket callers and TCP callers, including valid bearer holders,
receive 403. This is a local account boundary, not protection against malicious
processes already running as the same OS user. Trust actions are recorded in the
MCP audit without config values or arguments. Configuration text is escaped before
terminal display. Reviews allow at most 128 candidates, 256 KiB per source file,
200 KiB of preview JSON, eight pending reviews, and eight approved/revoked profiles.

## Current matching and transport inventory

- Stdio and HTTP/SSE transports initialize and load tools on attachment. Closing
  a session terminates its stdio children and schedules best-effort HTTP session cleanup.
- `GET /session/{id}/mcp` reports attached servers, health, and tools.
- `mcp-bindings.json` matches focused identity fields, not window titles, against
  attached healthy server IDs. The first matching healthy binding wins; all
  populated selectors must match. Class/app-ID comparisons retain case-insensitive compatibility. Executable
  paths match exactly, including case; use the resolved absolute path.
- X11 uses `WM_CLASS` and, on Unix-socket displays (`:N[.S]` or `unix:N[.S]`),
  a same-user `/proc/<pid>/exe` hint from `_NET_WM_PID`. The bounded PID property
  must be exactly one positive CARDINAL/32 value, and `WM_CLIENT_MACHINE` must
  exactly match the local hostname (with an optional trailing NUL). Missing,
  malformed, truncated, nonlocal, inaccessible, or deleted process hints contribute
  no executable; class matching remains available. TCP/SSH-forwarded displays,
  hostname aliases/FQDNs that differ from the local hostname, and different PID
  namespaces intentionally receive no reliable executable support. No DNS lookup
  or title/command-line inference is used. The active window is checked again
  before publishing the identity. These client-supplied properties are routing
  hints, not authenticated process ownership or protection from spoofing.
  See the [EWMH process and client-machine properties](https://specifications.freedesktop.org/wm/latest-single/).
- Wayland probes Hyprland, Sway, and KWin on a best-effort
  basis. Hyprland and Sway resolve the reported positive PID through `/proc/<pid>/exe`
  for the same user; missing, inaccessible, deleted, or foreign-user processes
  contribute no executable. Titles and command lines are never executable
  identities. These are routing hints, not authenticated application identities.
- Bindings are re-read at most once per second with bounded I/O; focus has its own
  short cache. Expired focus is withheld while refreshing, so a stalled probe
  cannot preserve a stale match. Failure to find a healthy match retains desktop mode. Input
  authorization and physical-user pause still apply.

Issue #29 remains open for additional documented discovery sources, additional editor integration, native stable
identity coverage beyond the current Linux probes, automatic server reconnect
policy, and native Windows/macOS discovery. The implemented protocol surface and
remaining compatibility limits are described below.

## Match diagnostics

`GET /companion` includes `session.mcp_match` alongside the existing `mcp`,
`mode`, `prefer_mcp`, and `focused` fields. It does not attach or start servers.

- `reason`: `focus_unavailable`, `no_bindings`, `identity_mismatch`,
  `server_unavailable`, `bindings_unavailable`, or `matched`.
- `bindings_status`: `missing`, `ready`, `invalid`, `unavailable`, or `too_large`.
  This reports configuration health independently of focus availability.
- `binding_index`: zero-based index in `mcp-bindings.json` of the winning binding,
  present only for a match. Reorder bindings to set explicit precedence.
- `identity_matches`: number of bindings matching every populated selector.
- `healthy_matches`: number of those bindings naming an attached healthy server.
  More than one exposes overlapping rules; the first healthy binding wins.

`server_unavailable` means the identity matched but none of its servers is
attached, healthy, and offering a directly callable tool. `no_bindings` means a missing file or an explicitly empty
`bindings` list (distinguished by `bindings_status`). Malformed configuration,
unknown fields, invalid selectors, and read/size failures produce
`bindings_unavailable`, zero match counters, and desktop fallback, even when
focus is unavailable. No parser excerpts, paths, or raw selector values are
included in diagnostics.

Binding files must be regular files, at most 64 KiB, with at most 128 rules.
Each rule requires a nonblank server ID (at most 1,024 bytes) and at least one
nonblank `wm_class`, `app_id`, or absolute `executable` selector (at most 4,096
bytes each, without NULs). Unknown root/rule fields reject the whole file, so a
misspelled selector cannot weaken a multi-selector rule. Correct unsupported
fields rather than relying on them being ignored. An empty `bindings` list is
valid. Nonblocking opens reject FIFOs/devices rather than waiting for content;
reads are capped even if a file grows during loading.

The bounded refresh re-reads content instead of trusting mtime, so equal-timestamp
edits and changed read permissions cannot indefinitely preserve old routing.
Failures clear cached rules on the next refresh; corrected files recover on the
next refresh without restarting the daemon. This is still a one-second routing
cache, not a new authorization boundary; loaded server approvals and sessions
are unchanged.

`focus_unavailable` also covers an expired sample while its refresh is pending. Counters describe the
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

`GET /session/{id}/mcp` preserves each discovered tool's optional `title`,
`inputSchema`, and `outputSchema`, using the MCP field names alongside `name`
and `description`. Schemas change atomically with the catalog on refresh and
reconnect. A malformed schema or text field rejects the entire catalog; failed
refresh clears the old metadata and removes the server from healthy routing.
All metadata, including schemas, counts toward the existing 8 MiB catalog limit.

Schemas must be JSON objects when supplied; title/description must be strings.
For compatibility, servers that omit input schemas remain discoverable, with
`inputSchema` absent rather than an invented no-argument schema. This is not full
MCP schema conformance validation: schema keywords and argument/result values
are not evaluated here. References are retained as data and never fetched.
Downstream clients can use the supplied schemas to construct calls, but must
still treat server metadata as untrusted. Metadata does not grant tool permission
or change call approval. Optional `annotations` and `execution` objects are also preserved. Known
annotation hints (`readOnlyHint`, `destructiveHint`, `idempotentHint`, and
`openWorldHint`) must be booleans; annotation `title` must be text. Missing hints
are left absent. Annotations remain untrusted descriptions and never bypass
approve/deny decisions, affect routing, or grant permissions. Unknown annotation
and execution fields remain data within the same size limit.

`execution.taskSupport`, when supplied, must be `forbidden`, `optional`, or
`required`. Visor does not implement task execution. A `required` tool remains
visible for inspection, but even an approved call fails locally before transport
I/O; health and the catalog remain intact. Focused-app routing excludes servers
with no directly callable tools (empty or entirely task-required catalogs),
allowing desktop fallback even while the connection is healthy. `optional`, `forbidden`, and omitted
values allow the existing direct-call path, still subject to normal approval.
Refresh replaces these requirements with the catalog; invalid execution metadata
fails the entire refresh. This is compatibility gating, not validation of whether
a server's claims are truthful. Task requests are never synthesized or retried.
The existing call API reports unsupported task-required calls as 502 and records
a failed approved call without arguments. Icons are not exposed yet. See the [MCP tool definition](https://modelcontextprotocol.io/specification/2025-11-25/server/tools#tool).

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

When a modern HTTP attachment with a negotiated session ID is discarded (close,
explicit reconnect, or failed attachment/transport), visor schedules one
best-effort `DELETE` to its original MCP endpoint with that connection's session
ID, negotiated version, and configured headers. Stateless, unnegotiated, and
`2024-11-05` connections do not send DELETE. A new connection never inherits the
old ID. This follows the [MCP session termination guidance](https://modelcontextprotocol.io/specification/2025-11-25/basic/transports#session-management).

Cleanup runs outside session locks on one process-wide worker with a queue of
32 requests and a three-second request deadline. Local closure stops subscription
work immediately and never waits for cleanup capacity or a remote response.
Queue saturation, unavailable worker, process exit, and network/server failures
can leave remote cleanup incomplete; server-side expiry remains necessary.
Cleanup is not persisted, retried, or reported as confirmed remote deletion.
Redirects are disabled, response bodies are ignored, and 404/405 are terminal
just like other outcomes. Cleanup never initializes a session or replays tools.
An already-running subscription read can still finish within its existing bound.

The `sse` configuration value remains an alias for the POST-based transport.
Legacy HTTP+SSE endpoint discovery, resumable POST responses, OAuth negotiation,
and non-tool MCP features are not implemented.
A server selecting `2024-11-05` over HTTP retains the existing POST/JSON/SSE
response compatibility path, without a GET subscription. This is not a claim
of complete MCP conformance. Additional discovery sources and broader platform
acceptance remain tracked in #29.

Protocol references: [lifecycle and capabilities](https://modelcontextprotocol.io/specification/2025-11-25/basic/lifecycle)
and [Streamable HTTP transport](https://modelcontextprotocol.io/specification/2025-11-25/basic/transports).

Pagination follows the [MCP cursor contract](https://modelcontextprotocol.io/specification/2024-11-05/server/utilities/pagination).
