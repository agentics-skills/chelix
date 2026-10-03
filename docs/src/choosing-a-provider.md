# Choosing a Provider

Chelix has one LLM provider type: **OpenAI Compatible**.

You choose the endpoint, API key, wire API (`chat-completions` or `responses`),
and the models. The provider name in the config and in model ids is the name
you enter. It is a lowercase slug of letters, digits, and hyphens, with no
prefix.

Add it from **Settings** → **Providers** → **Add LLM**, or declare it in
`chelix.toml`:

```toml
[providers.example]
enabled = true
base_url = "https://api.example.invalid/v1"
wire_api = "chat-completions"

[providers.example.models."your-model"]
context_length = 128000
max_input_tokens = 96000
max_output_tokens = 32000
input_modalities = ["text"]
output_modalities = ["text"]
tool_calling = true
zeroDataRetentionEnabled = false
reasoning_supported_efforts = ["off"]
```

The registry model id is `example::your-model`.

Names `offered` and `voice-*` are reserved and cannot be used for an LLM
provider.
