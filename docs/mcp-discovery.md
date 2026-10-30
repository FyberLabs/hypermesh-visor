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
  populated selectors must match. Current comparisons are case-insensitive.
- X11 uses `WM_CLASS`; Wayland probes Hyprland, Sway, and KWin on a best-effort
  basis. These are routing hints, not authenticated application identities.
- Bindings are cached with a one-second metadata refresh; focus has its own
  short cache. Failure to find a healthy match retains desktop mode. Input
  authorization and physical-user pause still apply.

Issue #29 remains open for a reviewable discovery/provenance UI, stronger stable
identity matching (including fixing Hyprland's legacy title-as-executable probe),
match diagnostics, capability negotiation/pagination, tool-list refresh,
reconnect behavior, and native Windows/macOS discovery. This change establishes
explicit project trust without claiming those discovery features are complete.
