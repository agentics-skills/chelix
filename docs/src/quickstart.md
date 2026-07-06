# Quickstart

Get Chelix running in under 5 minutes.

## 1. Install

```bash
curl -fsSL https://raw.githubusercontent.com/agentics-skills/chelix/master/install.sh | sh
```

## 2. Start

```bash
moltis
```

You'll see output like:

```
🚀 Chelix gateway starting...
🌐 Open http://localhost:13131 in your browser
```

## 3. Configure a Provider

You need an LLM provider configured to chat. The fastest options:

### Option A: API Key (Anthropic, OpenAI, Gemini, etc.)

1. Set an API key as an environment variable and restart Chelix:
   ```bash
   export ANTHROPIC_API_KEY="sk-ant-..."   # Anthropic
   export OPENAI_API_KEY="sk-..."          # OpenAI
   export GEMINI_API_KEY="..."             # Google Gemini
   ```
2. Models appear automatically in the model picker.

Or configure via the web UI: **Settings** → **Providers** → enter your API key.

### Option B: OAuth (Codex / Copilot)

1. In Chelix, go to **Settings** → **Providers**
2. Click **OpenAI Codex** or **GitHub Copilot** → **Connect**
3. Complete the OAuth flow

See [Providers](providers.md) for the full list of supported providers.

## 4. Chat!

Go to the **Chat** tab and start a conversation:

```
You: Write a Python function to check if a number is prime

Agent: Here's a Python function to check if a number is prime:

def is_prime(n):
    if n < 2:
        return False
    for i in range(2, int(n ** 0.5) + 1):
        if n % i == 0:
            return False
    return True
```

## What's Next?

### Enable Tool Use

Chelix can execute code, browse the web, and more. Tools are enabled by default with sandbox protection.

Try:

```
You: Create a hello.py file that prints "Hello, World!" and run it
```

### Connect Telegram

Chat with your agent from anywhere:

1. Create a bot via [@BotFather](https://t.me/BotFather)
2. Copy the bot token
3. In Chelix: **Settings** → **Telegram** → Enter token → **Save**
4. Message your bot!

### Connect Discord

1. Create a bot in the [Discord Developer Portal](https://discord.com/developers/applications)
2. Enable **Message Content Intent** and copy the bot token
3. In Chelix: **Settings** → **Channels** → **Connect Discord** → Enter token → **Connect**
4. Invite the bot to your server and @mention it!

→ [Full Discord setup guide](discord.md)

### Add MCP Servers

Extend capabilities with [MCP servers](mcp.md):

```toml
# In moltis.toml
[mcp]
[mcp.servers.github]
command = "npx"
args = ["-y", "@modelcontextprotocol/server-github"]
env = { GITHUB_TOKEN = "ghp_..." }
```

### Set Up Memory

Enable long-term memory for context across sessions:

```toml
# In moltis.toml
[memory]
provider = "openai"
model = "text-embedding-3-small"
```

Add knowledge by placing Markdown files in `~/.moltis/memory/`.

## Useful Commands

| Command | Description |
|---------|-------------|
| `/new` | Start a new session |
| `/model <name>` | Switch models |
| `/agent` | List or switch chat agents |
| `/mode` | List or switch temporary session modes |
| `/clear` | Clear chat history |
| `/help` | Show available commands |

## File Locations

| Path | Contents |
|------|----------|
| `~/.config/moltis/moltis.toml` | Configuration |
| `~/.config/moltis/provider_keys.json` | API keys |
| `~/.moltis/` | Data (sessions, memory, logs) |

## Getting Help

- **Documentation**: [Chelix docs](index.md)
- **GitHub Issues**: [github.com/agentics-skills/chelix/issues](https://github.com/agentics-skills/chelix/issues)
- **Discussions**: [github.com/agentics-skills/chelix/discussions](https://github.com/agentics-skills/chelix/discussions)
