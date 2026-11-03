# Native macOS backend work

This is an unsigned development backend, not a completed macOS application.
Signing/notarization is deferred to issue #84. Native companion UI, physical-input
monitoring, packaging and desktop acceptance remain in #18/#26.

On the assigned Apple Silicon Mac:

```sh
cargo build --locked --workspace
swiftc -parse-as-library -O native/macos/Desktop.swift \
  -o target/debug/hypermesh-native-desktop
TMPDIR=/tmp cargo test --locked --workspace
printf '{"action":"info"}\n' | target/debug/hypermesh-native-desktop
```

The daemon finds the helper beside its executable. It uses ScreenCaptureKit for
screenshots, tagged CoreGraphics events for input, and native process identity.
The helper requires the relevant OS permissions; it does not bypass denial.
`--permissions` requests permissions explicitly. Input events are tagged for the
future native companion monitor. Until that monitor exists, do not treat this
backend as ready for unattended desktop automation. The existing companion GUI
still uses X11; compiling it on macOS does not establish native UI support.

Local Unix socket peer authentication uses `getpeereid` on macOS. MCP process
liveness uses owned child handles instead of Linux `/proc`. The native suite
also accounts for BSD sockets inheriting nonblocking mode on accept.

Verified on MACMINI-1 (arm64, macOS 27.2): full workspace tests and native helper
build pass. With permissions absent, screen capture and keyboard requests return
permission errors. The observed foreground application was loginwindow; successful
desktop capture, real input and physical-input pause have not been qualified.
Linux workspace regression tests also pass. Audio capture is not implemented.
