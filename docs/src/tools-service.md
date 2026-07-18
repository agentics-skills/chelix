# Managed Tools Service

Chelix runs filesystem tools that require native executables through the
separate `chelix-tools-service` binary. The first migrated tool is `ripgrep`:
the gateway no longer starts `rg` directly.

The gateway and every supported container sandbox use the same versioned HTTP
API. Routing changes the service endpoint, not the tool protocol.

## Process model

At gateway startup, Chelix:

1. prepares the current deterministic sandbox image;
2. starts a managed host `chelix-tools-service` process;
3. verifies its authenticated health endpoint;
4. registers tools that call the service.

The host service binds to `127.0.0.1:0`, so the operating system selects an
available loopback port. It writes one JSON readiness record to its stdout pipe.
That record contains the protocol version, selected port, and generated token.
The gateway starts the child with `--shutdown-on-stdin-eof` and configures
kill-on-drop for the managed process.

```admonish note title="A host tools process is expected with sandboxing enabled"
Chelix starts the host-side tools service even when the default session uses a
sandbox. Sessions can disable sandboxing or use per-session overrides at
runtime, so the host endpoint remains available for host-routed sessions.

A host-side `chelix-tools-service` process does not mean that a sandboxed tool
call is running on the host. The session router selects the endpoint for every
call.
```

## Session routing

`ripgrep` includes the internal session key in its service call. The managed
client asks `SandboxRouter` for that session's execution environment:

- **Host environment** — use the managed loopback host endpoint.
- **Sandbox environment** — request the authenticated endpoint from the selected
  sandbox backend.

The sandbox path is fail-closed. If the selected backend does not expose a
managed tools-service endpoint, the tool call returns an error instead of
falling back to host-side `rg` execution.

```text
ripgrep tool
    │
    ├── session routes to host ───────► host tools service
    │                                  127.0.0.1:<random>
    │
    └── session routes to sandbox ────► sandbox tools service
                                       backend-selected endpoint
```

## Protocol and authentication

The shared wire types live in `chelix-protocol`. The current protocol version is
`1`.

| Method | Path          | Purpose                                      |
| ------ | ------------- | -------------------------------------------- |
| `GET`  | `/v1/health`  | Return the tools-service protocol version    |
| `POST` | `/v1/ripgrep` | Execute the `ripgrep` tool request and reply |

Both routes require an exact `Authorization: Bearer <token>` header. An invalid
or missing token returns `401 Unauthorized`. A valid `ripgrep` request that
cannot be executed returns `422 Unprocessable Entity` with the service error
body.

The gateway checks both HTTP success and `protocolVersion` before accepting a
host or sandbox endpoint.

## Container sandbox lifecycle

The deterministic sandbox image installs `ripgrep` and copies
`chelix-tools-service` to `/usr/local/bin/chelix-tools-service`. The service
replaces the previous idle sleep bootstrap as the container's long-running
workload:

```text
chelix-tools-service --listen 0.0.0.0:43271
```

The backend generates a token for each container and passes it through
`CHELIX_TOOLS_SERVICE_TOKEN`. Chelix keeps the selected endpoint and token in the
backend's in-memory runtime map. `ToolsServiceEndpoint` redacts the token from
its `Debug` output.

### Docker and Podman

Docker and Podman publish container port `43271` to a random port bound to the
container host's loopback interface:

```text
127.0.0.1::43271
```

Chelix then obtains two transport classes from the OCI runtime:

