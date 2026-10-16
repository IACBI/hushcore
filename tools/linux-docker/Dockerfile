# Headless Linux test image for the HushCore engine's PulseAudio-protocol backend.
# Base is ubuntu:24.04 (see run.sh). Rust comes from Ubuntu's versioned packages: the engine's locked
# crates need a toolchain newer than the default rustc in noble (1.75).
ARG BASE=ubuntu:24.04
FROM ${BASE}

ENV DEBIAN_FRONTEND=noninteractive \
    LANG=C.UTF-8 \
    PATH=/usr/lib/rust-1.91/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin \
    CARGO_HOME=/cargo \
    CARGO_TARGET_DIR=/target

RUN apt-get update \
 && apt-get install -y --no-install-recommends \
        rustc-1.91 cargo-1.91 rust-1.91-clippy \
        build-essential pkg-config libpulse-dev \
        pipewire pipewire-bin pipewire-pulse wireplumber \
        pulseaudio-utils dbus-daemon dbus-user-session \
        python3 python3-numpy shellcheck procps ca-certificates passwd \
 && rm -rf /var/lib/apt/lists/*

# Unprivileged user: PipeWire and WirePlumber refuse to run as root in practice, and the engine runs
# as a normal session user on real systems.
RUN useradd --create-home --uid 1001 hush \
 && mkdir -p /cargo /target \
 && chown hush:hush /cargo /target

USER hush
WORKDIR /home/hush
