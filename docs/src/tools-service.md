# Managed Tools Service

Chelix runs migrated tools exclusively through the separate
`chelix-tools-service` binary. The service exists to guarantee that every
migrated tool has one implementation and identical behavior whether the
gateway calls a service inside a sandbox or a locally managed service.

The gateway and every supported container sandbox use the same versioned HTTP
API. Routing changes the service endpoint, not the tool protocol.

## Architectural invariant

For every tool migrated to `chelix-tools-service`:

- the implementation and its runtime state live only in the service;
- the gateway is only an authenticated API client and policy boundary;
- global `sandbox.mode` selects the service endpoint, never another tool
  implementation;
- host execution, sandbox `exec`, remote-node execution, or any other direct
  execution path in the gateway is forbidden;
- service unavailability is an explicit error and never triggers local
  fallback execution.

The currently migrated tools are `list_directory`, `ripgrep`,
`execute_command`, `read_terminal_output`, and `process`.

`execute_command`, `read_terminal_output`, and `process` exclusively use tmux
managed by `chelix-tools-service`. The same tmux implementation and state model
are used by both the local service and the sandbox service. The gateway does
not own terminal state and does not execute these tools through a node or a
direct shell path.

For each `execute_command` run, the service starts a tmux `pipe-pane` capture
before pasting the command. Command completion and the returned output are read
from that complete PTY stream, not from finite tmux scrollback. Capture files
are private, transient service state: their paths are never returned through
the tool protocol, and they are removed when the terminal is reused, disappears,
or the service shuts down.

Transient terminal capture is not tool-result persistence. The agent runner is
the only owner of persistent `content.txt`/`content.json` files, configured
result-size limits, in-context truncation, and the marker that returns a
persisted path to the agent. The tools service must return the complete command
output without applying another byte limit.

The tools-service response is raw protocol state used by Chelix control and UI
code. Before persistence or LLM context, the terminal tool converts it into the
agent-facing text containing the terminal ID, status, and command output. Tmux
session, window, pane, and run identifiers remain protocol metadata and are not
included in that text. String agent results are persisted as `content.txt`;
structured agent results are persisted as `content.json` with `schema.json`.

## Process model

At gateway startup, Chelix:

1. prepares the current deterministic sandbox image;
2. selects one managed runtime from the global `sandbox.mode`;
3. initializes that runtime;
4. registers API-backed tool clients.

- `sandbox.mode = "On"`: only the container tools-service runtime is available.
  Chelix does not start a local `chelix-tools-service` process.
- `sandbox.mode = "Off"`: Chelix starts only the managed host tools-service
  runtime. Tool calls use its authenticated loopback endpoint.

In global `Off` mode, the host service binds to `127.0.0.1:0`, so the operating
system selects an available loopback port. It writes one JSON readiness record
to its stdout pipe. That record contains the protocol version, selected port,
and generated token. The gateway starts the child with
`--shutdown-on-stdin-eof` and configures kill-on-drop for the managed process.

## Global routing

The global `sandbox.mode` selects the only available tools-service runtime:

- **`On`** — request the authenticated endpoint from the selected sandbox
  backend.
- **`Off`** — use the managed loopback host endpoint.

Tool calls include an internal session key only to select the container
lifecycle instance when the configured sandbox scope requires one and to scope
service-owned state such as tmux terminals. It cannot change the global routing
policy.

The `On` path is fail-closed. If the selected backend does not expose a managed
tools-service endpoint, the tool call returns an error instead of falling back
to host-side `rg` execution.

```text
migrated tool
    │
    ├── global Off ───────────────────► host tools service
    │                                  127.0.0.1:<random>
    │
    └── global On ────────────────────► sandbox tools service
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

Before each tools-service call in global `On` mode, the router prepares the
container lifecycle instance again. Docker and Podman verify both that the
container is running and that the cached endpoint still passes authenticated
health. A stopped, removed, or OOM-killed container is recreated with a new
token, published port, and container address. If the container is still running
but its network endpoint changed, Chelix performs fresh endpoint discovery
before deciding to recreate it.

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

Gateway startup awaits image preparation before initializing the selected
managed runtime or registering tools. An image preparation error aborts startup;
image creation is not deferred to the first tool call.

## Binary discovery and packaging

In global `Off` mode, the host service binary is resolved in this order:

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

### Inspect the selected runtime

In global `Off` mode, inspect the host-side process with:

```bash
ps -ax -o pid=,command= | grep '[c]helix-tools-service'
```

When Chelix itself runs in Docker, the managed host process is inside the Chelix
container:

```bash
docker top chelix
```

In global `On` mode, no local `chelix-tools-service` process should be running.
Inspect the sandbox workload instead.

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
