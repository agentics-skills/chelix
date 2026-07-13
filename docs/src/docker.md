# Running Chelix in Docker

Chelix is available as a multi-architecture Docker image supporting both
`linux/amd64` and `linux/arm64`. The image is published to GitHub Container
Registry on every release.

## Quick Start

```bash
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

Open https://localhost:13131 in your browser and configure your LLM provider to start chatting.

For unattended bootstraps, add `CHELIX_TOKEN`, `CHELIX_PROVIDER`, and
`CHELIX_API_KEY` before first start. That pre-configures auth plus one LLM
provider so you can skip the browser setup wizard entirely.

### Ports

| Port | Purpose |
|------|---------|
| 13131 | Gateway (HTTPS by default, HTTP with `--no-tls`) — web UI, API, WebSocket |
| 13132 | HTTP — CA certificate download for local TLS trust |
| 1455 | OAuth callback — required for OpenAI Codex and other providers with pre-registered redirect URIs |

### Trusting the TLS certificate

Chelix generates a self-signed CA on first run. Browsers will show a security
warning until you trust this CA. Port 13132 serves the certificate over plain
HTTP so you can download it:

```bash
# Download the CA certificate
curl -o chelix-ca.pem http://localhost:13132/certs/ca.pem

# macOS — add to system Keychain and trust it
sudo security add-trusted-cert -d -r trustRoot \
  -k /Library/Keychains/System.keychain chelix-ca.pem

# Linux (Debian/Ubuntu)
sudo cp chelix-ca.pem /usr/local/share/ca-certificates/chelix-ca.crt
sudo update-ca-certificates
```

After trusting the CA, restart your browser. The warning will not appear again
(the CA persists in the mounted config volume).

This local CA only solves certificate trust for names included in the generated
server certificate, such as `localhost`, `chelix.localhost`, the container or
host name, and sometimes an inferred non-loopback bind IP. It does not make a
certificate valid for an arbitrary public VPS IP address or hosting-provider
domain. Browsers still reject those targets with a certificate name mismatch if
they are not present in the certificate SAN list. IP-address URLs require an IP
SAN. For direct `https://<public-ip>:13131` access, set `tls.public_ip` to that
address before starting Chelix so the auto-generated certificate includes it:

```toml
[tls]
public_ip = "203.0.113.10"
```

Regular public TLS deployments should use a domain name.

For internet-facing Docker deployments, prefer a domain name plus a reverse
proxy with public CA certificates. Run Chelix with `--no-tls`, set
`CHELIX_BEHIND_PROXY=true`, and point the proxy at `http://<chelix-host>:13131`.
If you want Chelix to serve HTTPS directly, mount a certificate and private key
whose SANs cover the public hostname and set `tls.cert_path` and `tls.key_path`
in `chelix.toml`.

```admonish note
When accessing from localhost, no authentication is required. If you access Chelix from a different machine (e.g., over the network), a setup code is printed to the container logs for authentication setup:

~~~bash
docker logs chelix
~~~
```

## Volume Mounts

Chelix uses two directories that should be persisted:

| Path | Contents |
|------|----------|
| `/home/chelix/.config/chelix` | Configuration files: `chelix.toml`, `credentials.json`, `mcp-servers.json` |
| `/home/chelix/.chelix` | Runtime data: databases, sessions, memory files, logs |
| `/home/chelix/.npm` | npm cache (used by stdio-based MCP servers) |

You can use named volumes (as shown above) or bind mounts to local directories
for easier access to configuration files:

```bash
docker run -d \
  --name chelix \
  -p 13131:13131 \
  -p 13132:13132 \
  -p 1455:1455 \
  -v ./config:/home/chelix/.config/chelix \
  -v ./data:/home/chelix/.chelix \
  -v /var/run/docker.sock:/var/run/docker.sock \
  ghcr.io/agentics-skills/chelix:latest
```

With bind mounts, you can edit `config/chelix.toml` directly on the host.

## Docker Socket (Sandbox Execution)

Chelix runs LLM-generated shell commands inside isolated containers for
security. When Chelix itself runs in a container, it needs access to the host's
container runtime to create these sandbox containers.

```bash
# Recommended for full container isolation
-v /var/run/docker.sock:/var/run/docker.sock
```

