# Native platform acceptance readiness — 2026-11-02

This is a readiness observation, not native desktop acceptance. The tested
repository baseline is merged revision
`cbd7d6e575d9999ab34992d6152d5b7987d66c07` (PR #82).

| Target | Observed access/environment | Result |
| --- | --- | --- |
| Assigned Windows host | Native PowerShell 5.1; Windows build 26300; x64; native commands executable from WSL | Host access available; native Visor runtime qualification blocked by #18/#25 |
| Assigned Apple Silicon host | SSH available; macOS 27.2 build 26B123; arm64; console session present; Xcode Command Line Tools selected | Host access available; native Visor runtime qualification blocked by #18/#26 |
| Linux container host | Docker Engine 29.8.1, Linux x86_64 accessible | Engine available; bot image/deployment/base qualification blocked by #27/#35 |

No user's applications, credentials, trust settings, or desktop permissions were
changed for these observations. Machine addresses and account names are omitted.
A console session alone does not establish that permissions are granted or that
an operator has performed a real-input pause test.

## Build and runtime blockers

- `src/lib.rs` explicitly rejects non-Linux builds. `companion/src/main.rs` also
  rejects non-Linux builds. Removing these checks does not implement a port.
- Local daemon/companion authentication uses Linux `SO_PEERCRED`; the runtime
  needs native authenticated local IPC and private trust storage before desktop
  operations can be admitted on Windows or macOS.
- The desktop implementation provides Linux X11/Wayland backends. The native
  focused-app probes, capture/input, physical-user pause gate, permission recovery,
  and companion presentation remain prerequisites under #18/#25/#26.
- No container image or deployment contract is present in this baseline. Docker
  availability does not establish per-bot credentials, private runtime namespaces,
  controlled shared resources, or virtual-desktop behavior.

The native source resolver CI runs on Windows and macOS, but intentionally
compiles an isolated source-enumeration crate. It is not an installed daemon or
companion. Neither green source checks nor an accessible machine can satisfy
native runtime acceptance.

## Required qualification after implementation

Use the available Windows and Apple Silicon hosts to install the actual native
candidate, record its checksum and source revision, and run every applicable row
in [the qualification procedure](../mcp-discovery-qualification.md). Exercise
visible permission denial/recovery and real-input pause with an operator; record
capture, focused identity, trust persistence, MCP lifecycle and cleanup results.
Signed/notarized distribution and installer acceptance belong to #25/#26 and
must not be inferred from an unsigned development process.

For #27/#35, qualify the selected immutable container image on two isolated bots
with distinct identities/trust stores and one explicitly shared resource. Verify
cross-bot denial, restart isolation, resource grants and cleanup using the actual
deployment. Do not replace this with two processes sharing one host account.

#29 remains open until these results exist or its owner explicitly changes scope.
