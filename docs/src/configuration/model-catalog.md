# Model Catalog & Parameters

Each provider can define a **model catalog** — a map of model names to their API identifiers, generation parameters, and named parameter variants.

## Defining Models

When a `models` map is present under a provider, `default_model` selects a key in this map:

```toml
[providers.openai]
type = "openai"
api_key = { env = "OPENAI_API_KEY" }
default_model = "gpt-4o-mini"  # selects from [providers.openai.models]

    [providers.openai.models.gpt-4o]
    model = "gpt-4o"
    options = { temperature = 0.7, max_tokens = 4096 }

    [providers.openai.models.gpt-4o-mini]
    model = "gpt-4o-mini"
    options = { temperature = 0.3, max_tokens = 2048, reasoning_effort = "low" }
```

When `models` is absent, `default_model` is used directly as the API model name (backward compatible):

```toml
# Simple configuration — no model catalog
[providers.deepseek]
type = "openai-compatible"
base_url = "https://api.deepseek.com"
api_key = { env = "DEEPSEEK_API_KEY" }
default_model = "deepseek-v4-flash"  # used directly as API model name
```

## Model Parameters

| Parameter | Type | Description | Example |
|-----------|------|-------------|---------|
| `temperature` | float (0.0–2.0) | Sampling temperature. Higher = more random | `0.7` |
| `max_tokens` | integer | Maximum tokens in the response | `4096` |
| `reasoning_effort` | string | Reasoning depth: `"low"`, `"medium"`, `"high"` | `"high"` |
| `top_p` | float (0.0–1.0) | Nucleus sampling threshold | `0.9` |

### Complete Example

```toml
[providers.anthropic]
type = "anthropic"
api_key = { env = "ANTHROPIC_API_KEY" }
default_model = "claude-sonnet-4-6"

    [providers.anthropic.models.claude-sonnet-4-6]
    model = "claude-sonnet-4-6"
    options = { temperature = 0.5, max_tokens = 8192 }

    [providers.anthropic.models.claude-haiku-4-5]
    model = "claude-haiku-4-5"
    options = { temperature = 0.3, max_tokens = 4096 }

[providers.ollama]
type = "ollama"
base_url = "http://127.0.0.1:11434"
default_model = "qwen3.6-35b-mlx"

    [providers.ollama.models.qwen3.6-35b-mlx]
    model = "qwen3.6:35b-mlx"
    options = { temperature = 0.6, max_tokens = 4096 }
```

## Named Variants

Named variants allow the **same model** to be used with **different parameters** for different agents. This is useful when the same model needs different reasoning depths or creativity levels depending on the task.

```toml
[providers.anthropic.models.claude-sonnet-4-6]
model = "claude-sonnet-4-6"
options = { temperature = 0.5, max_tokens = 8192 }

    # Variant: creative
    [providers.anthropic.models.claude-sonnet-4-6.variants.creative]
    temperature = 0.9

    # Variant: precise
    [providers.anthropic.models.claude-sonnet-4-6.variants.precise]
    temperature = 0.1
    reasoning_effort = "high"
```

Agents reference variants via the `variant` field:

```toml
[agents.Prometheus]
provider = "anthropic"
model = "claude-sonnet-4-6"
variant = "precise"  # uses temperature=0.1, reasoning_effort="high"
```

Variant parameters **merge into** the base model options — they only override the fields specified.

## Parameter Resolution Order

When an agent makes a call, parameters are resolved in this order:

1. **Model-level** defaults from the model catalog entry
2. **Variant** overrides (if specified)
3. **Agent-level** overrides (if specified in `[agents.*]`)

This means an agent can always override temperature or max_tokens at the agent config level, regardless of model defaults.

---

Next: [Agent → Model Routing](agent-routing.md)
