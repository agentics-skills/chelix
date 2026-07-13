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

# Build stage — nightly required for wacore-binary (portable_simd)
FROM rust:bookworm AS builder

WORKDIR /build

# Copy rust-toolchain.toml first so the nightly pin is defined in one place.
COPY rust-toolchain.toml ./
RUN NIGHTLY="$(sed -nE 's/^channel[[:space:]]*=[[:space:]]*"([^"]+)"/\1/p' rust-toolchain.toml)" \
    && rustup install "$NIGHTLY" && rustup default "$NIGHTLY"

# Copy manifests first for better caching
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
COPY apps/courier ./apps/courier
COPY scripts ./scripts
COPY wit ./wit
# docs/src is embedded into chelix-agents via include_dir! (crates/agents/src/docs.rs).
# CHANGELOG.md is the target of the docs/src/changelog.md symlink, so it must be
# present at the repo root for that symlink to resolve during the embed.
COPY CHANGELOG.md ./CHANGELOG.md
COPY docs/src ./docs/src

ENV DEBIAN_FRONTEND=noninteractive
# Install build dependencies used only by the separately built embedding sidecar
RUN apt-get update -qq && \
    apt-get install -yqq --no-install-recommends cmake build-essential libclang-dev pkg-config git && \
    rm -rf /var/lib/apt/lists/*

# Install Node.js for Vite/esbuild builds (web assets are gitignored)
RUN apt-get update -qq && \
    apt-get install -yqq --no-install-recommends ca-certificates curl gnupg && \
    curl -fsSL https://deb.nodesource.com/setup_22.x | bash - && \
    apt-get install -yqq --no-install-recommends nodejs && \
    rm -rf /var/lib/apt/lists/*

# Build all web assets (Vite JS + Tailwind CSS + service worker)
RUN ARCH=$(uname -m) && \
    case "$ARCH" in x86_64) TW="tailwindcss-linux-x64";; aarch64) TW="tailwindcss-linux-arm64";; esac && \
    curl -sLO "https://github.com/tailwindlabs/tailwindcss/releases/latest/download/$TW" && \
    chmod +x "$TW" && \
    TAILWINDCSS="./$TW" ./scripts/build-web-assets.sh

# Install WASM target and build WASM components (embedded via include_bytes!)
RUN rustup target add wasm32-wasip2 && \
    cargo build --target wasm32-wasip2 -p chelix-wasm-calc -p chelix-wasm-web-fetch -p chelix-wasm-web-search --release

# Build release binary with the same portable production feature set used by
# release/package builds.
ARG CHELIX_VERSION
ENV CHELIX_VERSION=${CHELIX_VERSION}
RUN ./scripts/cargo-build-chelix.sh --release

# Runtime stage
FROM debian:bookworm-slim

# Install base runtime dependencies
ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update -qq && \
    apt-get install -yqq --no-install-recommends \
        ca-certificates \
        chromium \
        curl \
        gnupg \
        libgomp1 \
        sudo \
        tmux \
        vim-tiny && \
    rm -rf /var/lib/apt/lists/*

# Install Node.js 22 LTS via NodeSource (npm/npx bundled) for stdio-based MCP servers
RUN install -m 0755 -d /etc/apt/keyrings && \
    curl -fsSL https://deb.nodesource.com/gpgkey/nodesource-repo.gpg.key \
        | gpg --dearmor -o /etc/apt/keyrings/nodesource.gpg && \
    echo "deb [signed-by=/etc/apt/keyrings/nodesource.gpg] https://deb.nodesource.com/node_22.x nodistro main" \
        > /etc/apt/sources.list.d/nodesource.list && \
    apt-get update -qq && \
    apt-get install -yqq --no-install-recommends nodejs && \
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

# Copy the core binary and its managed local-embedding sidecar from builder
COPY --from=builder /build/target/release/chelix /usr/local/bin/chelix
COPY --from=builder /build/target/release/chelix-embedding-service /usr/local/bin/chelix-embedding-service
COPY --from=builder /build/crates/web/src/assets /usr/share/chelix/web
COPY --from=builder /build/target/wasm32-wasip2/release/chelix_wasm_calc.wasm /usr/share/chelix/wasm/
COPY --from=builder /build/target/wasm32-wasip2/release/chelix_wasm_web_fetch.wasm /usr/share/chelix/wasm/
COPY --from=builder /build/target/wasm32-wasip2/release/chelix_wasm_web_search.wasm /usr/share/chelix/wasm/

# Create config and data directories
RUN mkdir -p /home/chelix/.config/chelix /home/chelix/.chelix /home/chelix/.npm && \
    chown -R chelix:chelix /home/chelix/.config /home/chelix/.chelix /home/chelix/.npm

# Volume mount points for persistence and container runtime
VOLUME ["/home/chelix/.config/chelix", "/home/chelix/.chelix", "/home/chelix/.npm", "/var/run/docker.sock"]

USER root

# Expose gateway port (HTTPS), HTTP port for CA certificate download (gateway port + 1),
# and OAuth callback port (used by providers with pre-registered redirect URIs).
# EXPOSE 13131 13132 1455

# Bind 0.0.0.0 so Docker port forwarding works (localhost only binds to
# the container's loopback, making the port unreachable from the host).
ENTRYPOINT ["chelix"]
CMD ["--bind", "0.0.0.0", "--port", "13131"]
