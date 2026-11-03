# Container desktop base decision

Select **Debian 13 trixie-slim with private Xvfb/Openbox**, Firefox ESR, Mousepad,
and xterm for #27. Debian 12 bookworm was the executable comparison candidate.
Both passed the same real browser form submission, editor save, terminal input,
screenshot, two-bot file sharing, MCP discovery/HTTP/SSE and trust-restart checks.

“AI-friendly” here means observable app identity, repeatable input and output,
working MCP attachment, clear permission failures and recoverable private state.
It does not imply a distribution understands or trusts model-generated commands.

## Measured prototype

Local Docker 29.8.1, Linux amd64; dependency snapshot 2026-11-02. Image sizes are
uncompressed local sizes. Full package inventories, image IDs and check results
are retained in [the qualification record](qualification/2026-11-02-container/).

| Base | Image size | Firefox ESR | Mousepad | xterm |
| --- | --- | --- | --- | --- |
| trixie | 982.7 MiB | 153.6.0esr-1~deb13u1 | 0.6.3-1 | 398-1 |
| bookworm | 1104.6 MiB | 153.6.0esr-1~deb12u2 | 0.5.10-2 | 379-1 |

After application tests, observed writer memory was 247.1 MiB for trixie and
134.4 MiB for bookworm, under a 2 GiB limit; CPU snapshots were 3.46% and 2.97%.
These are single observations with different cache histories, not benchmarks.
Readiness timings in JSON measure wait from the test starting, not cold startup.

Trixie provides the current stable Debian package baseline and a smaller image
in this comparison. Bookworm also works but begins with an older package set.
The base manifests support amd64 and arm64; native ARM qualification is performed
by the new container workflow, not inferred from the amd64 results here.

## Other candidates and tradeoffs

[Omarchy](https://omarchy.org/) is a machine-oriented Arch/Hyprland desktop.
Its Wayland integration would depend on Visor's unfinished native Wayland work
(#17). It was evaluated for architecture fit, not run as a container benchmark.
Installing a full opinionated machine environment adds maintenance and system
service assumptions without improving this private X11 automation path. Revisit
it when native Wayland automation is qualified. Ubuntu is another viable base,
but was not part of the measured shortlist.

X11 is confined inside each bot. There is no host display/input/socket mount,
privileged mode, external port or default network. Separate homes persist bot
identity and trust. A bounded exchange volume grants writer/read-only access.
This demonstrates container behavior; it does not qualify host physical-input
pause, native Wayland, GPU acceleration, audio capture quality or accessibility
semantics across arbitrary third-party apps. MCP fixtures demonstrate the
transport and trust behavior, not universal app support.

## Reproduction and maintenance

Follow [container/README.md](../container/README.md). Dockerfile pins base and Rust
manifest digests, Cargo.lock, and Debian snapshot packages; signature and HTTPS
verification remain enabled. Keep OS copyright/license files in the image and
retain the package inventory. Debian main packages have varied licenses; audit
any additional app before redistribution.

Refresh the base digest and snapshot at least monthly and promptly for relevant
security fixes. Rebuild and run the same native amd64/arm64 qualifications before
promoting a new immutable image digest. Preserve previous digests for rollback.
Pinned snapshots do not receive security fixes automatically. Debian release
support is described by [Debian](https://www.debian.org/releases/); historical
package reproduction uses [Debian Snapshot](https://snapshot.debian.org/).
