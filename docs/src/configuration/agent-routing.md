# Agent → Model Routing

Each agent can be assigned a specific provider and model, with a sequential fallback chain for reliability. Routing is defined in `[agents.*]` sections — both the knowledge-pipeline tasks (`notion_extraction`, `contradiction_detection`, `synthesis`, `dispatch`) and the 13 agent profiles (`Sisyphus`, `Prometheus`, `Explore`, …).

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
| `sensitivity` | No | Max data sensitivity this agent may handle: `"Public"`, `"Private"`, or `"Confidential"` — Private/Confidential data is routed to local providers only |

> Agent identity traits (role, LLM preferences, clearance) are **registry-hardcoded** — they are not configurable per agent. Only the routing fields above are read from config.

## Fallback Chain

Each fallback step can specify:

| Field | Description |
|-------|-------------|
| `provider` | Provider name for this fallback step |
| `model` | Override model (optional, uses provider's default if omitted) |
| `timeout_secs` | Timeout for this step (optional) |

```toml
[agents.dispatch]
provider = "anthropic"
model = "claude-sonnet-4-6"
fallbacks = [
    { provider = "openai", model = "gpt-4o", timeout_secs = 30 },
    { provider = "ollama", model = "qwen3.6:35b-mlx" }
]
```

## Sensitivity-Aware Routing

When `sensitivity` is set to `"Private"` or `"Confidential"`, Zen enforces local-only routing for sensitive data:

- **Private/Confidential** data is **never** sent to cloud providers
- If no local LLM is available, the agent returns an error instead of falling back to cloud

```toml
# Knowledge pipeline — private data, local-first
[agents.notion_extraction]
provider = "ollama"
model = "qwen3.6:35b-mlx"
fallbacks = [{ provider = "deepseek", model = "deepseek-v4-flash" }]
sensitivity = "Private"
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
sensitivity = "Private"
```

### Fast Explorer (cost-optimized)

```toml
[agents.Explore]
provider = "anthropic"
model = "claude-haiku-4-5"
fallbacks = [{ provider = "openai", model = "gpt-4o-mini" }]
```

---

Next: [Environment Variable Overrides](env-overrides.md)
