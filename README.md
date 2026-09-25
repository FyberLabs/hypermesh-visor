# hypermesh-visor

`hypermesh-visor` is a local Linux daemon a supervisor starts for a desktop session. Open a session, then call view, watch, listen, mouse, and type. Calls without a live session fail. It is not a virtual machine. This version is Linux only.

Purpose, recipes, agent, skills, and secrets are sent when the session opens. Only secrets included in that call are loaded. URL, MCP, chain, and IPFS sources are part of the session call and are not fetched.

## Linux build and tests

Stable Rust. Tests do not need a display server.

```bash
cargo test
cargo build --release
./target/release/hypermesh-visor --listen 127.0.0.1:9847
```

`--listen` must be a loopback address. Run the daemon as the logged-in user, not as root. It prints `listening 127.0.0.1:9847` when the socket is up.

## Release packages

Pull requests and `main` build Linux artifacts. Those builds are not a release. Their version is the `Cargo.toml` version plus `~ci.` and the commit. A release is a tag named `v` plus that same `Cargo.toml` version, pushed after the version is on `main`. The workflow refuses a tag that does not match.

```bash
git tag v0.1.0
git push origin v0.1.0
```

`v0.1.0` publishes these files:

- `hypermesh-visor_0.1.0_linux_amd64` — x86_64 binary
- `hypermesh-visor_0.1.0_linux_arm64` — aarch64 binary, including the AGX
- `hypermesh-visor_0.1.0_amd64.deb`
- `hypermesh-visor_0.1.0_arm64.deb`
- `hypermesh-visor-0.1.0-1.x86_64.rpm`
- `hypermesh-visor-0.1.0-1.aarch64.rpm`
- `checksums.txt` — SHA-256 of those files

The packages install `/usr/bin/hypermesh-visor` and do not start a service. Run that binary as the logged-in user. Builds run on Ubuntu 24.04, the same userspace generation as the AGX JetPack 7 image.

```bash
curl -s -X POST http://127.0.0.1:9847/session \
  -H 'content-type: application/json' \
  -d '{"purpose":"look at the desktop","recipes":[],"agent":{"name":"local","instructions":""},"skills":[]}'
```
