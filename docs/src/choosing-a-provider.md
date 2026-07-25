# Choosing a Provider

Not sure which LLM provider to use? This page compares the providers supported
by Chelix so you can pick the best fit for your use case.

## Quick Recommendations

| Goal                       | Provider      | Why                                                                                   |
| -------------------------- | ------------- | ------------------------------------------------------------------------------------- |
| **Best overall quality**   | Anthropic     | Claude Sonnet 4 and Opus 4 excel at tool use, long context, and instruction following |
| **Widest model range**     | OpenAI        | GPT-5.5, GPT-4.1, o3/o4-mini reasoning models, image generation                       |
| **Best membership option** | OpenAI        | GPT-5.5 is a top-quality model and can be available through memberships               |
| **Largest context window** | Google Gemini | Up to 1M tokens with Gemini 2.5 Pro                                                   |
| **Coding plan**            | Z.AI Coding   | Dedicated coding models and billing endpoint                                          |

## Provider Comparison

| Provider           | Top Models                    | Tool Use | Streaming | Context | Price Tier        | Speed  | Notes                                               |
| ------------------ | ----------------------------- | -------- | --------- | ------- | ----------------- | ------ | --------------------------------------------------- |
| **Anthropic**      | Claude Sonnet 4, Opus 4       | Full     | Yes       | 200K    | $$                | Fast   | Best tool-use reliability                           |
| **OpenAI**         | GPT-5.5, GPT-4.1, o3, o4-mini | Full     | Yes       | 128K-1M | $$ / Subscription | Fast   | Widest ecosystem, GPT-5.5 quality, reasoning models |
| **Google Gemini**  | Gemini 2.5 Pro, 2.5 Flash     | Full     | Yes       | 1M      | $                 | Fast   | Largest context, competitive pricing                |
| **xAI**            | Grok 3, Grok 3 Mini           | Yes      | Yes       | 128K    | $$                | Fast   | Strong reasoning capabilities                       |
| **OpenRouter**     | Any (aggregator)              | Varies   | Yes       | Varies  | Varies            | Varies | Access 100+ models with one key                     |
| **Z.AI (Zhipu)**   | GLM-4, GLM-4 Air              | Full     | Yes       | 128K    | $                 | Fast   | GLM-4 series, competitive quality                   |
| **Z.AI Coding**    | CodeGeeX, GLM-4 Code          | Full     | Yes       | 128K    | $                 | Fast   | Optimized for code tasks                            |
| **Moonshot**       | Kimi                          | Full     | Yes       | 200K    | $                 | Medium | Long context, Chinese/English                       |
| **GitHub Copilot** | GPT-4o, Claude (via Copilot)  | Full     | Yes       | Varies  | Subscription      | Fast   | Uses existing Copilot subscription                  |
| **OpenAI Codex**   | Codex models                  | Full     | Yes       | Varies  | $$                | Fast   | OAuth-based, code-focused                           |

### Price Tier Legend

| Symbol           | Meaning                                 |
| ---------------- | --------------------------------------- |
| **Free**         | No cost (local inference)               |
| **$**            | Budget-friendly (< $1/M input tokens)   |
| **$$**           | Standard pricing ($1-15/M input tokens) |
| **$$$**          | Premium pricing (> $15/M input tokens)  |
| **Subscription** | Flat monthly fee                        |

## How to Choose

### For personal projects or experimentation

Start with **Google Gemini** for a large context window, or use an existing
OpenAI, Anthropic, GitHub Copilot, or OpenAI Codex account.

### For production agent workflows

**Anthropic** and **OpenAI** are the most battle-tested for tool use and complex
multi-step tasks. Anthropic's Claude models tend to follow instructions
precisely; OpenAI offers a broader model range including GPT-5.5 and reasoning
models (o3, o4-mini). GPT-5.5 is especially strong when you want high overall
quality and can use membership-based access.

### For access to many models

**OpenRouter** aggregates 100+ models behind a single API key. Useful if you
want to experiment across providers without managing multiple accounts.

## Setting Up a Provider

See the [LLM Providers](providers.md) page for step-by-step setup instructions
for each provider, including configuration file options and environment
variables.
