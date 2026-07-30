pub(crate) const EXAMPLE_HOOK_MD: &str = r#"+++
name = "example"
description = "Skeleton hook — edit this to build your own"
emoji = "🪝"
events = ["BeforeToolCall"]
# command = "./handler.sh"
# timeout = 10
# priority = 0

# [requires]
# os = ["darwin", "linux"]
# bins = ["jq", "curl"]
# env = ["SLACK_WEBHOOK_URL"]
+++

# Example Hook

This is a skeleton hook to help you get started. It subscribes to
`BeforeToolCall` but has no `command`, so it won't execute anything.

## Quick start

1. Uncomment the `command` line above and point it at your script
2. Create `handler.sh` (or any executable) in this directory
3. Click **Reload** in the Hooks UI (or restart chelix)

## How hooks work

Your script receives the event payload as **JSON on stdin** and communicates
its decision via **exit code** and **stdout**:

| Exit code | Stdout | Action |
|-----------|--------|--------|
| 0 | *(empty)* | **Continue** — let the action proceed |
| 0 | `{"action":"modify","data":{...}}` | **Modify** — alter the payload |
| 1 | *(stderr used as reason)* | **Block** — prevent the action |

## Example handler (bash)

```bash
#!/usr/bin/env bash
# handler.sh — log every tool call to a file
payload=$(cat)
tool=$(echo "$payload" | jq -r '.tool_name // "unknown"')
echo "$(date -Iseconds) tool=$tool" >> /tmp/chelix-hook.log
# Exit 0 with no stdout = Continue
```

## Available events

**Can modify or block (sequential dispatch):**
- `BeforeAgentStart` — before a new agent run begins
- `BeforeLLMCall` — before a prompt is sent to the LLM provider
- `AfterLLMCall` — after an LLM response arrives, before any tool execution
- `BeforeToolCall` — before executing a tool (inspect/modify arguments)
- `MessageReceived` — when an inbound channel/UI message arrives;
  `Block(reason)` rejects it, `ModifyPayload({"content": "..."})` rewrites
  the text before the turn begins
- `MessageSending` — before sending a message to the LLM
- `ToolResultPersist` — before persisting a tool result

**Read-only (parallel dispatch, Block/Modify ignored):**
- `AfterToolCall` — after a tool finishes (observe result)
- `MessageSent` — after a message is sent
- `SessionStart` / `SessionEnd` — session lifecycle
- `GatewayStart` / `GatewayStop` — server lifecycle

## Frontmatter reference

```toml
name = "my-hook"           # unique identifier
description = "What it does"
emoji = "🔧"               # optional, shown in UI
events = ["BeforeToolCall"] # which events to subscribe to
command = "./handler.sh"    # script to run (relative to this dir)
timeout = 10                # seconds before kill (default: 10)
priority = 0                # higher runs first (default: 0)

[requires]
os = ["darwin", "linux"]    # skip on other OSes
bins = ["jq"]               # required binaries in PATH
env = ["MY_API_KEY"]        # required environment variables
```
"#;

pub(crate) const EXAMPLE_SKILL_MD: &str = r#"---
name: template-skill
description: Starter skill template (safe to copy and edit)
---

# Template Skill

Use this as a starting point for your own skills.

## How to use

1. Copy this folder to a new skill name (or edit in place)
2. Update `name` and `description` in frontmatter
3. Replace this body with clear, specific instructions

## Tips

- Keep instructions explicit and task-focused
- Avoid broad permissions unless required
- Document required tools and expected inputs
"#;

pub(crate) const TMUX_SKILL_MD: &str = r#"---
name: tmux
description: Run and interact with terminal applications (htop, vim, etc.) using tmux sessions in the sandbox
allowed-tools:
  - process
---

# tmux — Interactive Terminal Sessions

Use the `process` tool to run and interact with interactive or long-running
programs inside the sandbox. Every command runs in a named **tmux session**,
giving you full control over TUI apps, REPLs, and background processes.

