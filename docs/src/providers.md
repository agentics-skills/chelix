# LLM Providers

Chelix supports multiple LLM providers through a trait-based architecture.
Configure providers through the web UI or directly in configuration files.

## Available Providers

### API Key Providers

| Provider             | Config Name  | Env Variable         | Features                                                         |
| -------------------- | ------------ | -------------------- | ---------------------------------------------------------------- |
| **Anthropic**        | `anthropic`  | `ANTHROPIC_API_KEY`  | Streaming, tools, vision                                         |
| **OpenAI**           | `openai`     | `OPENAI_API_KEY`     | Streaming, tools, vision, model discovery                        |
| **Google Gemini**    | `gemini`     | `GEMINI_API_KEY`     | Streaming, tools, vision, model discovery                        |
| **xAI (Grok)**       | `xai`        | `XAI_API_KEY`        | Streaming                                                        |
| **OpenRouter**       | `openrouter` | `OPENROUTER_API_KEY` | Streaming, tools, model discovery                                |
| **Moonshot (Kimi)**  | `moonshot`   | `MOONSHOT_API_KEY`   | Streaming, tools, model discovery                                |
| **Z.AI (Zhipu)**     | `zai`        | `Z_API_KEY`          | Streaming, tools, model discovery                                |
| **Z.AI Coding Plan** | `zai-code`   | `Z_CODE_API_KEY`     | Streaming, tools, model discovery (Coding plan billing endpoint) |

### OAuth Providers

| Provider           | Config Name      | Notes                                |
| ------------------ | ---------------- | ------------------------------------ |
| **OpenAI Codex**   | `openai-codex`   | OAuth flow via web UI                |
| **GitHub Copilot** | `github-copilot` | Requires active Copilot subscription |

### Custom OpenAI-Compatible

Any OpenAI-compatible endpoint can be added with a `custom-` prefix. This is
the canonical complete-record format:

```toml
[providers.custom-ai-0xff-dad]
enabled = true
base_url = "https://ai.0xff.dad/v1"
wire_api = "responses"

[providers.custom-ai-0xff-dad.models."Combos/cx/gpt-sol"]
context_length = 400000
max_input_tokens = 272000
max_output_tokens = 128000
input_modalities = ["text", "image", "audio", "file"]
output_modalities = ["text"]
tool_calling = true
streaming = true
zeroDataRetentionEnabled = true

[providers.custom-ai-0xff-dad.models."Combos/cx/gpt-sol".reasoning]
supported_efforts = ["none", "minimal", "low", "medium", "high", "xhigh"]
summary = "detailed"
include = ["reasoning.encrypted_content"]
```

For a discovery-backed allowlist, use the same table format without fields:

```toml
[providers.custom-ai-0xff-dad]
enabled = true
base_url = "https://ai.0xff.dad/v1"
wire_api = "responses"

[providers.custom-ai-0xff-dad.models."Combos/cx/gpt-sol"]
[providers.custom-ai-0xff-dad.models."Combos/cx/gpt-mini"]
[providers.custom-ai-0xff-dad.models."Combos/cx/gpt-nano"]
```

Chelix calls `/models` and merges returned metadata into those records.

### OpenAI-Compatible Tool Schemas

OpenAI-compatible Chat Completions and Responses requests send native function
tools with `strict: false`. Chelix preserves each tool schema's declared
`required` array, so properties omitted from that array remain optional and are
not rewritten as required nullable properties. There is no `strict_tools`
provider setting.

## Configuration

### Via Web UI (Recommended)

1. Open Chelix in your browser.
2. Go to **Settings** → **Providers**.
3. Choose a provider card.
4. Complete OAuth or enter your API key.
5. Select your preferred model.

### Via Configuration Files

Configure providers in `chelix.toml`:

```toml
[providers]
offered = ["anthropic", "openai", "gemini"]

[providers.anthropic]
enabled = true

[providers.openai]
enabled = true
stream_transport = "sse"              # "sse", "websocket", or "auto"

[providers.openai.models."gpt-5.3"]
[providers.openai.models."gpt-5.2"]

[providers.gemini]
enabled = true
# api_key = "..."                     # Or set GEMINI_API_KEY / GOOGLE_API_KEY env var
# fetch_models = true                 # Discover models from the API
# base_url = "https://generativelanguage.googleapis.com/v1beta/openai"

[providers.gemini.models."gemini-2.5-flash"]
[providers.gemini.models."gemini-2.5-pro"]

[chat]
priority_models = ["gpt-5.2"]
```

### Model Metadata Resolution

Models are declared only as
`[providers.<name>.models."<raw-model-id>"]` tables. The tables form an ordered
allowlist and preserve declaration order. With no model tables, every
discovered model that resolves completely is accepted.

