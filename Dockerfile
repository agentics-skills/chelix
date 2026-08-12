# Multi-stage Dockerfile for chelix
# Builds a minimal debian-based image with the chelix gateway
#
# Chelix uses Docker/Podman for sandboxed command execution. To enable this,
# mount the container runtime socket when running:
#
#   Docker:    -v /var/run/docker.sock:/var/run/docker.sock
#   Podman:    -v /run/podman/podman.sock:/var/run/docker.sock
#   OrbStack:  -v /var/run/docker.sock:/var/run/docker.sock (same as Docker)
#
# See README.md for detailed instructions.

# Rust build software. This stage changes only when the base image or installed
# system packages change, so source edits do not invalidate it.
FROM rust:bookworm AS rust-system

ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update -qq && \
    apt-get install -yqq --no-install-recommends \
        build-essential \
        ca-certificates \
        cmake \
        git \
        libclang-dev \
        pkg-config && \
    rm -rf /var/lib/apt/lists/*

WORKDIR /build

RUN mkdir -p .cargo && \
    { echo '[unstable]'; echo 'cargo-lints = true'; } > .cargo/config.toml

# The pinned nightly is isolated from source and dependency changes.
FROM rust-system AS rust-toolchain

COPY rust-toolchain.toml ./
RUN NIGHTLY="$(sed -nE 's/^channel[[:space:]]*=[[:space:]]*"([^"]+)"/\1/p' rust-toolchain.toml)" \
    && rustup install "$NIGHTLY" && rustup default "$NIGHTLY"
RUN cargo +nightly-2026-07-30 install cargo-chef --version 0.1.77 --locked

# The recipe contains only Cargo manifests and the lockfile. Source changes may
# rerun the planner, but they do not change the dependency-layer input.
FROM rust-toolchain AS cargo-planner

COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
COPY apps/courier ./apps/courier

RUN cargo +nightly-2026-07-30 chef prepare --recipe-path recipe.json

# Download locked registry and Git dependencies before application sources are
# copied into the build stage.
FROM rust-toolchain AS cargo-dependencies

COPY --from=cargo-planner /build/recipe.json ./recipe.json
RUN cargo +nightly-2026-07-30 chef cook --recipe-path recipe.json --no-build && \
    cargo +nightly-2026-07-30 fetch --locked

# Frontend dependencies are cached solely by the npm manifests.
FROM node:22-bookworm-slim AS web-dependencies

WORKDIR /build/crates/web/ui

COPY crates/web/ui/package.json crates/web/ui/package-lock.json ./
RUN npm ci --ignore-scripts

# Frontend sources invalidate only the web build branch.
FROM web-dependencies AS web-builder

COPY crates/web/ui/input.css crates/web/ui/tsconfig.json crates/web/ui/vite.config.ts ./
COPY crates/web/ui/src ./src
COPY crates/web/src/assets /build/crates/web/src/assets

RUN npm run build:all

# Rust application build. Toolchains and frontend dependencies are inherited
# from independently cacheable stages above.
FROM cargo-dependencies AS rust-builder

COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
COPY apps/courier ./apps/courier
COPY scripts ./scripts

# docs/src is embedded into chelix-agents via include_dir! (crates/agents/src/docs.rs).
# CHANGELOG.md is the target of the docs/src/changelog.md symlink, so it must be
# present at the repo root for that symlink to resolve during the embed.
COPY CHANGELOG.md ./CHANGELOG.md
COPY docs/src ./docs/src

# Replace any generated assets from the build context with the independently
# built frontend output before include_dir! embeds it in the release binary.
COPY --from=web-builder /build/crates/web/src/assets ./crates/web/src/assets

ARG CHELIX_VERSION
ENV CHELIX_VERSION=${CHELIX_VERSION}
RUN CHELIX_BUILD_FEATURES="full,embedded-assets" ./scripts/cargo-build-chelix.sh --release

# Runtime software is independent from all application build stages. The
# official Node image replaces the repeated NodeSource repository bootstrap.
FROM node:22-bookworm-slim AS runtime-base

ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update -qq && \
    apt-get install -yqq --no-install-recommends \
        ca-certificates \
        chromium \
        curl \
        gnupg \
        libgomp1 \
        ripgrep \
        sudo \
        vim-tiny && \
    rm -rf /var/lib/apt/lists/*

# Install Docker CLI for sandbox execution (talks to mounted socket, no daemon in-container)
RUN install -m 0755 -d /etc/apt/keyrings && \
    curl -fsSL https://download.docker.com/linux/debian/gpg \
        | gpg --dearmor -o /etc/apt/keyrings/docker.gpg && \
    chmod a+r /etc/apt/keyrings/docker.gpg && \
    echo "deb [arch=$(dpkg --print-architecture) signed-by=/etc/apt/keyrings/docker.gpg] https://download.docker.com/linux/debian $(. /etc/os-release && echo \"$VERSION_CODENAME\") stable" \
        > /etc/apt/sources.list.d/docker.list && \
    apt-get update -qq && \
    apt-get install -yqq --no-install-recommends \
        docker-buildx-plugin \
        docker-ce-cli && \
    rm -rf /var/lib/apt/lists/*

# Create non-root user and add to docker group for socket access.
# Grant passwordless sudo so chelix can install host packages at startup.
RUN groupadd -f docker && \
    useradd --create-home --user-group chelix && \
    usermod -aG docker chelix && \
    echo "chelix ALL=(ALL) NOPASSWD:ALL" > /etc/sudoers.d/chelix

# Application changes invalidate only this final image assembly stage.
FROM runtime-base AS runtime

# Copy the core binary and its managed sidecars from builder
COPY --from=rust-builder /build/target/release/chelix /usr/local/bin/chelix
COPY --from=rust-builder /build/target/release/chelix-tools-service /usr/local/bin/chelix-tools-service
COPY --from=rust-builder /build/target/release/chelix-embedding-service /usr/local/bin/chelix-embedding-service

# Create config and data directories
RUN mkdir -p /home/chelix/.config/chelix /home/chelix/.chelix /home/chelix/.npm && \
    chown -R chelix:chelix /home/chelix/.config /home/chelix/.chelix /home/chelix/.npm

# Volume mount points for persistence and container runtime
VOLUME ["/home/chelix/.config/chelix", "/home/chelix/.chelix", "/home/chelix/.npm", "/var/run/docker.sock"]

USER root

# Expose gateway port (HTTPS) and HTTP port for CA certificate download (gateway port + 1).
# EXPOSE 13131 13132

# Bind 0.0.0.0 so Docker port forwarding works (localhost only binds to
# the container's loopback, making the port unreachable from the host).
ENTRYPOINT ["chelix"]
CMD ["--bind", "0.0.0.0", "--port", "13131"]
