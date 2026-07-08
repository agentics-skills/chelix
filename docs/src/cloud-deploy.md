# Cloud Deploy

Chelix publishes a multi-arch Docker image (`linux/amd64` and `linux/arm64`)
to `ghcr.io/moltis-org/moltis`. You can deploy it to any cloud provider that
supports container images.

## Common configuration

All cloud providers terminate TLS at the edge, so Chelix must run in plain
HTTP mode. The key settings are:

| Setting | Value | Purpose |
|---------|-------|---------|
| `--no-tls` or `MOLTIS_NO_TLS=true` | Disable TLS | Provider handles HTTPS |
| `--bind 0.0.0.0` | Bind all interfaces | Required for container networking |
| `--port <PORT>` | Listen port | Must match provider's expected internal port |
| `MOLTIS_CONFIG_DIR=/data/config` | Config directory | Persist moltis.toml, credentials |
| `MOLTIS_DATA_DIR=/data` | Data directory | Persist databases, sessions, memory |
| `MOLTIS_DEPLOY_PLATFORM` | Deploy platform | Hides local-only providers (see below) |
| `MOLTIS_PASSWORD` | Initial password | Set auth password via environment variable |

```admonish tip
If requests to your domain are redirected to `:13131`, Chelix TLS is still
enabled behind a TLS-terminating proxy. Use `--no-tls` (or
`MOLTIS_NO_TLS=true`).

Only keep Chelix TLS enabled when your proxy talks HTTPS to Chelix (or uses
TCP TLS passthrough). In that case, set `MOLTIS_ALLOW_TLS_BEHIND_PROXY=true`.
```

```admonish tip
**Sandbox on cloud deploys**: Most cloud providers do not support
Docker-in-Docker. To enable sandboxed command execution, configure a
[remote sandbox backend](sandbox-remote.md) — set `VERCEL_TOKEN` for Vercel
Firecracker microVMs, or `DAYTONA_API_KEY` for Daytona cloud sandboxes
(including self-hosted). Chelix auto-detects these when no local Docker is
available.
```

### `MOLTIS_DEPLOY_PLATFORM`

Set this to the name of your cloud provider (e.g. `flyio`,
`render`). When set, Chelix hides local-only LLM providers
such as Ollama from the provider setup page since they cannot run
on cloud VMs. The included deploy templates for Fly.io and Render already set
this variable.

## Coolify (self-hosted, e.g. Hetzner)

Coolify deployments can run Chelix with sandboxed execute_command tools, as long as the
service mounts the host Docker socket.

- Use [`examples/docker-compose.coolify.yml`](https://github.com/agentics-skills/chelix/blob/master/examples/docker-compose.coolify.yml)
  as a starting point.
- Run Chelix with `--no-tls` (Coolify terminates HTTPS at the proxy).
- Set `MOLTIS_BEHIND_PROXY=true` so client IP/auth behavior is correct behind
  reverse proxying.
- Mount `/var/run/docker.sock:/var/run/docker.sock` to enable container-backed
  sandbox execution.

## Fly.io

The repository includes a `fly.toml` ready to use.

### Quick start

```bash
# Install the Fly CLI if you haven't already
curl -L https://fly.io/install.sh | sh

# Launch from the repo (uses fly.toml)
fly launch --image ghcr.io/moltis-org/moltis:latest

# Set your password
fly secrets set MOLTIS_PASSWORD="your-password"

# Create persistent storage
fly volumes create moltis_data --region iad --size 1
```

### How it works

- **Image**: pulled from `ghcr.io/moltis-org/moltis:latest`
- **Port**: internal 8080, Fly terminates TLS and routes HTTPS traffic
- **Storage**: a Fly Volume mounted at `/data` persists the database, sessions,
  and memory files
- **Auto-scaling**: machines stop when idle and start on incoming requests

### Custom domain

```bash
fly certs add your-domain.com
```

Then point a CNAME to `your-app.fly.dev`.

## Render

[![Deploy to Render](https://render.com/images/deploy-to-render-button.svg)](https://render.com/deploy?repo=https://github.com/agentics-skills/chelix)

The repository includes a `render.yaml` blueprint. Click the button above or:

1. Go to **Dashboard** > **New** > **Blueprint**
2. Connect your fork of the Chelix repository
3. Render will detect `render.yaml` and configure the service

### Configuration details

- **Port**: Render uses port 10000 by default
- **Persistent disk**: 1 GB mounted at `/data` (included in the blueprint)
- **Environment**: set `MOLTIS_PASSWORD` in the Render dashboard under
  **Environment** > **Secret Files** or **Environment Variables**

<!-- TODO: Railway deploy does not work yet
## Railway

The repository includes a `railway.json` configuration that sets the required
environment variables (`MOLTIS_CONFIG_DIR`, `MOLTIS_DATA_DIR`,
`MOLTIS_DEPLOY_PLATFORM`) automatically.

1. Create a new project on [Railway](https://railway.com)
2. Add a service from **Docker Image**: `ghcr.io/moltis-org/moltis:latest`
3. Railway injects the `$PORT` variable automatically; the `railway.json` start
   command handles the rest
4. Set additional environment variables in the Railway dashboard:
   - `MOLTIS_PASSWORD` = your password

### Persistent storage

Railway supports persistent volumes. Add one in the service settings and mount
it at `/data`.
-->

## OAuth Providers (OpenAI Codex, GitHub Copilot)

OAuth providers that redirect to `localhost` (like OpenAI Codex) cannot
complete the browser flow when Chelix runs on a remote server — `localhost`
on the user's browser points to their own machine, not the cloud instance.

**Use the CLI to authenticate instead:**

```bash
# Fly.io
fly ssh console -C "moltis auth login --provider openai-codex"

# Generic container
docker exec -it <container> moltis auth login --provider openai-codex
```

The CLI opens a browser on the machine where you run the command. If automatic
callback capture fails, Chelix prompts you to paste the callback URL (or
`code#state`) directly in the terminal. After you log in, tokens are saved to
the config volume and the running gateway picks them up automatically — no
restart needed.

```admonish tip
GitHub Copilot uses device-flow authentication (a code you enter on
github.com), so it works from the web UI without this workaround.
```

## Authentication

On first launch, Chelix requires a password or passkey to be set. In cloud
deployments the easiest approach is to set the `MOLTIS_PASSWORD` environment
variable (or secret) before deploying. This pre-configures the password so the
setup code flow is skipped.

```bash
# Fly.io
fly secrets set MOLTIS_PASSWORD="your-secure-password"

```

For Render, set the variable in the dashboard's environment settings.

## Health checks

All provider configs use the `/health` endpoint which returns HTTP 200 when
the gateway is ready. Configure your provider's health check to use:

- **Path**: `/health`
- **Method**: `GET`
- **Expected status**: `200`
