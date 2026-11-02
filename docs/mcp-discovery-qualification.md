# MCP discovery qualification procedure

Use this procedure to collect acceptance evidence for #29. Implementation review,
portable-source CI, live Linux CI, and installed-platform qualification are
separate results. A blocked platform is not a pass. This procedure is ready for
qualification planning; Windows/macOS and container execution require their
runtime dependencies first (see the [acceptance ledger](mcp-discovery-acceptance.md)).

## Reproducible automated baseline

Run on the exact candidate revision, in an isolated test checkout/account:

```sh
cargo test --locked --workspace
cargo test --locked --manifest-path ci/mcp-discovery/Cargo.toml
bash ci/check-includes.sh
```

The `test` workflow runs the shared production source resolver on Linux, Windows,
and macOS. Those jobs qualify source enumeration/JSONC only. Run the `display`
workflow on the same revision for live Linux X11 and Wayland refusal evidence.
Its X11 job installs dependencies and runs `bash ci/display/run.sh x11`; the
harness needs Xorg, root-controlled uinput, and passwordless narrowly scoped sudo.
Use a disposable runner, not a user's active desktop. Do not make uinput
world-writable. The workflow checks device permissions before and after the run.

The `mcp-discovery-x11-<sha>` artifact contains only the scenario log, including
its final PASS line or safe failure diagnostics. Require a successful complete
workflow as well: a scenario artifact alone does not establish harness cleanup,
network isolation, or the other display scenarios. Missing evidence is not a pass.
Record the Actions run URL, attempt, full commit SHA, OS image and artifact with
an acceptance report. GitHub artifacts expire; retain the reviewed report and
relevant sanitized evidence with the release qualification record.

The real X11 fixture tests:

- Preview and approval do not spawn a server; private sentinel values stay absent
  from preview and compact status.
- A paginated catalog attaches to a real process and routes by exact executable.
- A separate `xmessage` process with the same window title does not match the
  bound Python app. Returning focus restores routing without restarting MCP.
- Terminating the bound app while leaving its stale active-window hint clears
  routing. A new app process restores the executable match while the MCP
  attachment remains healthy. These are X11 routing hints, not authenticated
  application identity; the harness supplies EWMH properties without a window
  manager.
- Two healthy attachments demonstrate explicit first-match order; reversing
  rules changes the winner. An unavailable first target is skipped, and a
  matching class with a mismatched executable does not match. Status must report
  the expected winning index and identity/health counts. Rule edits neither
  restart servers nor execute tools.
- Catalog refresh retains the catalog without spawning another process.
- A denied call produces no server event; a lost reply clears routing and tools.
- Explicit reconnect starts one replacement and never replays the lost call.
- Invalid bindings clear routing without detaching; correcting them restores it.
- Revocation prevents a new session from starting a process; the existing
  attachment retains its documented snapshot until explicitly closed.

The separate `mcp-persistence` scenario stops and starts the real daemon three
times using the same private configuration directory. It verifies that unchanged
approvals restore, comment-only source changes require renewed approval, renewed
approval attaches successfully, and revocation survives another restart without
starting a process. It also checks receipt ownership/mode and absence of the
fixture secret. Sessions are closed before each restart: crash recovery with a
live in-flight session is not asserted. Both source and installed-package jobs
retain `mcp-persistence.log` alongside the discovery log.

The `mcp-daemon-crash` scenario kills the actual daemon with SIGKILL only after
an MCP fixture records an in-flight call's side effect. Linux pidfds pin the
exact daemon and child processes. It checks failed delivery to the client, exit
of the pipe-aware stdio child, absence of old sessions after restart, restoration
of unchanged trust on an explicit new session, and no replay of the call. This
qualifies a child that exits on stdin EOF; it does not establish forced cleanup
of uncooperative servers, detached descendants, or remote HTTP side effects.
`mcp-daemon-crash.log` is retained by source and both installed-package jobs.

