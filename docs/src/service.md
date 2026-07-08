# Service Management

Chelix can be installed as an OS service so it starts automatically on boot
and restarts after crashes.

## Install

```bash
chelix service install
```

This creates a service definition and starts it immediately:

| Platform | Service file | Init system |
|----------|-------------|-------------|
| macOS | `~/Library/LaunchAgents/org.chelix.gateway.plist` | launchd (user agent) |
| Linux | `~/.config/systemd/user/chelix.service` | systemd (user unit) |

Both configurations:

- **Start on boot** (`RunAtLoad` / `WantedBy=default.target`)
- **Restart on failure** with a 10-second cooldown
- **Log to** `~/.chelix/chelix.log`

### Options

You can pass `--bind`, `--port`, and `--log-level` to bake them into the
service definition:

```bash
chelix service install --bind 0.0.0.0 --port 8080 --log-level debug
```

These flags are written into the service file. The service reads the rest of
its configuration from `~/.chelix/chelix.toml` as usual.

## Manage

```bash
chelix service status     # Show running/stopped/not-installed and PID
chelix service stop       # Stop the service
chelix service restart    # Restart the service
chelix service logs       # Print the log file path
```

To tail the logs:

```bash
tail -f $(chelix service logs)
```

## Uninstall

```bash
chelix service uninstall
```

This stops the service, removes the service file, and cleans up.

## CLI Reference

| Command | Description |
|---------|-------------|
| `chelix service install` | Install and start the service |
| `chelix service uninstall` | Stop and remove the service |
| `chelix service status` | Show service status and PID |
| `chelix service stop` | Stop the service |
| `chelix service restart` | Restart the service |
| `chelix service logs` | Print log file path |

## How It Differs from `chelix node add`

`chelix service install` manages the **gateway** — the main Chelix server
that hosts the web UI, chat sessions, and API.

`chelix node add` registers a **headless node** — a client process on a
remote machine that connects back to a gateway for command execution. See
[Multi-Node](nodes.md) for details.

| | `chelix service` | `chelix node` |
|---|---|---|
| What it runs | The gateway server | A node client |
| Needs `--host`/`--token` | No | Yes |
| Config source | `~/.chelix/chelix.toml` | `~/.chelix/node.json` |
| launchd label | `org.chelix.gateway` | `org.chelix.node` |
| systemd unit | `chelix.service` | `chelix-node.service` |