Chelix resolves each selected model in this order:

1. Configuration metadata wins field by field.
2. Provider `/models` metadata fills fields omitted by configuration.
3. Optional defaults apply only after the merge.
4. Incomplete or inconsistent records are excluded.

The mandatory fields are `context_length`, `max_input_tokens`,
`max_output_tokens`, and `reasoning.supported_efforts`. An empty
`supported_efforts` array explicitly identifies a non-reasoning model.
`reasoning.summary` and `reasoning.include` describe provider request metadata;
they do not enable reasoning or select an effort.

### Provider Entry Options

Each provider supports these options:

| Option             | Default  | Description                                |
| ------------------ | -------- | ------------------------------------------ |
| `enabled`          | `true`   | Enable or disable the provider             |
| `api_key`          | —        | API key (overrides env var)                |
| `base_url`         | —        | Override API endpoint URL                  |
| `models.<model_id>` | —       | Ordered model metadata table               |
| `fetch_models`     | `true`   | Discover available models from the API     |
| `stream_transport` | `"sse"`  | `"sse"`, `"websocket"`, or `"auto"`        |
| `alias`            | —        | Custom label for metrics                   |
| `tool_mode`        | `"auto"` | `"auto"`, `"native"`, `"text"`, or `"off"` |

## Provider Setup

### Google Gemini

Google Gemini uses an API key from
[Google AI Studio](https://aistudio.google.com/).

1. Get an API key from Google AI Studio.
2. Set `GEMINI_API_KEY` in your environment (or use `GOOGLE_API_KEY`).
3. Gemini models appear automatically in the model picker.

```toml
[providers.gemini]
enabled = true

[providers.gemini.models."gemini-2.5-flash"]
[providers.gemini.models."gemini-2.5-pro"]
```

Gemini supports native tool calling, vision/multimodal inputs, streaming, and
automatic model discovery.

### Anthropic

1. Get an API key from [console.anthropic.com](https://console.anthropic.com/).
2. Set `ANTHROPIC_API_KEY` in your environment.

### OpenAI

1. Get an API key from [platform.openai.com](https://platform.openai.com/).
2. Set `OPENAI_API_KEY` in your environment.

### OpenAI Codex

OpenAI Codex uses OAuth-based access.

1. Go to **Settings** → **Providers** → **OpenAI Codex**.
2. Click **Connect** and complete the auth flow.
3. Choose a Codex model.

If the browser cannot reach `localhost:1455`, Chelix now supports a manual
fallback in both **Settings** and **Onboarding**: paste the callback URL (or
`code#state`) into the OAuth panel and submit it.

```admonish note title="Docker and cloud deployments"
The OAuth flow redirects your browser to `localhost:1455`. In Docker, make sure
port 1455 is published (`-p 1455:1455`). On cloud platforms where `localhost`
cannot reach the server, authenticate via the CLI instead:

~~~bash
# Docker
docker exec -it chelix chelix auth login --provider openai-codex
~~~

The CLI opens a browser on your machine and handles the callback locally. If
automatic callback capture fails, the CLI prompts you to paste the callback URL
(or `code#state`) directly in the terminal.
Tokens are saved to the config volume and picked up by the gateway automatically.
```

Once OpenAI Codex OAuth is connected, agents can use the built-in
`generate_image` tool to create `gpt-image-2` images without an
`OPENAI_API_KEY`. Generated images are delivered through the same channel media
path as screenshots and `send_image`, so supported chat channels receive the
image as a native attachment.

### GitHub Copilot

GitHub Copilot uses OAuth authentication.

1. Go to **Settings** → **Providers** → **GitHub Copilot**.
2. Click **Connect**.
3. Complete the GitHub OAuth flow.

```admonish note title="Docker and cloud deployments"
GitHub Copilot uses device-flow authentication (a code you enter on github.com),
so it works from the web UI without extra port configuration. If you prefer the
CLI:

~~~bash
docker exec -it chelix chelix auth login --provider github-copilot
~~~
```

```admonish info
Requires an active GitHub Copilot subscription.
```

## Switching Models

- **Per session**: Use the model selector in the chat UI.
- **Per message**: Use `/model <name>` in chat.
- **Provider selection**: Use ordered
	`[providers.<name>.models."<raw-model-id>"]` tables.
- **Cross-provider ordering**: Use `[chat].priority_models` in `chelix.toml`.

## Troubleshooting

### "Model not available"

- Check provider auth is still valid.
- Check model ID spelling.
- Check account access for that model.

### "Rate limited"

- Retry after a short delay.
- Switch provider/model.
- Upgrade provider quota if needed.

### "Invalid API key"

- Verify the key has no extra spaces.
- Verify it is active and has required permissions.