The `mcp-source-precedence` scenario verifies the live review provenance and
actual process starts for conflicting user/explicit-source definitions. The user
definition wins even when disabled; a lower-priority enabled duplicate cannot
resurrect it. Selected-only definitions attach only when in the profile, and
excluded definitions never start. Cancelled reviews and source snapshots changed
after review cannot be approved. Preview/approval start no processes, and the
fixture private value stays out of API responses. Child probes also verify
profile command/cwd/environment overrides on the winning user definition, base
environment preservation on a selected-only definition, override provenance only
on winning definitions, and that environment overrides do not enable a disabled
entry. A profile changed between preview and approval must also be rejected.
A compatible `mcp.json` in the daemon working directory names a server included
in the profile. With no explicit project selection, review must fail for the
unknown member without starting it. Removing that member permits review and
attachment of only the user source. After revoking the explicit grant, a new
review without a path must also drop the previous selection while still attaching
the user source. The daemon cwd and fixture are in harness-owned runtime storage; this does not
qualify every editor workspace layout.
The cwd contains a space; argument-string parsing is not qualified by these cases. HTTP header/URL
overrides are exercised separately below. Source and installed-package
jobs retain `mcp-source-precedence.log`. This does not qualify arbitrary editor
layer inheritance, remote endpoints, or all profile override types.

The `mcp-http` scenario serves an ephemeral loopback-only HTTP endpoint for the
real daemon. Preview and approval must produce no HTTP traffic. The endpoint
verifies effective URL/Authorization overrides, preserved base headers, replacement
of configured connection-owned headers, negotiated protocol/session headers,
paginated tools, and manual refresh after optional GET returns 405. It rejects
any request to the base URL or redirect target. A tool redirect must fail without
following it, clear stale tools, and require explicit reconnect without replay.
The fixture observes negotiated-session DELETE on replacement and closure, with
405/404 responses; local closure must still succeed. API responses are scanned
for the private header/path sentinel. The listener is closed before the scenario
returns. A third session serves a real SSE subscription, changes its catalog,
closes the first stream, and requires a resumed GET with the observed event ID.
The daemon must refresh tools in the background, reconcile after reconnection,
keep the same MCP session, avoid replaying a tool call, and close the resumed
stream on session deletion. Both source and installed jobs retain `mcp-http.log`.

This qualifies local HTTP interoperability, not TLS, external providers, or guaranteed remote cleanup under queue saturation or
network failure. HTTP DELETE remains best effort; a server response does not
prove remote resources were destroyed.

## Installed-platform acceptance

The release workflow's `installed Debian X11` matrix downloads the
packages from the same workflow run, checks their SHA-256 manifest, installs the
matching Debian package with apt, and verifies installed package files. Each
job checks that the kernel, Debian host and package architectures agree:
`x86_64`/`amd64` on `ubuntu-24.04`, and `aarch64`/`arm64` on
`ubuntu-24.04-arm`. No emulation or cross-architecture fallback is configured. It runs the
full X11 harness with `bash ci/display/run.sh --installed x11`. This mode uses
only `/usr/bin/hypermesh-visor` and `/usr/bin/hypermesh-companion`; it never builds
or falls back to checkout binaries. Tag publication waits for this job.

Each `installed-x11-<architecture>-<sha>` artifact records the candidate/package and executable
hashes, OS and Xorg package versions, run/attempt, full harness result, and MCP
scenario log. A failed or absent job is not installed-package acceptance.
This exercises Ubuntu 24.04 x86_64 and ARM64 hosted VMs with dummy Xorg and
kernel uinput. Native ARM execution does not qualify Jetson/AGX hardware, GPU
drivers or physical displays. It also does not qualify RPM installation,
graphical-login autostart, native
Windows/macOS, container isolation, or every manual case below. Retain these distinctions in the acceptance report.

