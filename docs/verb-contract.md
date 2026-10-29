# Desktop verb and capability contract

This audit describes the Linux implementation. `GET /capabilities` returns
schema version 1 without opening a session, taking a screenshot, recording audio,
or connecting an MCP server. It uses the same protected Unix sockets and opt-in
TCP bearer authentication as the other routes.

`conditional` means implemented but dependent on device access, services, consent,
and the automation gate. `disabled` means deliberately refused. `unavailable`
means no recognized backend. `fixture` means test data, never live desktop access.
Discovery is neither a readiness probe nor an authorization grant. Clients must
handle operation errors even after discovery. Ignore unknown additive fields;
reject unsupported schema versions. There is no capability cache lifetime promise.

```sh
curl --unix-socket "$HYPERMESH_RUNTIME_DIR/control.sock" \
  http://localhost/capabilities
```

Set `HYPERMESH_RUNTIME_DIR` to the daemon's private runtime directory for this
example. Do not make its socket or bearer token available to other users.

## HTTP inventory

Paths below use `/session/{id}` for an existing session. JSON shapes are summarized;
the deserializers in `src/api.rs`, `src/input.rs`, `src/harness.rs`, and
`src/vault.rs` remain the implementation reference.

| Method and path | Input and result | Preconditions or limits |
| --- | --- | --- |
| `GET /capabilities` | Versioned JSON implementation description | No session; normal transport authorization |
| `POST /session` | `purpose`, `recipes`, `agent`, `skills`; optional `secrets`, `mcp_profile`; returns session ID and attached MCP profile/servers | Valid harness and secret sources |
| `DELETE /session/{id}` | Close session | Ends its work and clears session secrets |
| `POST …/view` | No body; PNG bytes | Live session and capture backend |
| `GET …/watch` | SSE capture updates; optional `mode` | Live session; disconnect/close ends streaming |
| `GET …/listen` | PCM audio stream | Live session; PulseAudio-compatible service; s16le, 48 kHz, mono |
| `POST …/mouse` | `action`: move/click/drag; `x`,`y`; drag adds `to_x`,`to_y`; optional left/right/middle `button`; move accepts `clickable` | X11 automation gate; no wheel/scroll action |
| `POST …/type` | `text`, `keys:[{key,action}]`, or secret handle `secret`; actions tap/down/up | At most 4096 planned keystrokes; X11 automation gate |
| `GET …/feed` | Activity and latest snapshot handle | Does not return secret values or file bytes |
| `GET …/feed/snapshot` | Cached PNG | Does not capture a new frame |
| `POST …/stream` | Newline-delimited prompt/file/secret items; acknowledgement lines | `X-Api-Key`; at most 1 MiB body; prompt items enter prompt routing |
| `GET …/inbox` | Held prompts, files including base64 content, secret handles | Sensitive file content; not a public forum feed |
| `GET …/mcp` | Profile, server health, tool names and schemas | Session tool discovery |
| `POST …/mcp/call` | `server`, `tool`, `decision` approve/deny, optional `arguments` | Explicit tool decision; returns outcome/result |
| `GET /companion` | Desktop/session state, focus and preferred MCP binding | Does not grant execution permission |
| `POST /companion/automation` | `action` | Trusted companion link required to lift a user pause |
| `POST /companion/interrupt`, `/companion/notice` | Companion control messages | See API request types; not task submission APIs |
| `POST /prompt` | Prompt and optional model | `X-Api-Key`, configured routing and model validation |
| `GET /audit`, `/mcp-audit` | Operation metadata | Not secret storage |

Invalid/closed sessions return 404; malformed semantic input generally returns
400. TCP authentication fails with 401 before routing. Root refusal and forbidden
control return 403. Paused/interrupted input returns 409, possibly with an error
string describing the number of keystrokes/pointer steps sent. Disabled Wayland
input and unimplemented secret sources return 501. Backend/MCP failures generally
return 502; unavailable automation conditions can return 503. Framework parsing
errors can have different status/body formats from the API's `{"error":…}`.

## Geometry, cancellation, and retries

X11 input uses root-window pixel coordinates, checked against known root geometry
and X11 coordinate bounds. There is no monitor ID, display enumeration, DPI
transform, screenshot crop transform, or normalized coordinate API. Clients must
not assume CSS pixels equal input pixels. The contract deliberately publishes no
Wayland coordinate promise while injection is disabled (#17).

Input runs in batches under a shared automation gate. Physical input, pause/stop,
session close, and watchdog events can interrupt an operation. Held keys/buttons
are released during unwind. A client HTTP timeout is not a cancellation receipt.
There is no general request ID, task cancellation endpoint, or idempotency key.
Do not automatically retry a click, type, drag, or MCP tool call after a timeout or
partial-progress error: observe state and reconcile first. Read-only discovery
can be retried. Watch/listen clients should bound their own consumption time.
MCP HTTP transport currently has a 15-second timeout; this is not a global API
execution deadline. Cancellation and structured progress need further work in #32.

## MCP preference and inverse adapter

`GET /companion` reports a preferred healthy configured binding for the focused
app. This is advisory; it does not route an HTTP desktop verb through MCP.
Clients should inspect `GET …/mcp`, choose an appropriate tool, and retain the
same authorization/approval boundary. A failed non-idempotent MCP call must not
silently trigger desktop replay. Discovery and stale-binding work is #29.

The inverse stdio adapter advertises only `view`, `mouse_move`, `mouse_click`,
and `type_text`. It needs an existing session. It does not yet expose watch,
listen, drag, named keys, secret handles, capability discovery, or session/task
lifecycle. Its current `view` response is a status string, not image content;
therefore the advertised tool alone does not complete a visual observation loop.
Argument validation and structured error fidelity also need improvement in #32.

## Task coverage and evidence

| Task | Current implementation evidence | Gap / next acceptance |
| --- | --- | --- |
| Browser/editor/terminal interaction | X11 capture, typing and pointer primitives; live xev injection tests | App-specific end-to-end tasks remain unverified in #28 |
| Scroll a document | No scroll verb | #38 defines bounded scroll and MCP parity |
| Drag and keyboard chords | Live held-release/interruption tests; named-key planner | Application-specific shortcuts/layouts need acceptance |
| Multiple windows | Root coordinates and best-effort focused-app identity | No semantic window enumeration/activation API |
| Multiple displays / DPI | Root geometry validation | Display enumeration and explicit coordinate transforms absent |
| Clipboard | No direct API | Must define scoped read/write and secret handling before adding |
| File handoff | Stream/inbox retain file bytes | No filesystem materialization/shared resource authorization; #27/#37 |
| Audio | Recorder implementation and deterministic fixture | No live audio acceptance or playback API |
| Native Wayland | Refusal/access-control scenarios | Portal/libei scenario is skipped; #17 |
| Windows/macOS | Linux compile guard | #18/#25/#26 |
| Container bots | No supported production deployment yet | #27/#35 |
| Conventional AI client | Four inverse MCP tools | Image result, validation, parity and lifecycle: #32 |

The live X11 harness covers 14 scenarios: uinput channel, key state, TCP startup,
injection, stop/resume ordering, physical key/motion pause, pausing-click behavior,
menu behavior, synthetic-click rejection, watchdog, held-input release,
auto-resume, and cross-user denial. These are primitive and security checks,
not evidence that browser/editor task suites or all desktop platforms work.

Issue #28 remains open until its application-level acceptance matrix is exercised.
The shared vault (#37) must define read/use/execute distinctions before secret
files and executable skills become shared resources. A generic shell verb is not
an implicit fallback for missing desktop capabilities.
