# Choosing a Provider

Not sure which LLM provider to use? This page compares the providers supported
by Chelix so you can pick the best fit for your use case.

## Quick Recommendations

| Goal                       | Provider      | Why                                                                                   |
| -------------------------- | ------------- | ------------------------------------------------------------------------------------- |
| **Widest model range**     | OpenAI        | GPT-5.5, GPT-4.1, o3/o4-mini reasoning models, image generation                       |
| **Best membership option** | OpenAI        | GPT-5.5 is a top-quality model and can be available through memberships               |
| **Coding plan**            | Z.AI Coding   | Dedicated coding models and billing endpoint                                          |

## Provider Comparison

| Provider           | Top Models                    | Tool Use | Streaming | Context | Price Tier        | Speed  | Notes                                               |
| ------------------ | ----------------------------- | -------- | --------- | ------- | ----------------- | ------ | --------------------------------------------------- |
| **OpenAI**         | GPT-5.5, GPT-4.1, o3, o4-mini | Full     | Yes       | 128K-1M | $$ / Subscription | Fast   | Widest ecosystem, GPT-5.5 quality, reasoning models |
| **OpenRouter**     | Any (aggregator)              | Varies   | Yes       | Varies  | Varies            | Varies | Access 100+ models with one key                     |
| **Z.AI (Zhipu)**   | GLM-4, GLM-4 Air              | Full     | Yes       | 128K    | $                 | Fast   | GLM-4 series, competitive quality                   |
| **Z.AI Coding**    | CodeGeeX, GLM-4 Code          | Full     | Yes       | 128K    | $                 | Fast   | Optimized for code tasks                            |
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

Start with an existing **OpenAI** or **OpenAI Codex** account.

### For production agent workflows

**OpenAI** offers a broad model range including GPT-5.5 and reasoning models
(o3, o4-mini). GPT-5.5 is especially strong when you want high overall quality
and can use membership-based access.

### For access to many models

**OpenRouter** aggregates 100+ models behind a single API key. Useful if you
want to experiment across providers without managing multiple accounts.

## Setting Up a Provider

See the [LLM Providers](providers.md) page for step-by-step setup instructions
for each provider, including configuration file options and environment
variables.