Use a disposable OS account and fixture credentials only. Install the candidate
package through its documented installer. Record package checksum, source SHA,
OS/build, desktop/compositor, application version, and enabled permissions. First
verify the platform supports local peer authentication, private trust storage,
focused-app identity, process cleanup, and the physical-user pause gate. If these
are missing, mark the run blocked under #18/#25/#26 or #17; do not substitute the
portable-source test or enable an unauthenticated TCP API.

For each supported platform, perform and record these cases:

| Case | Action | Required observable result |
| --- | --- | --- |
| Source selection | Create a compatible fixture in a documented user location; also create an unselected project config | User source appears with provenance; project source is not imported automatically; neither starts a process |
| Trust | Preview, cancel, then review and approve the selected source | Cancel starts nothing; approval pins the selected source; no private values appear in status/logs |
| Persistence | Close the session and restart the daemon, then attach again | Unchanged approval restores; original source bytes changed afterward require renewed review on restart |
| Identity | Focus the bound app, an unrelated app with the same title, then restart the bound app | Stable platform identity determines matching; title alone does not match; stale identity does not retain routing |
| Precedence | Supply matching rules in explicit order, conflicting source definitions, a disabled entry and an unknown selector | Documented first rule/config precedence applies; disabled/invalid configuration cannot silently enable routing |
| Lifecycle | Refresh, stop the server during a call, then explicitly reconnect | Catalog refresh is atomic; failure clears stale tools/routing; reconnect restores health without replay |
| Revocation | Revoke while a session is attached, then attempt a new session | Existing snapshot behavior is explicit; new attachment is denied without process startup |
| Fallback | Invalidate then repair the binding; stop the server | Diagnostic reason is accurate; healthy MCP preference returns only after recovery; desktop fallback obeys user pause/permissions |
| Cleanup | Close session, exit daemon and companion | Child processes/subscriptions are reaped, runtime credentials remain private, cursor/input state is restored |

Use a local, auditable stdio fixture for process counts and a local HTTP fixture
for negotiated session cleanup. Do not put real credentials, full endpoints,
private headers, environment values or tool payloads into the report. The Linux
fixture server in `ci/display/mcp-discovery.py --server <event-file>` is reusable
with Python on native systems; its X11/Unix-socket driver is Linux-only. Native
transport adapters and focused-app fixtures must exercise the actual native
runtime, not emulate it with Linux or a mocked identity provider.

## Container qualification

Wait for the selected image and deployment contract from #27/#35. Record its
immutable image digest and desktop stack. Launch two bots with separate user
identities, homes, runtime sockets and trust stores. Mount only an explicitly
allowed test resource into both; keep each bot's private source and trust receipt
outside the shared mount. Run the lifecycle cases independently for each bot.
Confirm bot A cannot read/import bot B's private config, credentials or receipts,
connect to B's control interface, or approve a source for B. Restart one bot and
verify the other's session and trust remain unchanged. Verify shared-resource
read/write policy and that changing a shared selected source requires renewed
trust. Do not treat two processes under one host account as container isolation.

## Report and handoff

For every case record `pass`, `fail`, or `blocked`, with a sanitized evidence link,
actual result, and responsible follow-up issue. Include:

- Exact candidate SHA/package checksum or image digest, OS/build and runtime.
- Test date, operator, commands/workflow run URL and attempt.
- Source format and identity mechanism exercised, without private values.
- Trust restart/revocation, lost-call count, fallback and cleanup results.
- Known unsupported behavior and each unexecuted case, explicitly marked.

Review all rows in the acceptance ledger before closing #29. A green Linux run
supports Linux acceptance; it does not satisfy Windows/macOS or container rows.
Keep #29 open until their evidence is linked, or the owner explicitly changes its
scope. Native installers, backends and container deployment are owned by the
linked implementation issues, not claimed as delivered by this procedure.

Runner reference: [GitHub-hosted runners](https://docs.github.com/en/actions/reference/runners/github-hosted-runners).
