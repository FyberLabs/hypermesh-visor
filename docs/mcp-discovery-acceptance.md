# MCP discovery acceptance ledger (#29)

This ledger separates implemented discovery behavior from platform qualification.
A green portable-source test does not qualify a native visor daemon or desktop.
#29 is not ready to close while the platform rows below remain unqualified.

The [qualification procedure](mcp-discovery-qualification.md) provides commands,
installed-platform cases, container isolation checks, and required evidence.
Discovery implementation can be handed off for acceptance without representing
the dependent native backends or container deployment as implemented.

## Recorded qualification

The [consolidated candidate record](qualification/2026-11-02-a70e6fd/README.md)
retains inspected native x86_64/ARM64 package evidence for source selection,
identity/precedence, profile overrides, HTTP/SSE lifecycle, durable trust and
in-flight crash recovery. Its full candidate/package hashes and sanitized PASS
excerpts are immutable evidence; its acceptance disposition remains blocked on
the native runtime/container gates below.

The [2026-11-02 merged-candidate record](qualification/2026-11-02-b08fffa/README.md)
contains inspected post-merge evidence for native x86_64 and ARM64 Debian/X11
packages at `b08fffabe0fd5ccf359085af56d9aa06501d33c6`, with package/executable
hashes, workflow links, retained sanitized excerpts and outstanding cases.
Both installed suites pass, including real-daemon trust persistence. This is a
CI candidate qualification, not release publication or complete #29 acceptance.

## Acceptance evidence

| Original criterion | Implemented evidence | Remaining qualification |
| --- | --- | --- |
| Inventory precedence, transports, bindings, health, focus | [Configuration and trust](mcp-discovery.md), resolver tests in config.rs, binding diagnostics, protocol/status API | Keep inventory accurate as native backends land |
| Discover documented locations and explicit endpoints with provenance/trust before startup | sources.rs; trust.rs/trust_disk.rs; companion review/editor APIs; original-byte receipts; live selected-source fixture | Native daemon/trust-store integration on Windows/macOS |
| Stable app hints with overrides and deterministic precedence; no title matching | X11 local PID/executable and WM_CLASS; Sway/Hyprland PID/app ID; exact executable matching, all-selectors rules, ordered bindings; X11 live fixture | Native Windows executable and macOS bundle/executable probes (#18); supported compositor qualification (#17) |
| Capabilities, pagination, refresh, reconnect, stale bindings, match diagnostics | protocol.rs/discovery.rs; stdio and HTTP lifecycle tests; status/refresh endpoints; explicit desktop reconnect; live crash/reconnect fixture | Native transport/process cleanup and focus lifecycle qualification |
| Ambiguity, unavailable servers, untrusted projects, restarts, fallback, secret-safe diagnostics | binding/config/trust suites; API trust and stale-focus tests; stdio/HTTP lost-reply tests; live no-replay and invalid-binding fallback | Same workflow on native platforms and chosen isolated container desktop |

## Platform gates

The [2026-11-02 readiness observations](qualification/2026-11-02-platform-readiness.md)
confirm access to assigned Windows/Apple Silicon hosts and a Linux container
engine. The remaining blockers are native runtime and container implementation;
host availability does not qualify those gates.

| Platform | Discovery source enumeration | Identity/trust/lifecycle in a shipped runtime | Owner/dependency |
| --- | --- | --- | --- |
| Linux X11 | Native fixture tests | Installed Debian packages pass on native hosted x86_64/ARM64: discovery, matching, lifecycle, binding recovery and restart persistence. See the recorded evidence; RPM, physical hardware, login autostart and remaining manual cases are not qualified | #29 |
| Linux Wayland | Same Linux source resolver | Unit-tested Sway/Hyprland hints; automation remains refused and the refusal is tested. GNOME/KDE portal qualification is not complete | #17 |
| Windows | Shared source/JSONC code runs on native Windows CI; APPDATA/USERPROFILE roots | Unqualified: full daemon, secure trust persistence, focused-app probe, transport cleanup, physical-user gate | #18, #25 |
| macOS | Shared source/JSONC code runs on native macOS CI; Application Support/HOME roots | Unqualified: full daemon, secure trust persistence, bundle/executable focus, permissions, transport cleanup | #18, #26 |
| Isolated Linux container | Source resolver is namespace-local and does not scan other homes/projects | Unqualified: two bots, separate identities/trust, allowed shared resources, restarts, and chosen virtual desktop | #27, #35 |

The Linux live fixture runs with a real X11 window and real stdio process through
the production daemon's Unix APIs. It asserts preview and approval do not start
a server, denied calls do not reach the process, and lost calls execute only once
across reconnect. It does not claim native Windows/macOS or isolated-container
acceptance.

## Deliberate protocol boundaries

The reconnect policy is explicit: failed connections require a user-requested
reconnect from the saved session configuration; tool calls are never replayed.
Only read-only GET subscriptions/catalog refresh reconnect automatically within
the documented bounds. This satisfies the implemented reconnect behavior without
requiring unattended process restarts or silently widening consent.

The original discovery criteria do not require every possible editor integration
or all optional MCP features. Additional editors, OAuth, legacy SSE, resumable
POST responses, full JSON Schema evaluation, resource/prompt operations, task
execution, and icon rendering remain documented compatibility limits. They must
not be treated as implemented merely because their metadata is discoverable.
They also must not become an unbounded sequence of additions that obscures the
remaining native-platform acceptance gates.

## Close procedure

1. Merge the reviewed discovery/lifecycle acceptance changes and verify the
   post-merge runs, including native source checks and Linux live display tests.
2. Link native runtime acceptance from #18/#25/#26 and container acceptance from
   #27/#35; record actual platform/build versions and the exercised workflows.
3. Confirm failures still clear stale routing, trust is explicit/persistent, and
   no credentials or private payloads appear in diagnostics on each platform.
4. Close #29 only when the original cross-platform scope has evidence, or after
   the owner explicitly approves a narrower scope and linked follow-up ownership.