**Without the socket mount**, Chelix automatically falls back to the
[restricted-host sandbox](sandbox.md#restricted-host-sandbox), which provides
lightweight isolation by clearing environment variables, restricting `PATH`,
and applying resource limits via `ulimit`. Commands will execute successfully
inside the Chelix container but without filesystem or network isolation.

For full container-level isolation (filesystem boundaries, network policies),
mount the Docker socket.

If Chelix is itself running in Docker and your `data_dir()` mount is backed by
a different host path than `/home/chelix/.chelix`, Chelix tries to discover that
host path automatically from `docker inspect`/`podman inspect`. It first checks
the current container's hostname/cgroup references, then scans running
containers for an unambiguous mount of Chelix's data directory. If that lookup
still fails, add this to `/home/chelix/.config/chelix/chelix.toml` inside the
container:

```toml
[sandbox]
host_data_dir = "/absolute/host/path/to/data"
```

For a bind mount like `-v ./data:/home/chelix/.chelix`, use the resolved host
path to `./data`. This setting is also used by sandboxed browser containers for
their persistent Chrome profile directory. If browser startup logs show
`/data/browser-profile/SingletonLock: Permission denied`, Chelix probably fell
back to the in-container path (`/home/chelix/.chelix/...`) instead of the real
host path. Set `host_data_dir` to the host-visible data directory and restart
Chelix so new sandbox and browser containers pick up the corrected mount
source.

### Security Consideration

Mounting the Docker socket gives the container full access to the Docker
daemon. This is equivalent to root access on the host for practical purposes.
Only run Chelix containers from trusted sources (official images from
`ghcr.io/agentics-skills/chelix`).

## Docker Compose

See [`examples/docker-compose.yml`](https://github.com/agentics-skills/chelix/blob/master/examples/docker-compose.yml) for a
complete example:

```yaml
services:
  chelix:
    image: ghcr.io/agentics-skills/chelix:latest
    container_name: chelix
    restart: unless-stopped
    ports:
      - "13131:13131"
      - "13132:13132"
      - "1455:1455"   # OAuth callback (OpenAI Codex, etc.)
    volumes:
      - ./config:/home/chelix/.config/chelix
      - ./data:/home/chelix/.chelix
      - /var/run/docker.sock:/var/run/docker.sock
```

For unattended recovery after host reboots or in-place `/update`, store the
vault recovery key as a Docker secret and point Chelix at the mounted file:

```yaml
services:
  chelix:
    image: ghcr.io/agentics-skills/chelix:latest
    environment:
      CHELIX_VAULT_AUTO_UNSEAL_KEY_FILE: /run/secrets/chelix_vault_recovery_key
    secrets:
      - chelix_vault_recovery_key

secrets:
  chelix_vault_recovery_key:
    file: ./chelix-vault-recovery-key
```

This lets encrypted environment variables and channel credentials load during
startup. Treat the secret file as sensitive as the vault recovery key itself.
If you create the secret file before the vault is initialized, Docker will
accept the mount but Chelix cannot auto-unseal from an empty file. After you
initialize the vault in **Settings > Encryption**, copy the one-time recovery
key into this file before relying on unattended auto-unseal.

## Browser Sandbox in Docker

When Chelix runs inside Docker and launches a sandboxed browser, the browser
container is a sibling container on the host. By default, Chelix connects to
`127.0.0.1` which only reaches its own loopback, not the browser.

The sibling browser also needs a host-visible mount for its Chrome profile. If
your Chelix data directory is bind-mounted or stored somewhere that is not
visible on the host as `/home/chelix/.chelix`, configure
`[sandbox].host_data_dir` as described in
[Docker Socket Sandbox Execution](#docker-socket-sandbox-execution). Without
that override, Chrome may fail with `SingletonLock: Permission denied` when the
browser container tries to write `/data/browser-profile`.

Add `container_host` to your `chelix.toml` so Chelix can reach the browser
container through the host's port mapping:

```toml
[tools.browser]
container_host = "host.docker.internal"
```

On Linux, add `--add-host` to the Chelix container so `host.docker.internal`
resolves to the host:

```bash
docker run -d \
  --name chelix \
  --add-host=host.docker.internal:host-gateway \
  -p 13131:13131 \
  -p 13132:13132 \
  -p 1455:1455 \
  -v chelix-config:/home/chelix/.config/chelix \
  -v chelix-data:/home/chelix/.chelix \
  -v /var/run/docker.sock:/var/run/docker.sock \
  ghcr.io/agentics-skills/chelix:latest
```

Alternatively, use the Docker bridge gateway IP directly
(`container_host = "172.17.0.1"` on most Linux setups).

## Podman Support

Chelix works with Podman using its Docker-compatible API. Mount the Podman
socket instead of the Docker socket:

```bash
# Podman rootless
podman run -d \
  --name chelix \
  -p 13131:13131 \
  -p 13132:13132 \
  -p 1455:1455 \
  -v chelix-config:/home/chelix/.config/chelix \
  -v chelix-data:/home/chelix/.chelix \
  -v /run/user/$(id -u)/podman/podman.sock:/var/run/docker.sock \
  ghcr.io/agentics-skills/chelix:latest

# Podman rootful
podman run -d \
  --name chelix \
  -p 13131:13131 \
  -p 13132:13132 \
  -p 1455:1455 \
  -v chelix-config:/home/chelix/.config/chelix \
  -v chelix-data:/home/chelix/.chelix \
  -v /run/podman/podman.sock:/var/run/docker.sock \
  ghcr.io/agentics-skills/chelix:latest
```

You may need to enable the Podman socket service first:

```bash
# Rootless
systemctl --user enable --now podman.socket

# Rootful
sudo systemctl enable --now podman.socket
```

## Environment Variables

| Variable | Description |
|----------|-------------|
| `CHELIX_CONFIG_DIR` | Override config directory (default: `~/.config/chelix`) |
| `CHELIX_DATA_DIR` | Override data directory (default: `~/.chelix`) |
| `CHELIX_NO_TLS` | Disable TLS (serve plain HTTP) — equivalent to `--no-tls` |

Example:

```bash
docker run -d \
  --name chelix \
  -p 13131:13131 \
  -p 13132:13132 \
  -p 1455:1455 \
  -e CHELIX_CONFIG_DIR=/config \
  -e CHELIX_DATA_DIR=/data \
  -v ./config:/config \
  -v ./data:/data \
  -v /var/run/docker.sock:/var/run/docker.sock \
  ghcr.io/agentics-skills/chelix:latest
```

### API Keys and the `[env]` Section

Features like web search (Brave), embeddings, and LLM provider API calls read
keys from process environment variables (`std::env::var`). In Docker, there are
three ways to provide these:

**Option 1: Generic first-run LLM bootstrap** (best for one provider)

Use this when you want a minimal `docker compose` file with one chat provider
and no manual setup:

```yaml
services:
  chelix:
    image: ghcr.io/agentics-skills/chelix:latest
    environment:
      CHELIX_TOKEN: "change-me"
      CHELIX_PROVIDER: "openai"
      CHELIX_API_KEY: "sk-..."
```

`CHELIX_PROVIDER` must be a Chelix provider name such as `openai`,
`anthropic`, `gemini`, `openrouter`, or `moonshot`. The shorter
aliases `PROVIDER` and `API_KEY` also work, but the `CHELIX_*` names are
preferred because they are less likely to collide with other containers.

**Option 2: Provider-specific `docker -e` flags** (takes precedence for that provider)

```bash
docker run -d \
  --name chelix \
  -e BRAVE_API_KEY=your-key \
  -e OPENROUTER_API_KEY=sk-or-... \
  ...
  ghcr.io/agentics-skills/chelix:latest
```

**Option 3: `[env]` section in `chelix.toml`**

Add an `[env]` section to your config file. These variables are injected into
the Chelix process at startup, making them available to all features:

```toml
[env]
BRAVE_API_KEY = "your-brave-key"
OPENROUTER_API_KEY = "sk-or-..."
```

If a variable is set both via `docker -e` and `[env]`, the Docker/host
environment value wins — `[env]` never overwrites existing variables.

```admonish info title="Settings UI env vars"
Environment variables set through the Settings UI (Settings > Environment)
are stored in SQLite. At startup, Chelix injects them into the process
environment so they are available to all features (search, embeddings,
provider API calls), not just sandbox commands.

Precedence order (highest wins):
1. Host / `docker -e` environment variables
2. Config file `[env]` section
3. Settings UI environment variables
```

## Building Locally

To build the Docker image from source:

```bash
# Single architecture (current platform)
docker build -t chelix:local .

# Multi-architecture (requires buildx)
docker buildx build --platform linux/amd64,linux/arm64 -t chelix:local .
```

## OrbStack

OrbStack on macOS works identically to Docker — use the same socket path
(`/var/run/docker.sock`). OrbStack's lightweight Linux VM provides good
isolation with lower resource usage than Docker Desktop.

## Troubleshooting

### "Cannot connect to Docker daemon"

The Docker socket is not mounted or the Chelix user doesn't have permission
to access it. Verify:

```bash
docker exec chelix ls -la /var/run/docker.sock
```

### Setup code not appearing in logs (for network access)

The setup code only appears when accessing from a non-localhost address. If you're accessing from the same machine via `localhost`, no setup code is needed. For network access, wait a few seconds for the gateway to start, then check logs:

```bash
docker logs chelix 2>&1 | grep -i setup
```

### OAuth authentication error (OpenAI Codex)

If clicking **Connect** for OpenAI Codex shows "unknown_error" on OpenAI's
page, port 1455 is not reachable from your browser. Make sure you published it:

```bash
-p 1455:1455
```

If you're running Chelix on a remote server (cloud VM, VPS) and accessing it
over the network, `localhost:1455` on the browser side points to your local
machine — not the server. In that case, authenticate via the CLI instead:

```bash
docker exec -it chelix chelix auth login --provider openai-codex
```

The CLI opens a browser on the machine where you run the command and handles
the OAuth callback locally. If automatic callback capture fails, Chelix prompts
you to paste the callback URL (or `code#state`) into the terminal. Tokens are
saved to the config volume and picked up by the running gateway automatically.

### Permission denied on bind mounts

When using bind mounts, ensure the directories exist and are writable:

```bash
mkdir -p ./config ./data
chmod 755 ./config ./data
```

The container runs as user `chelix` (UID 1000). If you see permission errors,
you may need to adjust ownership:

```bash
sudo chown -R 1000:1000 ./config ./data
```