## When to use this skill

- **TUI / ncurses apps**: htop, vim, nano, less, top, iftop
- **Interactive REPLs**: python3, node, irb, psql, sqlite3
- **Long-running commands**: tail -f, watch, servers, builds
- **Programs that need keyboard input**: anything that waits for keypresses

For simple one-shot commands (ls, cat, echo), use `execute_command` instead.

## Workflow

1. **Start** a session with a command
2. **Poll** to see the current terminal output
3. **Send keys** or **paste text** to interact
4. **Poll** again to see the result
5. **Kill** when done

Always poll after sending keys — the terminal updates asynchronously.

## Actions

### start — Launch a program

```json
{"action": "start", "command": "htop", "session_name": "my-htop"}
```

- `session_name` is optional (auto-generated if omitted)
- The command runs in a 200x50 terminal

### poll — Read terminal output

```json
{"action": "poll", "session_name": "my-htop"}
```

Returns the visible pane content (what a user would see on screen).

### send_keys — Send keystrokes

```json
{"action": "send_keys", "session_name": "my-htop", "keys": "q"}
```

Common key names:
- `Enter`, `Escape`, `Tab`, `Space`
- `Up`, `Down`, `Left`, `Right`
- `C-c` (Ctrl+C), `C-d` (Ctrl+D), `C-z` (Ctrl+Z)
- `C-l` (clear screen), `C-a` / `C-e` (line start/end)
- Single characters: `q`, `y`, `n`, `/`

### paste — Insert text

```json
{"action": "paste", "session_name": "repl", "text": "print('hello world')\n"}
```

Use paste for multi-character input (code, file content). For single
keystrokes, prefer `send_keys`.

### kill — End a session

```json
{"action": "kill", "session_name": "my-htop"}
```

### list — Show active sessions

```json
{"action": "list"}
```

## Tips

- Session names must be `[a-zA-Z0-9_-]` only (no spaces or special chars)
- Always `kill` sessions when done to free resources
- If a program is unresponsive, `send_keys` with `C-c` or `C-\\` first
- Poll output is a snapshot; poll again for updates after sending input
"#;

pub(crate) const DEFAULT_BOOT_MD: &str = r#"<!--
BOOT.md is optional startup context.

How Chelix uses this file:
- Loaded per session and injected into the system prompt.
- Missing/empty/comment-only file = no startup injection.
- Agent-specific overrides: place in agents/<id>/BOOT.md.

Recommended usage:
- Keep it short and explicit.
- Use for startup checks/reminders, not onboarding identity setup.
-->"#;

pub(crate) const DEFAULT_WORKSPACE_AGENTS_MD: &str = r#"<!--
Workspace AGENTS.md contains global instructions for this workspace.

How Chelix uses this file:
- Loaded from data_dir/AGENTS.md when present.
- Injected as workspace context in the system prompt.
- Separate from project AGENTS.md/CLAUDE.md discovery.

Use this for cross-project rules that should apply everywhere in this workspace.
-->"#;

pub(crate) const DEFAULT_TOOLS_MD: &str = r#"<!--
TOOLS.md contains workspace-specific tool notes and constraints.

How Chelix uses this file:
- Loaded from data_dir/TOOLS.md when present.
- Injected as workspace context in the system prompt.

Use this for local setup details (hosts, aliases, device names) and
tool behavior constraints (safe defaults, forbidden actions, etc.).
-->"#;

pub(crate) const DEFAULT_HEARTBEAT_MD: &str = r#"<!--
HEARTBEAT.md is an optional heartbeat prompt source.

Prompt precedence:
1) heartbeat.prompt from config
2) HEARTBEAT.md
3) built-in default prompt

Cost guard:
- If HEARTBEAT.md exists but is empty/comment-only and there is no explicit
  heartbeat.prompt override, Chelix skips heartbeat LLM turns to avoid token use.
-->"#;
