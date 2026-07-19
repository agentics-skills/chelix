# Sandbox Backends

Chelix has one global sandbox policy for command, filesystem, browser, cron,
skill, and managed-tools execution. Enabled sandboxing always uses a
filesystem-isolated runtime; direct host execution is available only when the
global policy is explicitly `"Off"`.

## Environment Variables

Manage sandbox command environment variables at `/settings/environment`.

- **Secret** defaults to enabled. Secret values are masked in the settings list
  and redacted from command output. Disable it for ordinary configuration values
  that should remain visible in both places.
- **Enabled** defaults to enabled. Disabled variables remain stored in the list
  but are not injected into sandbox commands.
- Vault encryption at rest is independent of **Secret**. Changing visibility
  does not change how the value is stored.

## Backend Selection

Configure in `chelix.toml`:

```toml
[sandbox]
mode = "On"
backend = "auto"          # default — picks the best available
# backend = "podman"      # force Podman (daemonless, rootless)
# backend = "docker"      # force Docker
# backend = "apple-container"  # force Apple Container (macOS only)
# backend = "wasm"        # force WASM sandbox (Wasmtime + WASI)
```

Only `"auto"`, `"docker"`, `"podman"`, `"apple-container"`, and `"wasm"`
are accepted. Legacy values such as `"restricted-host"` and `"cgroup"` are
configuration errors.

With `"auto"` (the default), Chelix selects the first available isolated
container runtime:

| Priority | Backend         | Platform | Isolation                               |
| -------- | --------------- | -------- | --------------------------------------- |
| 1        | Apple Container | macOS    | VM (Virtualization.framework)           |
| 2        | Podman          | any      | Linux namespaces / cgroups (daemonless) |
| 3        | Docker          | any      | Linux namespaces / cgroups              |

The WASM backend (`backend = "wasm"`) is not in the auto-detect chain because it
cannot execute arbitrary shell commands — use it explicitly when you want
WASI-isolated execution.

When `mode = "On"`, an unavailable explicit backend or an `"auto"` selection
with no available isolated runtime aborts gateway startup. Chelix never falls
back to host execution. `mode = "Off"` is the only direct host execution path.

## Apple Container (recommended on macOS)

