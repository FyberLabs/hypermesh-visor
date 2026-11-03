# Private desktop bots

Each bot has its own X11 desktop, audio server, daemon, companion, MCP configuration,
identity and persistent home. No host display, input device, Docker socket or network
is exposed. This example gives one bot write access and another read access to a
bounded local exchange volume.

```sh
docker build -f container/Dockerfile -t visor-container:local .
export VISOR_IMAGE=$(docker image inspect visor-container:local --format '{{.Id}}')
docker compose -p visor -f container/compose.yaml up -d

docker compose -p visor -f container/compose.yaml exec -T writer \
  visor-resource put share example.txt --expected missing <<'TEXT'
Hello from the writer.
TEXT
docker compose -p visor -f container/compose.yaml exec -T reader \
  visor-resource get share example.txt

docker compose -p visor -f container/compose.yaml down
```

Use an immutable registry digest when distributing an image. The local build above
uses its immutable local image ID. The daemon's control socket is private to its
container; run clients using `docker compose exec -T`. The inverse MCP executable
is `/usr/local/bin/hypermesh-desktop-mcp`. The Go CLI is not bundled yet.

Homes survive container recreation. The shared exchange is **ephemeral**, limited
to 256 MiB, and may disappear when unmounted. For durable sharing, provision a
separate quota-enforced external volume. Never share a bot's home, credential
store or control socket. Do not use `down -v` unless intentionally deleting state.

`visor-resource` accepts flat filenames, refuses symlinks and hard links, and
publishes atomically with `--expected missing` or the previous SHA-256 for conflict
detection. Files are limited to 16 MiB; cooperative publishers share a lock and a
240 MiB budget. The mount's 256 MiB quota remains the enforcement boundary even
when a writer bypasses the helper. Read-only grants are enforced by the mount.
An authorized writer can replace or delete shared files. Share only with trusted
writers. Interrupted temporary publications count against quota; inspect and
remove them explicitly rather than deleting unrelated resources.

Networking is off by default. Enabling network access is an operator decision;
apply an egress policy appropriate to the apps and MCP services in use. Local
HTTP/SSE fixtures work over each container's own loopback interface.

## Qualification

Use a fresh project name, then run:

```sh
python3 ci/container/qualify.py visor --image "$VISOR_IMAGE" --output /tmp/visor-results
docker cp ci/container/apps.py visor-writer-1:/home/bot/qualification/apps.py
docker exec visor-writer-1 python3 /home/bot/qualification/apps.py
```

The first command checks two independent bots, read-only grants, atomic conflicts,
path attacks, hard quota, MCP discovery/HTTP/SSE and persistent trust through
restarts. The second drives real Firefox, Mousepad and xterm using Visor input
and captures the private display. Tests leave the project and its state available
for inspection. Stop containers with `compose down`; retain volumes unless their
deletion is intended.

See [the base decision](../docs/container-base.md). Host physical input and native
Wayland automation are separate from this private X11 container environment.
