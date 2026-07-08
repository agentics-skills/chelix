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
Docker-in-Docker. Sandboxed command execution therefore requires a
deployment target with a local container runtime available to Chelix.
```

### `MOLTIS_DEPLOY_PLATFORM`

Set this to the name of your cloud provider (e.g. `render`,
`coolify`). When set, Chelix hides local-only LLM providers
such as Ollama from the provider setup page since they cannot run
on cloud VMs. The included Render blueprint and Coolify example already set
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

## OAuth Providers (OpenAI Codex, GitHub Copilot)

OAuth providers that redirect to `localhost` (like OpenAI Codex) cannot
complete the browser flow when Chelix runs on a remote server — `localhost`
on the user's browser points to their own machine, not the cloud instance.

**Use the CLI to authenticate instead:**

```bash
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

For Render, set the variable in the dashboard's environment settings. On other
platforms, add the same variable through the platform's secret or environment
variable configuration.

## Health checks

All provider configs use the `/health` endpoint which returns HTTP 200 when
the gateway is ready. Configure your provider's health check to use:

- **Path**: `/health`
- **Method**: `GET`
- **Expected status**: `200`