[Apple Container](https://github.com/apple/container) runs each sandbox in a
lightweight virtual machine using Apple's Virtualization.framework. Every
container gets its own kernel, so a kernel exploit inside the sandbox cannot
reach the host — unlike Docker, which shares the host kernel.

### Install

Download the signed installer from GitHub:

```bash
# Download the installer package
gh release download --repo apple/container --pattern "container-installer-signed.pkg" --dir /tmp

# Install (requires admin)
sudo installer -pkg /tmp/container-installer-signed.pkg -target /

# First-time setup — downloads a default Linux kernel
container system start
```

Alternatively, build from source with `brew install container` (requires Xcode
26+).

### Verify

```bash
container --version
# Run a quick test
container run --rm ubuntu echo "hello from VM"
```

Once installed, restart `chelix gateway` — the startup banner will show
`sandbox: apple-container backend`.

## Podman

[Podman](https://podman.io/) is a daemonless, rootless container engine that is
CLI-compatible with Docker. It is preferred over Docker in auto-detection
because it doesn't require a background daemon process and runs rootless by
default for better security.

### Install

```bash
# macOS
brew install podman
podman machine init && podman machine start

# Debian/Ubuntu
sudo apt-get install -y podman

# Fedora/RHEL
sudo dnf install -y podman
```

### Verify

```bash
podman --version
podman run --rm docker.io/library/ubuntu echo "hello from podman"
```

Once installed, restart `chelix gateway` — the startup banner will show
`sandbox: podman backend`. All Docker hardening flags (see below) apply
identically to Podman containers.

## Docker

Docker is supported on macOS, Linux, and Windows. On macOS it runs inside a
Linux VM managed by Docker Desktop, so it is reasonably isolated but adds more
overhead than Apple Container.

Install from <https://docs.docker.com/get-docker/>

### Docker/Podman Hardening

Docker and Podman containers launched by Chelix include the following security
hardening flags by default:

| Flag                                         | Effect                                                                                    |
| -------------------------------------------- | ----------------------------------------------------------------------------------------- |
| `--cap-drop ALL`                             | Drops all Linux capabilities when `workspace_sysmount = "ro"`                             |
| `--security-opt no-new-privileges`           | Prevents privilege escalation via setuid/setgid binaries when `workspace_sysmount = "ro"` |
| `--tmpfs /tmp:rw,nosuid,size=256m`           | Writable tmpfs for temp files (noexec on real root)                                       |
| `--tmpfs /run:rw,nosuid,size=64m`            | Writable tmpfs for runtime files                                                          |
| `--read-only`                                | Read-only root filesystem (prebuilt images with `workspace_sysmount = "ro"`)              |
| `--hostname sandbox`                         | Prevents host hostname leakage                                                            |
| `--tmpfs /sys/firmware:ro,nosuid`            | Masks BIOS/UEFI firmware data (Docker only)                                               |
| `--tmpfs /sys/class/dmi:ro,nosuid`           | Masks system serial numbers and identifiers (Docker only)                                 |
| `--tmpfs /sys/devices/virtual/dmi:ro,nosuid` | Masks DMI attributes (Docker only)                                                        |
| `--tmpfs /sys/class/block:ro,nosuid`         | Masks block device info (Docker only)                                                     |

With `workspace_sysmount = "ro"` (the default), Docker/Podman sandbox containers
keep `--cap-drop ALL` and `--security-opt no-new-privileges`, and prebuilt
sandbox images also keep `--read-only`.

With `workspace_sysmount = "rw"`, Chelix skips `--cap-drop ALL`,
`--security-opt no-new-privileges`, and `--read-only` so package managers can
work against a writable root filesystem.

```toml
[sandbox]
workspace_sysmount = "rw"   # default: "ro"
```

The `/sys` tmpfs overlays prevent host hardware metadata (serial numbers, disk
models, LUKS UUIDs) from being visible inside the container. Note that
`tools.fs.deny_paths` only restricts Chelix file-access tools — these kernel
filesystem masks prevent leakage via shell commands as well.

> **Podman note:** The sysfs tmpfs overlays are applied on Docker only. Podman's
> OCI runtime performs "tmpcopyup" when mounting tmpfs over sysfs paths, which
> fails under `--cap-drop ALL` because some sysfs files are permission-denied
> even for root. Podman masks `/sys/firmware` via its built-in OCI
> `MaskedPaths`; `/sys/class/dmi`, `/sys/devices/virtual/dmi`, and
> `/sys/class/block` remain readable inside the container on Podman.

## WASM Sandbox (Wasmtime + WASI)

The WASM sandbox provides real sandboxed execution using
[Wasmtime](https://wasmtime.dev/) with WASI. Commands execute in an isolated
filesystem tree with fuel metering and epoch-based timeout enforcement.

### How It Works

The WASM sandbox has two execution tiers:

**Tier 1 — Built-in commands** (~20 common coreutils implemented in Rust):
`echo`, `cat`, `ls`, `mkdir`, `rm`, `cp`, `mv`, `pwd`, `env`, `head`, `tail`,
`wc`, `sort`, `touch`, `which`, `true`, `false`, `test`/`[`, `basename`,
`dirname`.

These operate on a sandboxed directory tree, translating guest paths (e.g.
`/home/sandbox/file.txt`) to host paths under `~/.chelix/sandbox/wasm/<id>/`.
Paths outside the sandbox root are rejected.

Basic shell features are supported: `&&`, `||`, `;` sequences, `$VAR` expansion,
quoting via `shell-words`, and `>` / `>>` output redirects.

**Tier 2 — Real WASM module execution**: When the command references a `.wasm`
file, it is loaded and run via Wasmtime + WASI preview1 with full isolation:
preopened directories, fuel metering, epoch interruption, and captured I/O.

**Unknown commands** return exit code 127: "command not found in WASM sandbox".

### Filesystem Isolation

```
~/.chelix/sandbox/wasm/<session-key>/
  home/        preopened as /home/sandbox (rw)
  tmp/         preopened as /tmp (rw)
```

Home persistence is respected:

- `shared`: uses `data_dir()/sandbox/home/shared/wasm/`
- `session`: uses `data_dir()/sandbox/wasm/<session-id>/`
- `off`: per-session, cleaned up on `cleanup()`

### Resource Limits

- **Fuel metering**: `store.set_fuel(fuel_limit)` — limits WASM instruction
  count (Tier 2 only)
- **Epoch interruption**: background thread ticks epochs, store traps on
  deadline (Tier 2 only)
- **Memory**: `wasm_config.memory_reservation(bytes)` — Wasmtime memory limits
  (Tier 2 only)

### Configuration

```toml
[sandbox]
backend = "wasm"

# WASM-specific settings
wasm_fuel_limit = 1000000000       # instruction fuel (default: 1 billion)
wasm_epoch_interval_ms = 100       # epoch interruption interval (default: 100ms)

[sandbox.resource_limits]
memory_limit = "512M"    # Wasmtime memory reservation
```

### Limitations

- Built-in commands cover common coreutils but not a full shell
- No pipe support yet (planned via busybox.wasm in future)
- No network access from WASM modules
- `.wasm` modules must target WASI preview1

### When to Use

The WASM sandbox is a good fit when:

- You want filesystem-isolated execution without container overhead
- You need a sandboxed environment on platforms without Docker or Apple
  Container
- You are running `.wasm` modules and want fuel-metered, time-bounded execution

### Compile-Time Feature

The WASM sandbox is gated behind the `wasm` cargo feature, which is enabled by
default. To build without Wasmtime (saves ~30 MB binary size):

```bash
cargo build --release --no-default-features --features lightweight
```

When the feature is disabled and the config requests `backend = "wasm"`, Chelix
returns a startup error.

## Failover Chain

When `backend = "auto"` selects Apple Container on macOS, Chelix can attach one
isolated fallback: Podman when available, otherwise Docker. A non-isolated
primary or fallback is rejected when the chain is constructed.

Failover is sticky for the lifetime of the gateway process — once triggered, all
subsequent commands use the fallback backend. Restart the gateway to retry the
primary backend.

If no isolated fallback is available, Apple Container remains the sole backend;
an execution failure is returned rather than routed to the host.

## Global mode

Sandbox execution is controlled only by the exact global value in
`chelix.toml`:

```toml
[sandbox]
mode = "On" # or "Off"
```

The values are case-sensitive. Other spellings and legacy values are rejected.
With `"On"`, gateway startup requires a filesystem-isolated runtime. With
`"Off"`, execution runs directly on the host and no sandbox runtime is created.

There are no agent, session, heartbeat, project, chat, cron, skill, or browser
sandbox overrides.

The Sandboxes settings page displays this value as a read-only `On`/`Off`
indicator. Change the policy in `chelix.toml` and restart Chelix.

## Shared data directory

Every isolated backend mounts Chelix's `data_dir()` read-write at the identical
absolute guest path. This mandatory mount keeps databases, sessions, memory,
logs, installed skill folders, and skill-owned scripts or binaries visible to
both the host process and sandbox runtime.

Additional mounts are declarative:

```toml
[[sandbox.mounts]]
host = "/srv/reference"
guest = "/mnt/reference"
mode = "ro"
```

The mandatory data mount cannot be redirected to a different guest path.

## Home persistence

By default, `/home/sandbox` is persisted in a shared host folder so that CLI
auth/config files survive container recreation. You can change this with
`home_persistence`:

```toml
[sandbox]
home_persistence = "session"   # "off", "session", or "shared" (default)
# shared_home_dir = "/path/to/shared-home"  # optional, used when mode is "shared"
```

- `off`: no home mount, container home is ephemeral
- `session`: mount a per-session host folder to `/home/sandbox`
- `shared`: mount one shared host folder to `/home/sandbox` for all sessions
  (defaults to `data_dir()/sandbox/home/shared`, or `shared_home_dir` if set)

Chelix stores persisted homes under `data_dir()/sandbox/home/`.

## Docker-in-Docker data directory mounts

When Chelix runs inside a container and launches Docker-backed sandboxes via a
mounted container socket, the sandbox bind mount source must be a host-visible
path. Chelix auto-detects this by inspecting the parent container's mounts. If
that lookup fails or you want to pin the value explicitly, set `host_data_dir`:

```toml
[sandbox]
host_data_dir = "/srv/chelix/data"
```

This changes the host-visible source of the mandatory `data_dir()` mount and
default sandbox persistence paths. The guest path remains the agent's absolute
`data_dir()` path and the data mount remains read-write. This option is mainly
for Docker-in-Docker deployments where mount auto-detection is unavailable or
ambiguous.

## Managed tools service

Filesystem tools that require native executables run through the managed
`chelix-tools-service` runtime. With global `sandbox.mode = "On"`, the service
is the long-running workload in Docker, Podman, and Apple Container sandboxes;
`ripgrep` is installed in the sandbox image and executed there. Chelix does not
start a local `chelix-tools-service` process in this mode.

With global `sandbox.mode = "Off"`, Chelix starts the host-side service and does
not use the container tools-service runtime. The two runtimes are mutually
exclusive.

Docker and Podman endpoint readiness is checked from Chelix's own network
namespace. Chelix tries the random host-loopback publication and inspect-derived
container addresses, then retains the first endpoint that passes authenticated
protocol health. If no endpoint becomes ready, Chelix removes the failed
container. See [Managed Tools Service](tools-service.md) for the protocol,
process lifecycle, deterministic image identity, and troubleshooting commands.

## Container network

Docker and Podman sandboxes use the configured container network directly. The
default is `bridge`, and Chelix passes the value to the container runtime as
`--network=<name>`.

```toml
[sandbox]
network = "bridge"
```

Use any runtime network name accepted by Docker or Podman, for example a custom
network created outside Chelix:

```toml
[sandbox]
network = "chelix-sandbox-net"
```

Runtime-provided values such as `none` or `host` are passed through unchanged;
Chelix no longer has sandbox-specific network policy modes.

> **Note**: Home persistence applies to Docker, Podman, Apple Container, and
> WASM backends.

## Resource limits

```toml
[sandbox.resource_limits]
memory_limit = "512M"
cpu_quota = 1.0
pids_max = 256
```

How resource limits are applied depends on the backend:

Chelix prepares the current deterministic sandbox image during gateway startup
and waits for every available image-building backend. The image identity
includes the generated Dockerfile and the exact Linux `chelix-tools-service`
bytes copied into it. A missing artifact or failed image build aborts startup;
there is no lazy first-call rebuild.

Docker and Podman sandboxes use one CPU by default. Set `cpu_quota` to override
that launch limit.

| Limit          | Docker/Podman  | Apple Container | WASM                 |
| -------------- | -------------- | --------------- | -------------------- |
| `memory_limit` | `--memory`     | `--memory`      | Wasmtime reservation |
| `cpu_quota`    | `--cpus`       | `--cpus`        | epoch timeout        |
| `pids_max`     | `--pids-limit` | `--pids-limit`  | n/a                  |

## Comparison

| Feature               | Apple Container    | Docker/Podman    | WASM              | Mode `Off`       |
| --------------------- | ------------------ | ---------------- | ----------------- | ---------------- |
| Filesystem isolation  | ✅ VM boundary     | ✅ namespaces    | ✅ sandboxed tree | ❌ host FS       |
| Network isolation     | ✅                 | ✅               | ✅ (no network)   | ❌               |
| Kernel isolation      | ✅ separate kernel | ❌ shared kernel | ✅ WASM VM        | ❌               |
| Environment isolation | ✅                 | ✅               | ✅                | ❌               |
| Resource limits       | ✅                 | ✅               | ✅ fuel + epoch   | ❌               |
| Image building        | ✅ (via Docker)    | ✅               | ❌                | ❌               |
| Shell commands        | ✅ full shell      | ✅ full shell    | ~20 built-ins     | ✅ direct host   |
| Platform              | macOS 26+          | any              | any               | any              |
