# Installation

Chelix is distributed as a single self-contained binary. Choose the installation method that works best for your setup.

## Quick Install (Recommended)

The fastest way to get started on macOS or Linux:

```bash
curl -fsSL https://raw.githubusercontent.com/agentics-skills/chelix/master/install.sh | sh
```

This downloads the latest release for your platform and installs it to `~/.local/bin`.

## Docker

Multi-architecture images (amd64/arm64) are published to GitHub Container Registry:

```bash
docker pull ghcr.io/moltis-org/moltis:latest
```

See [Docker Deployment](docker.md) for full instructions on running Chelix in a container.

## First Run

After installation, start Chelix:

```bash
moltis
```

On first launch:

1. Open `http://localhost:<port>` in your browser (the port is shown in the terminal output)
2. Configure your LLM provider (API key)
3. Start chatting!

```admonish tip
Chelix picks a random available port on first install to avoid conflicts. The port is saved in your config and reused on subsequent runs.
```

```admonish note
Authentication is only required when accessing Chelix from a non-localhost address (e.g., over the network). When this happens, a one-time setup code is printed to the terminal for initial authentication setup.
```

## Verify Installation

```bash
moltis --version
```

## Uninstalling

### Remove Data

Chelix stores data in two directories:

```bash
# Configuration
rm -rf ~/.config/moltis

# Data (sessions, databases, memory)
rm -rf ~/.moltis
```

```admonish warning
Removing these directories deletes all your conversations, memory, and settings permanently.
```
