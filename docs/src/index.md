# Chelix

```admonish warning title="Alpha software: use with care"
Running an AI assistant on your own machine or server is still new territory. Treat Chelix as alpha software: run it in isolated environments, review enabled tools/providers, keep secrets scoped and rotated, and avoid exposing it publicly without strong authentication and network controls.
```

<div style="text-align: center; margin: 2em 0;">
<strong style="font-size: 1.2em;">A secure persistent personal agent server written in Rust.<br>Native gateway and managed services, no Node.js or npm runtime.</strong>
</div>

> Chelix is a fork of [Chelix](https://github.com/agentics-skills/chelix).

Chelix compiles the AI gateway and web assets into native Rust executables. The
gateway manages a required `chelix-tools-service` sidecar for native filesystem
tools, and local embeddings use a separate optional managed service. There is no
Node.js process to babysit, no `node_modules` to sync, and no V8 garbage collector
introducing latency spikes.

```bash
# Quick install (macOS / Linux)
curl -fsSL https://raw.githubusercontent.com/agentics-skills/chelix/master/install.sh | sh
```

## Why Chelix?

| Feature             | Chelix                   | Other Solutions        |
| ------------------- | ------------------------ | ---------------------- |
| **Deployment**      | Native gateway + managed services | Node.js + dependencies |
| **Memory Safety**   | Rust ownership           | Garbage collection     |
| **Secret Handling** | Zeroed on drop           | "Eventually collected" |
| **Sandbox**         | Docker + Apple Container | Docker only            |
| **Startup**         | Awaited service and sandbox readiness | Deferred setup          |

## Key Features

- **Multiple LLM Providers** — Anthropic, OpenAI, Google Gemini, xAI,
  OpenRouter, Moonshot, Z.AI, and more
- **Streaming-First** — Responses appear as tokens arrive, not after completion
- **Managed Tool Execution** — Host and sandbox tools use the required
  `chelix-tools-service`
- **MCP Support** — Connect to Model Context Protocol servers for extended
  capabilities
- **Multi-Channel** — Web UI, Telegram, Discord, API access with synchronized
  responses
- **Built-in Throttling** — Per-IP endpoint limits with strict login protection
- **Long-Term Memory** — Embeddings-powered knowledge base with hybrid search
- **Cross-Session Recall** — Search earlier sessions for relevant snippets and
  prior decisions
- **SSH Key Management** — Manage deploy keys, named targets, connectivity
  checks, and host-key pins in Settings
- **Context Hardening** — Load `CLAUDE.md`, `AGENTS.md`, `.cursorrules`, and
  rule directories with safety scanning
- **Hook System** — Observe, modify, or block actions at any lifecycle point
- **Compile-Time Safety** — Misconfigurations caught by `cargo check`, not
  runtime crashes

See the full list of [supported providers](providers.md).

## Quick Start

```bash
# Install
curl -fsSL https://raw.githubusercontent.com/agentics-skills/chelix/master/install.sh | sh

# Run
chelix
```

On first launch:

1. Open the URL shown in your browser (e.g., `http://localhost:13131`)
2. Add your LLM API key
3. Start chatting!

```admonish note
Authentication is only required when accessing Chelix from a non-localhost address. On localhost, you can start using it immediately.
```

→ [Full Quickstart Guide](quickstart.md)

## How It Works

```
┌──────────┐  ┌──────────┐  ┌──────────┐  ┌──────────┐
│  Web UI  │  │ Telegram │  │ Discord  │  │   API    │
└────┬─────┘  └────┬─────┘  └────┬─────┘  └────┬─────┘
     │             │             │             │
     └─────────────┴─────────┬───┴─────────────┘
                             │
                             ▼
        ┌───────────────────────────────┐
        │       Chelix Gateway          │
        │   ┌─────────┐ ┌───────────┐   │
        │   │  Agent  │ │   Tools   │   │
        │   │  Loop   │◄┤  Registry │   │
        │   └────┬────┘ └───────────┘   │
        │        │                      │
        │   ┌────▼────────────────┐     │
        │   │  Provider Registry  │     │
        │   │ Anthropic·OpenAI·Gemini… │   │
        │   └─────────────────────┘     │
        └───────────┬───────────────────┘
                    │
        ┌───────────▼───────────────────┐
        │ Managed Tools Service         │
        │ Host or sandbox endpoint      │
        └───────────┬───────────────────┘
                    │
            ┌───────▼───────┐
            │    Sandbox    │
            │ Docker/Apple  │
            └───────────────┘
```

## Documentation

### Getting Started

- **[Quickstart](quickstart.md)** — Up and running in 5 minutes
- **[Installation](installation.md)** — All installation methods
- **[Configuration](configuration.md)** — `chelix.toml` reference
- **[End-to-End Testing](e2e-testing.md)** — Browser regression coverage for the
  web UI

### Features

- **[Providers](providers.md)** — Configure LLM providers
- **[MCP Servers](mcp.md)** — Extend with Model Context Protocol
- **[Hooks](hooks.md)** — Lifecycle hooks for customization

### Deployment

- **[Docker](docker.md)** — Container deployment

### Architecture

- **[Streaming](streaming.md)** — How real-time streaming works
- **[Managed Tools Service](tools-service.md)** — Native tool process, routing,
  authentication, and sandbox lifecycle
- **[Metrics & Tracing](metrics-and-tracing.md)** — Observability

## Security

Chelix applies defense in depth:

- **Authentication** — Password or passkey (WebAuthn) required for non-localhost
  access
- **SSRF Protection** — Blocks requests to internal networks
- **Secret Handling** — `secrecy::Secret` zeroes memory on drop
- **Sandboxed Execution** — Enabled sessions use Docker or Apple Container;
  per-session policy can select host execution
- **Origin Validation** — Prevents Cross-Site WebSocket Hijacking
- **No Unsafe Code** — `unsafe` is denied workspace-wide

## Community

- **GitHub**:
  [github.com/agentics-skills/chelix](https://github.com/agentics-skills/chelix)
- **Issues**: [Report bugs](https://github.com/agentics-skills/chelix/issues)
- **Discussions**:
  [Ask questions](https://github.com/agentics-skills/chelix/discussions)

## License

MIT — Free for personal and commercial use.
