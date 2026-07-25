# Installation

Chelix is distributed as a native gateway plus the required managed
`chelix-tools-service` binary. Release packages install both executables together;
local embeddings may add an optional managed embedding service. Choose the
installation method that works best for your setup.

## Quick Install (Recommended)

The fastest way to get started on macOS or Linux:

```bash
curl -fsSL https://raw.githubusercontent.com/agentics-skills/chelix/master/install.sh | sh
```

This downloads the latest release for your platform and installs `chelix` and
`chelix-tools-service` to `~/.local/bin`. On macOS, the release also installs the
matching Linux `chelix-tools-service-linux-<arch>` artifact used to construct
sandbox images.

## Docker

Multi-architecture images (amd64/arm64) are published to GitHub Container
Registry:

```bash
docker pull ghcr.io/agentics-skills/chelix:latest
```

See [Docker Deployment](docker.md) for full instructions on running Chelix in a
container.

## First Run

After installation, start Chelix:

```bash
chelix
```

On first launch:

1. Open `http://localhost:<port>` in your browser (the port is shown in the
   terminal output)
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
chelix --version
chelix-tools-service --help
```

The tools service is normally started and supervised by Chelix; the second
command only verifies that the required sibling executable is installed.

## Uninstalling

### Remove Data

Chelix stores data in two directories:

```bash
# Configuration
rm -rf ~/.config/chelix

# Data (sessions, databases, memory)
rm -rf ~/.chelix
```

```admonish warning
Removing these directories deletes all your conversations, memory, and settings permanently.
```
