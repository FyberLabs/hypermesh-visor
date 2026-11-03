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

For a visible, installed permission setup app, run:

```sh
bash native/macos/install-development.sh
```

This installs `~/Applications/Hypermesh Visor Development.app` and opens its
permission setup window. Use its buttons to request Screen Recording,
Accessibility and Input Monitoring, then refresh status. It uses local ad-hoc
signing only, not Developer ID signing or notarization. The installer refuses to
overwrite an existing bundle. Replacing the development executable can require
permissions to be granted again. To uninstall, quit the development app and remove
that bundle; revoke its entries in System Settings if desired.

The installed app is a permission setup app only, not the finished companion.
**Desktop bridge invocation is disabled**, including `--bridge`, arbitrary
arguments and the old `HYPERMESH_NATIVE_BRIDGE` environment switch. Granting TCC
permissions to an app must not turn it into an automation deputy for any process
that can invoke `open --stdin/--stdout`. An authenticated daemon-owned channel
is required before capture or input is enabled again. Same UID alone does not
establish that authority. The daemon receives an unavailable error today.

The event construction code uses explicit US ANSI virtual key identities for
modifier shortcuts; unsupported Unicode shortcuts fail rather than aliasing A.
Unmodified Unicode text uses Unicode events. Left, right and middle drags select
both the matching drag event type and button identity. Mapping tests construct
events without posting input:

```sh
target/debug/hypermesh-native-desktop --self-test
python3 native/macos/test-bridge-refusal.py target/debug/hypermesh-native-desktop
```

`--permissions` requests OS permission explicitly. Native companion monitoring
and authenticated integration remain unfinished. The companion GUI still uses
X11; compiling it on macOS does not establish native UI support.

Local Unix socket peer authentication uses `getpeereid` on macOS. MCP process
liveness uses owned child handles instead of Linux `/proc`. The native suite
also accounts for BSD sockets inheriting nonblocking mode on accept.

Verified on MACMINI-1: native event mapping and bridge refusal tests pass.
The earlier direct helper qualification is historical and does not establish
secure daemon integration; that unauthenticated entry point has been removed.
Audio capture is not implemented.
