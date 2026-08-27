# LLM Providers

Chelix supports multiple LLM providers through a trait-based architecture.
Configure providers through the web UI or directly in configuration files.

## Model Registry Business Logic

### Configuration Source

The only source of model composition and model parameters is the service
configuration.

The registry is built only from `[providers.<name>.models."<model-id>"]` tables.

The `/models` request is forbidden and is never performed.

Defaults are forbidden for all parameters. A missing optional parameter is not
substituted and is not sent to the provider.

### Registry Record Contents

A registry record contains the model ID, the provider name, and the parameters
below. The record has no other fields.

### Mandatory Parameters

- `context_length`
- `max_input_tokens`
- `max_output_tokens`
- `input_modalities`
- `output_modalities`
- `tool_calling`
- `streaming`
- `zeroDataRetentionEnabled`
- `reasoning_supported_efforts`

### Optional Parameters

- `reasoning_summary`
- `reasoning_include`

A `reasoning_include` value goes to the API with a prefix: `encrypted_content`
is sent as `reasoning.encrypted_content`.

### Value Validity Criteria

- `context_length`, `max_input_tokens`, `max_output_tokens` — greater than zero
- `max_input_tokens + max_output_tokens` does not exceed `context_length`
- `input_modalities`, `output_modalities` — non-empty, without duplicates
- `reasoning_include` — without duplicates

### Non-Reasoning Model

A non-reasoning model is defined explicitly by an empty array:

```toml
reasoning_supported_efforts = []
```

When `reasoning_supported_efforts` is empty, the `reasoning_summary` and
`reasoning_include` parameters are forbidden.

### supported_efforts

A non-empty `reasoning_supported_efforts` makes the model reasoning-capable
regardless of its contents: `["none"]` and `["off"]` are reasoning-capable, as
is any other explicitly specified value.

There is no interference with or restriction on the set of
`reasoning_supported_efforts` levels listed in the configuration.

Forbidden: a local value allowlist, filtering, renaming, replacement,
reordering, autocompletion.

### Load Refusal

Service load refusal is caused by:

- a missing mandatory parameter
- a value violating the validity criteria
- `reasoning_summary` or `reasoning_include` with an empty
  `reasoning_supported_efforts`
- an unknown key in the model settings (the common configuration validator)
- an enabled provider without a single model

The registry is built atomically: a partial registry is not published, a
problematic model is not excluded for the sake of continuing startup.

The error contains the provider, the model ID, and the field name when the
field is applicable.

### Example: reasoning model

```toml
[providers.custom-meta.models."muse-spark-1.2"]
context_length = 1048576
max_input_tokens = 983040
max_output_tokens = 65536
input_modalities = ["text", "image", "audio", "file"]
output_modalities = ["text"]
tool_calling = true
streaming = true
zeroDataRetentionEnabled = true
reasoning_supported_efforts = ["low", "medium", "high"]
reasoning_summary = "detailed"
reasoning_include = ["encrypted_content"]
```

### Example: non-reasoning model

```toml
[providers.custom-meta.models."muse-flash-0.9"]
context_length = 262144
max_input_tokens = 196608
max_output_tokens = 65536
input_modalities = ["text"]
output_modalities = ["text"]
tool_calling = true
streaming = true
zeroDataRetentionEnabled = false
reasoning_supported_efforts = []
```

## Available Providers

### API Key Providers

| Provider             | Config Name  | Env Variable         | Features                                                         |
| -------------------- | ------------ | -------------------- | ---------------------------------------------------------------- |
| **OpenAI**           | `openai`     | `OPENAI_API_KEY`     | Streaming, tools, vision                        |
| **OpenRouter**       | `openrouter` | `OPENROUTER_API_KEY` | Streaming, tools                                |
| **Z.AI (Zhipu)**     | `zai`        | `Z_API_KEY`          | Streaming, tools                                |
| **Z.AI Coding Plan** | `zai-code`   | `Z_CODE_API_KEY`     | Streaming, tools (Coding plan billing endpoint) |

### Custom OpenAI-Compatible

Any OpenAI-compatible endpoint can be added with a `custom-` prefix. This is
the canonical complete-record format:

```toml
[providers.custom-ai-example]
enabled = true
base_url = "https://ai.example.invalid/v1"
wire_api = "responses"

[providers.custom-ai-example.models."Combos/cx/gpt-sol"]
context_length = 400000
max_input_tokens = 272000
max_output_tokens = 128000
input_modalities = ["text", "image", "audio", "file"]
output_modalities = ["text"]
tool_calling = true
streaming = true
zeroDataRetentionEnabled = true
reasoning_supported_efforts = ["none", "minimal", "low", "medium", "high", "xhigh"]
reasoning_summary = "detailed"
reasoning_include = ["encrypted_content"]
```

### OpenAI-Compatible Tool Schemas

OpenAI-compatible Chat Completions and Responses requests send native function
tools with `strict: false`. There is no `strict_tools` provider setting.

Before a tool reaches the wire, Chelix checks its `parameters` schema. The rules
are the same for every provider and every model — no rule is selected by model
name or family:

| Rule | Behaviour |
|------|-----------|
| The schema is a valid JSON Schema draft-07 document | Refused otherwise |
| The root declares `"type": "object"` and a `properties` map | Refused otherwise |
| The root carries no `oneOf`, `anyOf`, `allOf`, `not`, `if`, `then`, or `else` | Refused otherwise |
| Every `"type": "array"`, at any depth, declares `items` | Refused otherwise |
| Draft-07 tuple `items: [A, B]` | Rewritten to `items: {"anyOf": [A, B]}` |
| A `required` entry with no matching property | Dropped |

Anything else is passed through unchanged. In particular, unions **nested under
a property** are preserved as written: Chelix never collapses them to a single
branch, and never removes optional properties.

A schema that breaks a rule is an error naming the tool, and the request is
refused. The tool is not silently dropped and the schema is not silently
repaired, so a broken tool definition surfaces immediately instead of turning
into an opaque provider `400` or a model that keeps calling a tool wrong.

## Configuration

### Via Web UI

Use **Settings** → **Providers** to save credentials for a provider whose complete
model records are already declared in the service configuration.

The **OpenAI Compatible** entry lists config-declared `custom-*` providers. Select
one to save its API key and API base URL. This flow does not discover models and
does not write model metadata to `provider_keys.json`.

### Via Configuration Files

Configure providers in `chelix.toml`:

```toml
[providers.custom-ai-example]
enabled = true
base_url = "https://ai.example.invalid/v1"
wire_api = "responses"

[providers.custom-ai-example.models."muse-flash-0.9"]
context_length = 262144
max_input_tokens = 196608
max_output_tokens = 65536
input_modalities = ["text"]
output_modalities = ["text"]
tool_calling = true
streaming = true
zeroDataRetentionEnabled = false
reasoning_supported_efforts = []

[chat]
priority_models = ["custom-ai-example::muse-flash-0.9"]
```

### Model Metadata Resolution

The service configuration is the only source of model composition and model
parameters. Each enabled provider must declare at least one complete
`[providers.<name>.models."<raw-model-id>"]` table.

A missing mandatory parameter, an invalid value, an unknown model setting, or an
enabled provider without models refuses service load. The registry is built
atomically: no incomplete registry is published and no problematic model is
excluded to continue startup.

### Provider Entry Options

Each provider supports these options:

| Option             | Default  | Description                                |
| ------------------ | -------- | ------------------------------------------ |
| `enabled`          | `true`   | Enable or disable the provider             |
| `api_key`          | —        | API key (overrides env var)                |
| `base_url`         | —        | Override API endpoint URL                  |
| `models.<model_id>` | —       | Ordered model metadata table               |
| `stream_transport` | `"sse"`  | `"sse"`, `"websocket"`, or `"auto"`        |
| `alias`            | —        | Custom label for metrics                   |
| `tool_mode`        | `"native"` | `"native"`, `"text"`, or `"off"`          |

## Provider Setup

### OpenAI

1. Declare complete model records under
   `[providers.openai.models."<model-id>"]` in the service configuration.
2. Get an API key from [platform.openai.com](https://platform.openai.com/).
3. Set `OPENAI_API_KEY` in your environment, or save the credentials through
   **Settings** → **Providers**. Credentials saved through provider setup are
   persisted in `~/.config/chelix/provider_keys.json` and loaded for the
   matching provider declared in the service configuration.

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
