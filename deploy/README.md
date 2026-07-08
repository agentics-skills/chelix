# Chelix Deployment Templates

This directory contains templates for deploying Chelix on a VPS or bare-metal server.

## Files

| File | Purpose |
|------|---------|
| `docker-compose.yml` | Docker Compose for VPS deployment |
| `chelix.service` | systemd unit file for bare-metal installs |

## Docker Compose (recommended)

```bash
cd deploy
export CHELIX_PASSWORD="your-secure-password"
docker compose up -d
```

Open `https://<your-server-ip>:13131` and configure your LLM provider.

## Systemd (bare-metal)

```bash
# Create user and directories
sudo useradd -r -s /usr/sbin/nologin chelix
sudo mkdir -p /var/lib/chelix /etc/chelix
sudo chown chelix:chelix /var/lib/chelix /etc/chelix

# Install the binary
sudo cp chelix /usr/local/bin/chelix

# Install and start the service
sudo cp deploy/chelix.service /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now chelix
```

See the project repository for deployment-related documentation and examples:
<https://github.com/agentics-skills/chelix>.
