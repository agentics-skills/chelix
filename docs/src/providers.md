# LLM Providers

Chelix supports multiple LLM providers through a trait-based architecture.
Configure providers through the web UI or directly in configuration files.

## Model Registry Business Logic

### Configuration Source

The only source of model composition and model parameters is the service
configuration.

The registry is built only from model tables of providers selected by `providers.offered` (an empty list selects all providers) whose `enabled` setting is `true`.

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
- `reasoning_supported_efforts` — a non-empty array without empty strings
- `reasoning_include` — without duplicates

### Non-Reasoning Model

The only configuration that activates the non-reasoning path when calling the
LLM provider API is:

```toml
reasoning_supported_efforts = ["off"]
```

`off` is not mandatory and may be absent from `reasoning_supported_efforts`.
Validation that requires `off` to be present is forbidden.

Until the provider API request is built, `off` is an ordinary selected effort
from `reasoning_supported_efforts`. Configuration, the registry, resolution,
sessions, persistence, and the UI do not classify the model as non-reasoning
and do not create a separate flag, enum, or state for that purpose.

A single shared transport-neutral reasoning-policy helper applied at the
provider API request boundary alone interprets exact ordered `["off"]` as non-reasoning.
It returns closed `Omit | Send { effort, summary, include }`; `Omit` contains no reasoning values.
`Send` preserves selected typed effort and unchanged optional fields; serializers only encode the result.

`reasoning_summary` and `reasoning_include` are valid configuration fields for
`["off"]`. Special validation of those fields outside the reasoning-policy
helper is forbidden.

`["off", "low"]` with selected effort `off` is not a non-reasoning model. It is
an ordinary reasoning configuration that sends the effort,
`reasoning_summary`, and `reasoning_include` according to the general rules.

### supported_efforts

`reasoning_supported_efforts` always contains at least one non-empty value.
Every selected effort is an ordinary provider-defined value until the
reasoning-policy helper and must be present in the model's array.

There is no interference with or restriction on the set of
`reasoning_supported_efforts` levels listed in the configuration, except that
empty strings are forbidden. In particular, `off` is neither added nor required
automatically.

Forbidden: a local value allowlist, filtering, renaming, replacement,
reordering, autocompletion.

### Exact Runtime Model IDs

A runtime registry key is the canonical namespaced model ID
`<provider>::<raw-model-id>` returned by `models.list`.

`ProviderRegistry::get()` and model/reasoning resolution accept only an exact
canonical registry-key match. A raw model ID is never converted automatically.
A raw ID with one or multiple suffix matches is rejected as noncanonical or
ambiguous. Runtime model overrides must use an ID directly from `models.list`.

### `chat.send` and `chat.send_sync` Selection

Both RPC methods accept an optional complete `modelOverride` object:

```json
{
  "modelOverride": {
    "model": "openai::gpt-5.2",
    "reasoningEffort": "medium"
  }
}
```

If `modelOverride` is present, both fields are required. The effort must be
non-empty and must occur in the selected model's ordered
`reasoning_supported_efforts`. A missing field, empty effort, unknown model, or
unsupported effort is rejected before persistence or an LLM call. The selected
provider's tool mode is then checked for compatibility with the request.

If `modelOverride` is omitted, the methods use the complete persisted session
model/reasoning pair. A request override is persisted only as one atomic
model/reasoning pair.

The public payload is closed. `chat.send` accepts exactly one of `text` or
`content`, plus optional `modelOverride`, `toolChoice`, `documents`,
`audioFilename`, `inputMedium`, and `clientSequence`. `chat.send_sync` accepts
`text` plus optional `modelOverride`, `toolChoice`, and `inputMedium`. Unknown
fields are rejected. Session, connection, channel, tool-policy, and agent
execution context are not public JSON fields.

WebSocket clients must complete `sessions.switch` successfully before calling
`chat.send` or `chat.send_sync` on that connection. The RPC methods use the
active session bound to the connection; they do not accept a public session key
or fall back to a default session when that context is absent.

Queued prompts store prompt content only. When a queued batch runs, it resolves
the persisted session pair at that time through the same model/reasoning path.

### Load Refusal

Service load behavior:

- A missing mandatory parameter causes service load refusal.
- A value violating the validity criteria, including
  `reasoning_supported_efforts = []`, `[""]`, or an array containing any empty
  string, causes service load refusal.
- An unknown key in the model settings causes service load refusal.
- A provider without configured models does not cause an error or service load refusal, regardless of its configured status.

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
reasoning_supported_efforts = ["off"]
```

## Session Titles

See [Session Titles](configuration.md#session-titles) for the model/reasoning
configuration used by title generation.

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

Use **Settings** → **Providers** to save credentials for a provider. This flow
does not require model records to be declared before credentials are saved.

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
reasoning_supported_efforts = ["off"]

[chat]
priority_models = ["custom-ai-example::muse-flash-0.9"]
```

### Model Metadata Resolution

The service configuration is the only source of model composition and model
parameters. The registry includes configured model tables only for providers selected
by `providers.offered` (an empty list selects all) whose `enabled` setting is `true`.

A provider without configured models does not cause an error or service load
refusal, regardless of its configured status. Missing or invalid mandatory model
metadata and unknown model settings still refuse service load. Registry construction
is atomic: no problematic configured model is excluded to continue startup.

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
- **In channel sessions**: `/model` lists models; `/model providers` lists
  providers; `/model provider:<name>` filters the list; `/model efforts:<N>`
  lists efforts for model `N`; and `/model <N> <reasoning-effort>` atomically
  changes the persisted model/reasoning pair.
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
