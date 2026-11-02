# Installed Debian/X11 qualification record

Recorded 2026-11-02 by Codex from GitHub Actions and downloaded evidence.
Candidate: merged main commit `b08fffabe0fd5ccf359085af56d9aa06501d33c6`
(PR #76). This is a CI package qualification, not a published release or a
claim that every #29 acceptance criterion is complete.

## Candidate and execution

Both native hosted VMs ran Ubuntu 24.04.5 LTS, Xorg
`2:21.1.12-1ubuntu1.9`, dummy video and kernel uinput with libinput. Package
version: `0.1.0~ci.b08fffabe0fd-1`.

[Release run 41475250190, attempt 1](https://github.com/FyberLabs/hypermesh-visor/actions/runs/41475250190)
completed successfully, including packaging and both installed-package jobs.
Publication was skipped because this was a main-branch run, not a release tag.

| Native architecture | Installed job | Evidence artifact | Result |
| --- | --- | --- | --- |
| x86_64 / amd64 | [125382363644](https://github.com/FyberLabs/hypermesh-visor/actions/runs/41475250190/job/125382363644) | [13156488811](https://github.com/FyberLabs/hypermesh-visor/actions/runs/41475250190/artifacts/13156488811) | PASS |
| aarch64 / arm64 | [125382363536](https://github.com/FyberLabs/hypermesh-visor/actions/runs/41475250190/job/125382363536) | [13157467675](https://github.com/FyberLabs/hypermesh-visor/actions/runs/41475250190/artifacts/13157467675) | PASS |

Artifact names are `installed-x11-<architecture>-<full candidate SHA>`.
The evidence artifacts were downloaded and inspected. Sanitized verbatim
excerpts are retained in [x86_64.txt](x86_64.txt) and [aarch64.txt](aarch64.txt):
candidate identity, package/executable SHA-256 values, MCP scenario PASS lines,
and complete-harness cleanup results. Temporary runtime paths and unrelated OS
metadata were omitted. These excerpts are not substitutes for the full run logs.

| Debian asset | SHA-256 |
| --- | --- |
| hypermesh-visor-x86_64.deb | `526984d6a2a8610c7c2101cc718f21f1ea8f7276b076717d35a1dcfc69346373` |
| hypermesh-visor-aarch64.deb | `6c567fa1c243137630337358c1adfcd36255fb3e7f44f193a73c669b6b5e59ad` |

## Observed qualification

These cases passed on **both** installed native packages:

- Host/kernel/package architectures agreed; the downloaded package checksum
  manifest and installed package files verified successfully.
- The full X11 harness executed `/usr/bin` binaries without a source-build
  fallback, including physical-input pause and cross-user access checks.
- Selected-source preview/approval did not start a server; paginated discovery,
  exact executable matching, refresh without process restart, denial before I/O,
  lost-reply fallback and explicit reconnect without replay passed.
- Invalid bindings cleared routing; corrected bindings recovered it. Revocation
  blocked a new attachment while preserving the existing session snapshot.
- Three real daemon restarts verified unchanged approval restoration,
  comment-only source-change denial, reapproval, durable revocation and private
  secret-free receipts. Sessions were closed before each restart.
- Cleanup reported no remaining harness process or listening TCP socket;
  `/dev/uinput` remained `600 root:root`, unchanged and not world-writable.

Supporting workflows on the same merged SHA also passed:
[tests and native source matrix](https://github.com/FyberLabs/hypermesh-visor/actions/runs/41475250118),
[source-built display](https://github.com/FyberLabs/hypermesh-visor/actions/runs/41475250120),
and [verbs](https://github.com/FyberLabs/hypermesh-visor/actions/runs/41475250123).
Windows/macOS source tests do not qualify native desktop runtimes. Wayland
refusal tests do not qualify Wayland automation.

## Outstanding qualification

| Scope | Status | Required next evidence / ownership |
| --- | --- | --- |
| Installed Debian/X11 on native hosted x86_64 and ARM64 | PASS within cases above | Re-run for changed release candidates; retain exact hashes |
| Jetson/AGX, GPU drivers, physical displays | NOT RUN | Device-specific package, driver and desktop acceptance |
| RPM installation/runtime | NOT RUN | Native RPM-family distribution and installed-package suite |
| Graphical-login autostart | NOT RUN | Actual desktop login executes packaged autostart entry |
| Crash with an in-flight call/session | NOT RUN | Crash cleanup/recovery evidence; no replay |
| Remaining installed-platform manual cases | NOT RUN as a complete matrix | Execute all cases in the qualification procedure, including title-collision/app restart and source precedence scenarios |
| Native Windows/macOS runtime | BLOCKED | Backends/installers and runtime acceptance: #18/#25/#26 |
| Native Wayland automation | BLOCKED | Portal/libei backend and compositor acceptance: #17 |
| Two isolated containers and shared resources | BLOCKED | Image/deployment contract and isolation acceptance: #27/#35 |

#29 remains open. This record is evidence for the stated candidate and cases;
it does not waive the remaining gates or silently narrow the original scope.
Use the [qualification procedure](../../mcp-discovery-qualification.md) and
[acceptance ledger](../../mcp-discovery-acceptance.md) for subsequent runs.