1. the published host-loopback port from `docker port` or `podman port`;
2. the container IPv4 address from backend-specific `inspect` fields (Docker's
   per-network endpoint data or Podman's native network settings).

Candidates are discovered once for each container generation and tried in that
order. Readiness retries repeat only the authenticated `/v1/health` probes;
they do not respawn `docker port` or `docker inspect` on every attempt. Chelix
selects the first endpoint whose health response reports protocol version `1`.

This supports both gateway topologies:

- A gateway running directly on the container host can reach the published
  `127.0.0.1:<random-port>` endpoint.
- A containerized gateway on a network that can reach the sandbox container can
  use the sandbox's direct address on port `43271`.

A published address bound to `127.0.0.1` is on the Docker host's loopback; it is
not the loopback interface of another container. Docker containers on the same
user-defined bridge network can communicate through their container addresses.
See Docker's documentation for
[bridge networks](https://docs.docker.com/engine/network/drivers/bridge/) and
[port publishing](https://docs.docker.com/engine/network/port-publishing/).

If no candidate passes the authenticated readiness check, startup of that
sandbox fails and Chelix runs `docker rm -f` or `podman rm -f` for the container
instead of retaining an unready sandbox.

Before each sandbox-routed tools-service call, the router prepares the session
again. Docker and Podman verify both that the container is running and that the
cached endpoint still passes authenticated health. A stopped, removed, or
OOM-killed container is recreated with a new token, published port, and
container address. If the container is still running but its network endpoint
changed, Chelix performs fresh endpoint discovery before deciding to recreate
it.

A container can also disappear after this preflight check and before or during
the HTTP request. For that race, an availability, authentication, or protocol
failure causes one sandbox re-prepare and one retry using the newly selected
endpoint. A valid `422 Unprocessable Entity` tool error is not retried. Recovery
remains fail-closed and never reroutes a sandbox call to the host service.

### Apple Container

The Apple Container backend reserves an available host loopback port before
launch, publishes it explicitly to container port `43271`, and waits for the
same authenticated protocol-version health check. The service remains the
container workload.

## Deterministic image identity

The current sandbox image tag is a SHA-256 digest over:

1. the generated Dockerfile bytes;
2. one zero-byte separator;
3. the exact Linux `chelix-tools-service` artifact bytes copied into the image.

Changing the service binary therefore changes the sandbox image tag even when
the configured base image and package list are unchanged.

Gateway startup awaits image preparation before starting the managed host
service or registering tools. An image preparation error aborts startup; image
creation is not deferred to the first tool call.

## Binary discovery and packaging

The host service binary is resolved in this order:

1. `CHELIX_TOOLS_SERVICE_BINARY`;
2. a `chelix-tools-service` sibling next to the running Chelix executable;
3. the executable's development profile directory when running from `deps`;
4. `PATH`.

Sandbox image construction requires a Linux ELF artifact:

- On Linux, Chelix uses the sibling `chelix-tools-service` artifact.
- On macOS, Chelix expects a sibling named
  `chelix-tools-service-linux-aarch64` or
  `chelix-tools-service-linux-x86_64`, matching the host architecture.
- `CHELIX_TOOLS_SERVICE_LINUX_BINARY` overrides sandbox artifact discovery.

Chelix validates the Linux ELF magic before using the artifact. The release
installer validates the required gateway and tools-service payload before
replacing installed binaries. macOS release payloads also include the matching
Linux sandbox artifact.

## Troubleshooting

### Inspect the host-side process

```bash
ps -ax -o pid=,command= | grep '[c]helix-tools-service'
```

When Chelix itself runs in Docker, the managed host process is inside the Chelix
container:

```bash
docker top chelix
```

Seeing this process is expected even when sessions use container sandboxes.

### Inspect the sandbox workload and transport

```bash
# Find Chelix sandbox containers
docker ps --filter 'name=chelix-' --format '{{.Names}}\t{{.Ports}}\t{{.Status}}'

# Inspect the selected sandbox container
docker top <sandbox-container>
docker port <sandbox-container> 43271/tcp
docker inspect --format '{{range .NetworkSettings.Networks}}{{println .IPAddress}}{{end}}' <sandbox-container>
```

Do not print or copy `CHELIX_TOOLS_SERVICE_TOKEN` while collecting diagnostics.
The authenticated readiness probe is performed by Chelix from the gateway's
actual network namespace.

### Containerized gateway networking

For a containerized gateway, attach the gateway and its sandboxes to a shared
user-defined bridge network and configure the sandbox backend to use that
network:

```bash
docker network create chelix-sandbox-net
```

```toml
[sandbox]
network = "chelix-sandbox-net"
```

Start the Chelix container with `--network=chelix-sandbox-net`. New sandbox
containers receive the same `--network` value, allowing endpoint discovery to
select a direct container address when the Docker-host loopback publication is
not reachable from the gateway container.
