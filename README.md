<div align="center">

# Chelix — A secure persistent personal agent server in Rust

One core binary — sandboxed, secure, yours. Native local embeddings run in an optional managed sidecar.

[![codecov](https://codecov.io/gh/agentics-skills/chelix/graph/badge.svg)](https://codecov.io/gh/agentics-skills/chelix)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/Rust-1.91%2B-orange.svg)](https://www.rust-lang.org)

[Installation](#installation) • [Architecture](#architecture--crate-map) • [Security](#security) • [Features](#features) • [How It Works](#how-it-works) • [Contributing](CONTRIBUTING.md)

</div>

---

> Chelix is a fork of [Moltis](https://github.com/moltis-org/moltis).

Please [open an issue](https://github.com/agentics-skills/chelix/issues) for any friction at all. I'm focused on making Chelix excellent.

**Secure by design** — Your keys never leave your machine. Every command runs in a sandboxed container, never on your host.

**Your hardware** — Runs on a Mac Mini, a Raspberry Pi, or any server you own. The Rust gateway has no Node.js or npm runtime; optional native local embeddings are isolated in a separately built managed sidecar.

**Full-featured** — Voice, memory, cross-session recall, automatic edit checkpoints, scheduling, Telegram, Signal, Discord, browser automation, MCP servers, SSH or node-backed remote command execution, managed deploy keys with host pinning in the web UI, a live Settings → Tools inventory, Cursor-compatible project context, and context-file threat scanning — all built-in. No plugin marketplace to get supply-chain attacked through.

**Auditable** — The agent runner and model interface fit in ~7.5K lines, with providers in ~19K more. The Rust workspace is ~270K lines across 59 modular crates you can audit independently, with 470+ Rust files containing tests. Unsafe code is isolated to FFI and precompiled runtime boundaries, not the core agent loop.

## Installation

```bash
# One-liner install script (macOS / Linux)
curl -fsSL https://raw.githubusercontent.com/agentics-skills/chelix/master/install.sh | sh

# Docker (multi-arch: amd64/arm64)
docker pull ghcr.io/agentics-skills/chelix:latest
```

## Architecture — Crate Map

Current Rust workspace: ~270K LoC across 59 crates. The table below groups the main crates by role so the architecture stays scannable.

**Core runtime**:

| Crate | LoC | Role |
|-------|-----|------|
| `chelix-gateway` | 37.4K | HTTP/WS server, RPC, auth, startup wiring |
| `chelix-tools` | 37.0K | Tool execution, sandboxing, WASM tools |
| `chelix-providers` | 18.9K | LLM provider implementations |
| `chelix-agents` | 14.5K | Agent loop, streaming, prompt assembly |
| `chelix-chat` | 14.2K | Chat engine, agent orchestration |
| `chelix-config` | 10.3K | Configuration, validation |
| `chelix-httpd` | 9.9K | HTTP server primitives and middleware |
| `chelix` (CLI) | 4.7K | Entry point, CLI commands |
| `chelix-embedding-service` | — | Optional managed local-GGUF embedding sidecar |
| `chelix-sessions` | 3.5K | Session persistence |
| `chelix-common` | 1.5K | Shared utilities |
| `chelix-service-traits` | 1.2K | Shared service interfaces |
| `chelix-protocol` | 0.7K | Wire protocol types |

**Feature and integration crates**:

| Category | Crates | Combined LoC |
|----------|--------|-------------|
| Channels | `chelix-telegram`, `chelix-whatsapp`, `chelix-signal`, `chelix-discord`, `chelix-msteams`, `chelix-matrix`, `chelix-slack`, `chelix-nostr`, `chelix-channels` | 34.0K |
| Web and APIs | `chelix-web`, `chelix-graphql`, `chelix-webhooks` | 10.8K |
| Extensibility | `chelix-mcp`, `chelix-mcp-agent-bridge`, `chelix-skills`, `chelix-plugins` | 11.5K |
| Memory and context | `chelix-memory`, `chelix-qmd`, `chelix-code-index`, `chelix-projects` | 11.7K |
| Voice and browser | `chelix-voice`, `chelix-browser` | 9.2K |
| Auth and security | `chelix-auth`, `chelix-oauth`, `chelix-vault`, `chelix-secret-store`, `chelix-tls` | 8.5K |
| Scheduling and automation | `chelix-cron`, `chelix-caldav`, `chelix-auto-reply` | 4.7K |
| Setup and import | `chelix-provider-setup`, `chelix-onboarding` | 11.7K |
| Native and node hosts | `chelix-node-host`, `chelix-courier` | 5.7K |
| WASM tools | `chelix-wasm-precompile`, `chelix-wasm-calc`, `chelix-wasm-web-fetch`, `chelix-wasm-web-search` | 1.4K |
| Supporting crates | `chelix-media`, `chelix-metrics`, `chelix-routing`, `chelix-canvas`, `chelix-schema-export`, `benchmarks` | 2.1K |

Use `--no-default-features --features lightweight` for constrained devices (Raspberry Pi, etc.).

## Security

- **Small unsafe surface** — core agent/gateway code stays safe Rust; unsafe is isolated to Swift FFI and precompiled WASM boundaries
- **Sandboxed execution** — Docker + Apple Container, per-session isolation
- **Secret handling** — `secrecy::Secret`, zeroed on drop, redacted from tool output
- **Authentication** — password + passkey (WebAuthn), rate-limited, per-IP throttle
- **SSRF protection** — DNS-resolved, blocks loopback/private/link-local
- **Origin validation** — rejects cross-origin WebSocket upgrades
- **Hook gating** — `BeforeToolCall` hooks can inspect/block any tool invocation
- **Supply chain integrity** — [artifact attestations](https://github.com/agentics-skills/chelix/attestations), Sigstore keyless signing, GPG signing (YubiKey), SHA-256/SHA-512 checksums

See [Security Architecture](docs/src/security.md) for details.
Verify releases with `gh attestation verify <artifact> -R agentics-skills/chelix` or see [Release Verification](docs/src/release-verification.md).

## Features

- **AI Gateway** — Multi-provider LLM support (OpenAI Codex, GitHub Copilot, Local), streaming responses, agent loop with sub-agent delegation, session modes, parallel tool execution
- **Communication** — Web UI, Telegram, Signal, Microsoft Teams, Discord, API access, voice I/O (8 TTS + 7 STT providers), mobile PWA with push notifications
- **Memory & Recall** — Per-agent memory workspaces, embeddings-powered long-term memory, hybrid vector + full-text search, session persistence with auto-compaction, cross-session recall, Cursor-compatible project context, context-file safety scanning
- **Safer Agent Editing** — Automatic checkpoints before built-in skill and memory mutations, restore tooling, session branching
- **Extensibility** — MCP servers (stdio + HTTP/SSE), skill system, 15 lifecycle hook events with circuit breaker, destructive command guard
- **Security** — Encryption-at-rest vault (XChaCha20-Poly1305 + Argon2id), password + passkey + API key auth, sandbox isolation, SSRF/CSWSH protection
- **Operations** — Cron scheduling, OpenTelemetry tracing, Prometheus metrics, cloud deploy, managed SSH deploy keys, host-pinned remote targets, live tool inventory in Settings, and CLI/web remote command doctor flows

## How It Works

Chelix is a **local-first persistent agent server** — a single Rust binary that
sits between you and multiple LLM providers, keeps durable session state, and
can meet you across channels without handing your data to a cloud relay.

```
┌─────────────┐  ┌─────────────┐  ┌─────────────┐
│   Web UI    │  │  Telegram   │  │  Discord    │
└──────┬──────┘  └──────┬──────┘  └──────┬──────┘
       │                │                │
       └────────┬───────┴────────┬───────┘
                │   WebSocket    │
                ▼                ▼
        ┌─────────────────────────────────┐
        │          Gateway Server         │
        │   (Axum · HTTP · WS · Auth)     │
        ├─────────────────────────────────┤
        │        Chat Service             │
        │  ┌───────────┐ ┌─────────────┐  │
        │  │   Agent   │ │    Tool     │  │
        │  │   Runner  │◄┤   Registry  │  │
        │  └─────┬─────┘ └─────────────┘  │
        │        │                        │
        │  ┌─────▼─────────────────────┐  │
        │  │    Provider Registry      │  │
        │  │  Multiple providers       │  │
        │  │  (Codex · Copilot · Local)│  │
        │  └───────────────────────────┘  │
        ├─────────────────────────────────┤
        │  Sessions  │ Memory  │  Hooks   │
        │  (JSONL)   │ (SQLite)│ (events) │
        └─────────────────────────────────┘
                       │
               ┌───────▼───────┐
               │    Sandbox    │
               │ Docker/Apple  │
               │  Container    │
               └───────────────┘
```

See [Quickstart](docs/src/quickstart.md) for gateway startup, message flow, sessions, and memory details.

## Getting Started

### Build & Run

Requires [just](https://github.com/casey/just) (command runner) and Node.js (for Tailwind CSS).

```bash
git clone https://github.com/agentics-skills/chelix.git
cd chelix
just build-css                  # Build Tailwind CSS for the web UI
just build-release              # Build gateway, then local-embedding sidecar separately
cargo run --release --bin chelix
```

For a full release build including WASM sandbox tools:

```bash
just build-release-with-wasm    # Builds WASM artifacts + release binary
cargo run --release --bin chelix
```

Open `https://chelix.localhost:3000`. On first run, a setup code is printed to
the terminal — enter it in the web UI to set your password or register a passkey.

Optional flags: `--config-dir /path/to/config --data-dir /path/to/data`

### Docker

```bash
# Docker / OrbStack
docker run -d \
  --name chelix \
  -p 13131:13131 \
  -p 13132:13132 \
  -p 1455:1455 \
  -v chelix-config:/home/chelix/.config/chelix \
  -v chelix-data:/home/chelix/.chelix \
  -v /var/run/docker.sock:/var/run/docker.sock \
  ghcr.io/agentics-skills/chelix:latest
```

Open `https://localhost:13131` and complete the setup. For unattended Docker
deployments, set `CHELIX_PASSWORD`, `CHELIX_PROVIDER`, and `CHELIX_API_KEY`
before first boot to skip the setup wizard. See [Docker docs](docs/src/docker.md)
for Podman, OrbStack, TLS trust, and persistence details.

### Cloud Deployment

Chelix publishes `ghcr.io/agentics-skills/chelix:latest` and includes deployment
examples for supported container platforms.

All cloud configs use `--no-tls` because the provider handles TLS termination.
See [Cloud Deploy docs](docs/src/cloud-deploy.md) for generic settings and
current platform examples.

## Star History

[![Star History Chart](https://api.star-history.com/svg?repos=agentics-skills/chelix&type=date&legend=top-left)](https://www.star-history.com/#agentics-skills/chelix&type=date&legend=top-left)

## License

MIT — see [LICENSE.md](LICENSE.md).
