# Cloud Deploy

Chelix publishes a multi-arch Docker image (`linux/amd64` and `linux/arm64`) to
`ghcr.io/agentics-skills/chelix`. You can deploy it to any cloud provider that
supports container images.

## Common configuration

All cloud providers terminate TLS at the edge, so Chelix must run in plain HTTP
mode. The key settings are:

| Setting                            | Value               | Purpose                                      |
| ---------------------------------- | ------------------- | -------------------------------------------- |
| `--no-tls` or `CHELIX_NO_TLS=true` | Disable TLS         | Provider handles HTTPS                       |
| `--bind 0.0.0.0`                   | Bind all interfaces | Required for container networking            |
| `--port <PORT>`                    | Listen port         | Must match provider's expected internal port |
| `CHELIX_CONFIG_DIR=/data/config`   | Config directory    | Persist chelix.toml, credentials             |
| `CHELIX_DATA_DIR=/data`            | Data directory      | Persist databases, sessions, memory          |
| `CHELIX_DEPLOY_PLATFORM`           | Deploy platform     | Hides local-only providers (see below)       |
| `CHELIX_PASSWORD`                  | Initial password    | Set auth password via environment variable   |

```admonish tip
If requests to your domain are redirected to `:13131`, Chelix TLS is still
enabled behind a TLS-terminating proxy. Use `--no-tls` (or
`CHELIX_NO_TLS=true`).

Only keep Chelix TLS enabled when your proxy talks HTTPS to Chelix (or uses
TCP TLS passthrough). In that case, set `CHELIX_ALLOW_TLS_BEHIND_PROXY=true`.
```

```admonish tip
**Sandbox on cloud deploys**: Most cloud providers do not support
Docker-in-Docker. Sandboxed command execution therefore requires a
deployment target with a local container runtime available to Chelix.
```

### `CHELIX_DEPLOY_PLATFORM`

Set this to a non-empty label for your deployment target when Chelix runs on a
remote container platform. When set, Chelix hides local-only provider entries
from the provider setup page since they cannot run on cloud VMs.

## OAuth Providers (OpenAI Codex, GitHub Copilot)

OAuth providers that redirect to `localhost` (like OpenAI Codex) cannot complete
the browser flow when Chelix runs on a remote server — `localhost` on the user's
browser points to their own machine, not the cloud instance.

**Use the CLI to authenticate instead:**

```bash
# Generic container
docker exec -it <container> chelix auth login --provider openai-codex
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
deployments the easiest approach is to set the `CHELIX_PASSWORD` environment
variable (or secret) before deploying. This pre-configures the password so the
setup code flow is skipped.

Add the variable through your platform's secret or environment variable
configuration.

## Health checks

All provider configs use the `/health` endpoint which returns HTTP 200 when the
gateway is ready. Configure your provider's health check to use:

- **Path**: `/health`
- **Method**: `GET`
- **Expected status**: `200`
