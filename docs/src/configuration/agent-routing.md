# Agent → Model Routing

Each agent can be assigned a specific provider and model, with a sequential fallback chain for reliability.

## Basic Routing

```toml
[agents.Sisyphus]
provider = "anthropic"
model = "claude-sonnet-4-6"
fallbacks = [
    { provider = "openai", model = "gpt-4o" },
    { provider = "deepseek", model = "deepseek-v4-flash" }
]
```

The router tries providers in order: primary → first fallback → second fallback → ... → `Mock` (always available).

## Agent Routing Fields

| Field | Required | Description |
|-------|----------|-------------|
| `provider` | Yes | Primary provider name (must match a `[providers.*]` key) |
| `model` | No | Model override (falls back to provider's `default_model`) |
| `fallbacks` | No | Ordered fallback chain if primary fails |
| `llm_preferences` | No | `"Any"`, `"LocalOnly"`, `"CloudOnly"`, or `"Provider(name)"` |
| `max_sensitivity` | No | Max data sensitivity: `"Low"`, `"Medium"`, `"High"` |
| `temperature` | No | Override per-agent temperature |
| `max_tokens` | No | Override per-agent max tokens |
| `variant` | No | Select a named variant from the model catalog |
| `retry_policy` | No | Retry settings for transient errors |

## LLM Preferences

| Preference | Behavior |
|------------|----------|
| `Any` | Standard routing (primary → fallbacks) |
| `LocalOnly` | Force Ollama if available, error if unreachable |
| `CloudOnly` | Force `default_provider` if it's a cloud provider |
| `Provider(name)` | Use the named provider directly |

### Privacy-Sensitive Routing

When `max_sensitivity` is set to `"Medium"` or `"High"`, Zen enforces local-only routing for sensitive data:

- **Private/Confidential** data is **never** sent to cloud providers
- If no local LLM is available, the agent returns an error instead of falling back to cloud

```toml
# Private data — local-only enforced
[agents.Metis]
provider = "deepseek"
model = "deepseek-v4-flash"
fallbacks = [{ provider = "ollama", model = "qwen3.6:35b-mlx" }]
max_sensitivity = "Medium"

# Public data — can use any provider
[agents.Explore]
provider = "anthropic"
model = "claude-haiku-4-5"
fallbacks = [{ provider = "openai", model = "gpt-4o-mini" }]
llm_preferences = "CloudOnly"
```

## Fallback Chain

Each fallback step can specify:

| Field | Description |
|-------|-------------|
| `provider` | Provider name for this fallback step |
| `model` | Override model (optional, uses provider's default if omitted) |
| `timeout_secs` | Timeout for this step (optional) |
| `variant` | Variant name for this step's model (optional) |

```toml
[agents.dispatch]
provider = "anthropic"
model = "claude-sonnet-4-6"
fallbacks = [
    { provider = "openai", model = "gpt-4o", timeout_secs = 30 },
    { provider = "ollama", model = "qwen3.6:35b-mlx" }
]
retry_policy = { max_retries = 3, timeout_secs = 30 }
```

## Complete Agent Configuration Examples

### Orchestrator Tier (requires capable models)

```toml
[agents.Sisyphus]
provider = "anthropic"
model = "claude-sonnet-4-6"
fallbacks = [
    { provider = "openai", model = "gpt-4o" },
    { provider = "deepseek", model = "deepseek-v4-flash" }
]
llm_preferences = "Any"
max_sensitivity = "High"
```

### Knowledge Pipeline (local-first)

```toml
[agents.notion_extraction]
provider = "ollama"
model = "qwen3.6:35b-mlx"
fallbacks = [
    { provider = "deepseek", model = "deepseek-v4-flash" },
    { provider = "openai", model = "gpt-4o-mini" }
]
```

### Fast Explorer (cost-optimized)

```toml
[agents.Explore]
provider = "anthropic"
model = "claude-haiku-4-5"
fallbacks = [{ provider = "openai", model = "gpt-4o-mini" }]
llm_preferences = "CloudOnly"
max_sensitivity = "Low"
```

### Privacy-Sensitive Analyst

```toml
[agents.Hermes]
provider = "deepseek"
model = "deepseek-v4-flash"
fallbacks = [
    { provider = "openai", model = "gpt-4o-mini" },
    { provider = "ollama", model = "qwen3.6:35b-mlx" }
]
llm_preferences = "Any"
max_sensitivity = "High"
```

---

Next: [Environment Variable Overrides](env-overrides.md)
