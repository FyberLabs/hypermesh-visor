# Installed MCP discovery qualification — consolidated candidate

Recorded 2026-11-02 from downloaded GitHub Actions artifacts. Candidate
`a70e6fdbd7b9f7c737fbdf911b6a09c8a2ce56dd` is the PR #83 merge candidate
for head `b3da994031adbf74533182383ad5c34a218fb381` against main
`cbd7d6e575d9999ab34992d6152d5b7987d66c07`. This records that immutable
candidate, not the later documentation commit retaining this report.

[Run 41549877673, attempt 1](https://github.com/FyberLabs/hypermesh-visor/actions/runs/41549877673)
passed packaging and both installed-package jobs. Each runner installed package
`0.1.0~ci.a70e6fdbd7b9-1`, verified its files, and exercised `/usr/bin` binaries
without source fallback. Both were Ubuntu 24.04.5 with Xorg
`2:21.1.12-1ubuntu1.9`, dummy video, and kernel uinput/libinput. This is not a
published release, physical-display qualification, or complete #29 acceptance.

| Native platform | Job | Artifact | Debian SHA-256 |
| --- | --- | --- | --- |
| x86_64 / amd64 | [125636562545](https://github.com/FyberLabs/hypermesh-visor/actions/runs/41549877673/job/125636562545) | [13190443172](https://github.com/FyberLabs/hypermesh-visor/actions/runs/41549877673/artifacts/13190443172) | `c37a7ac647c27f76f1a2b7797af38178b5b44a7fc82ca04648024a8e4d1a62e0` |
| aarch64 / arm64 | [125636562638](https://github.com/FyberLabs/hypermesh-visor/actions/runs/41549877673/job/125636562638) | [13191211341](https://github.com/FyberLabs/hypermesh-visor/actions/runs/41549877673/artifacts/13191211341) | `4ac9851b161eed59c2d02e67f9f882b5e9a9df424227510d0ad17f3f7c495edc` |

Downloaded artifacts were inspected. Sanitized verbatim excerpts in
[x86_64.txt](x86_64.txt) and [aarch64.txt](aarch64.txt) retain package/binary hashes,
all MCP scenario PASS lines, and complete-harness cleanup. Unrelated OS metadata
and temporary paths are omitted. The source display run
[41549877662](https://github.com/FyberLabs/hypermesh-visor/actions/runs/41549877662)
also passed.

## Observed result on both installed packages

| Case | Result and boundary |
| --- | --- |
| Source selection and trust | PASS: no startup before approval/attachment, source precedence, disabled/excluded entries, cancellation and changed-snapshot rejection, project opt-in, no retained prior project selection |
| Effective profile overrides | PASS: command, cwd containing a space, environment, URL and HTTP authorization; winning-source provenance and private-value redaction |
| Identity and precedence | PASS: exact executable, same-title collision refusal, app exit/restart, ordered healthy bindings, unavailable target skipped, all selectors required |
| Stdio lifecycle | PASS: pagination, refresh, failed-call fallback, explicit reconnect without replay, invalid-binding recovery, revocation behavior |
| HTTP lifecycle | PASS: negotiated version/session headers, connection-owned header replacement, pagination, GET 405/manual refresh, denied-call isolation, redirect refusal, stale catalog clearing, explicit reconnect, no replay, DELETE cleanup with 405/404 responses |
| HTTP subscriptions | PASS: background list-change refresh, GET resumption with event ID, reconciliation without reinitialization or call replay; stream closure was not independently established (see correction below) |
| Durable trust | PASS: unchanged restoration, changed-source denial, reapproval, revocation across real daemon restarts, private receipt |
| In-flight daemon crash | PASS: failed delivery, pipe-aware child exit, old session absent after restart, explicit new session, restored trust, no replay |
| Whole harness and cleanup | PASS: all display scenarios, cross-user denial, physical-input pause through uinput, no remaining harness processes/listeners, unchanged `600 root:root` device permissions |

HTTP results use an ephemeral loopback fixture, not TLS or an external provider.
DELETE is best effort; observing it does not prove remote resource destruction.
Crash cleanup covers a pipe-aware child, not uncooperative/detached descendants.
X11 identity remains a local routing hint, not authenticated app attestation.

## Stream-closure evidence correction

PR #83 review found that this candidate's fixture ended its own SSE response
when DELETE arrived. Its retained `stream-closes` PASS text therefore does not
prove client disconnection. Other assertions are unchanged. The corrected
fixture holds the resumed stream open after DELETE and requires client EOF/reset
within 20 seconds, independently of teardown. The corrected scenario passed
against the real daemon in the isolated local desktop container; new installed
CI evidence must come from the corrected candidate rather than these old logs.

## Acceptance disposition

The exercised installed Debian/X11 discovery cases pass. The original
cross-platform #29 acceptance is **BLOCKED**, not complete:

- Windows and macOS source checks pass, but native daemon/companion runtimes,
  authenticated IPC, desktop backends and permission/real-input qualification
  remain under #18/#25/#26. Assigned hosts are available; see the
  [readiness observations](../2026-11-02-platform-readiness.md).
- Container base/image and two-bot isolation/shared-resource qualification remain
  under #27/#35. A working Docker engine does not establish deployment acceptance.
- Wayland automation remains refused pending #17.
- RPM, graphical-login autostart, physical displays/Jetson/GPU hardware, external
  HTTP providers and other unexecuted platform variants are not qualified here.

Do not close #29 or infer native runtime acceptance from the source-only matrix.
Use the [acceptance ledger](../../mcp-discovery-acceptance.md) and
[qualification procedure](../../mcp-discovery-qualification.md) for remaining work.
