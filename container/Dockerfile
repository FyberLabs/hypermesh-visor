# syntax=docker/dockerfile:1
ARG RUNTIME_BASE=debian:trixie-slim@sha256:f6b08b09a05725f9431502d3b5714f5c4576e82c1a84fa1debe0c24a7e0c3ec4
FROM rust:1.90.0-bookworm@sha256:3914072ca0c3b8aad871db9169a651ccfce30cf58303e5d6f2db16d1d8a7e58f AS toolchain
FROM toolchain AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src src
COPY session session
COPY companion companion
COPY third_party third_party
COPY tests tests
RUN cargo build --locked --release --workspace

FROM ${RUNTIME_BASE} AS desktop
COPY --from=toolchain /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/ca-certificates.crt
ARG DEBIAN_SUITE=trixie
ARG DEBIAN_SNAPSHOT=20261102T000000Z
# Snapshot archives pin dependencies as well as the base. Signature checks stay on.
RUN rm -f /etc/apt/sources.list.d/debian.sources && \
    printf 'deb [check-valid-until=no] https://snapshot.debian.org/archive/debian/%s/ %s main\ndeb [check-valid-until=no] https://snapshot.debian.org/archive/debian-security/%s/ %s-security main\n' \
      "$DEBIAN_SNAPSHOT" "$DEBIAN_SUITE" "$DEBIAN_SNAPSHOT" "$DEBIAN_SUITE" > /etc/apt/sources.list && \
    apt-get -o Acquire::ForceIPv4=true -o Acquire::https::Timeout=30 -o APT::Update::Error-Mode=any -o Acquire::https::CaInfo=/etc/ssl/certs/ca-certificates.crt update && apt-get -o Acquire::ForceIPv4=true -o Acquire::https::Timeout=30 -o APT::Update::Error-Mode=any -o Acquire::https::CaInfo=/etc/ssl/certs/ca-certificates.crt install -y --no-install-recommends \
      ca-certificates dbus-x11 firefox-esr fonts-dejavu-core mousepad openbox \
      python3 python3-tk python3-xlib pulseaudio tini x11-utils xauth xdotool xterm xvfb zenity && \
    dpkg-query -W > /usr/share/visor-packages.txt && rm -rf /var/lib/apt/lists/* && \
    groupadd --gid 2000 resources && useradd --uid 1000 --create-home --shell /bin/bash bot && \
    usermod -a -G resources bot && install -d -o bot -g bot -m 0700 /run/visor /home/bot/.config /home/bot/.local/state /home/bot/workspace
COPY --from=build /src/target/release/hypermesh-visor /src/target/release/hypermesh-companion /src/target/release/hypermesh-desktop-mcp /usr/local/bin/
RUN install -d -o bot -g resources -m 2770 /resources/share
COPY container/entrypoint.py /usr/local/bin/visor-container
COPY container/resource.py /usr/local/bin/visor-resource
COPY container/health.py /usr/local/bin/visor-health
RUN chmod 0755 /usr/local/bin/visor-container /usr/local/bin/visor-health /usr/local/bin/visor-resource
ENV HOME=/home/bot DISPLAY=:99 XAUTHORITY=/run/visor/Xauthority XDG_RUNTIME_DIR=/run/visor \
    XDG_CONFIG_HOME=/home/bot/.config XDG_STATE_HOME=/home/bot/.local/state \
    HYPERMESH_RUNTIME_DIR=/run/visor/hypermesh HYPERMESH_CONFIG_DIR=/home/bot/.config/hypermesh \
    PULSE_SERVER=unix:/run/visor/pulse/native
USER 1000:1000
WORKDIR /home/bot/workspace
HEALTHCHECK --interval=5s --timeout=3s --start-period=30s --retries=3 CMD ["visor-health"]
ENTRYPOINT ["/usr/bin/tini", "--", "/usr/bin/dbus-run-session", "--", "/usr/local/bin/visor-container"]
