# Configuration Examples

## 1. Local-Only Setup (Ollama Only)

For complete offline operation with no cloud dependencies:

```toml
default_provider = "ollama"
default_model = "qwen3.6:35b-mlx"

[providers.ollama]
type = "ollama"
base_url = "http://127.0.0.1:11434"
default_model = "qwen3.6:35b-mlx"

# No other providers needed
```

**Best for:** Privacy-critical environments, air-gapped setups, offline use.

## 2. Cloud-First with Local Fallback

Use cloud for quality, fall back to local when offline:

```toml
default_provider = "anthropic"
default_model = "claude-haiku-4-5"

[providers.anthropic]
type = "anthropic"
api_key = { env = "ANTHROPIC_API_KEY" }
default_model = "claude-haiku-4-5"

[providers.ollama]
type = "ollama"
base_url = "http://127.0.0.1:11434"
default_model = "qwen3.6:35b-mlx"

[agents.dispatch]
provider = "anthropic"
model = "claude-sonnet-4-6"
fallbacks = [{ provider = "ollama", model = "qwen3.6:35b-mlx" }]
```

**Best for:** Daily driver — cloud quality with offline resilience.

## 3. Multi-Cloud Hybrid Routing

Route different tasks to different cloud providers based on cost and capability:

```toml
default_provider = "deepseek"
default_model = "deepseek-v4-flash"

[providers.deepseek]
type = "openai-compatible"
base_url = "https://api.deepseek.com"
api_key = { env = "DEEPSEEK_API_KEY" }
default_model = "deepseek-v4-flash"

[providers.aliyun]
type = "openai-compatible"
base_url = "https://dashscope.aliyuncs.com/compatible-mode/v1"
api_key = { env = "DASHSCOPE_API_KEY" }
default_model = "qwen3.6-plus"

[providers.groq]
type = "openai-compatible"
base_url = "https://api.groq.com/openai/v1"
api_key = { env = "GROQ_API_KEY" }
default_model = "llama-3.3-70b-versatile"

# Knowledge pipeline: local-first, cloud backup
[agents.notion_extraction]
provider = "ollama"
model = "qwen3.6:35b-mlx"
fallbacks = [
    { provider = "deepseek", model = "deepseek-v4-flash" }
]

# Orchestrator: capable cloud model with fallbacks
[agents.Sisyphus]
provider = "anthropic"
model = "claude-sonnet-4-6"
fallbacks = [
    { provider = "openai", model = "gpt-4o" },
    { provider = "deepseek", model = "deepseek-v4-flash" }
]
```

**Best for:** Cost optimization with multiple provider accounts.

## 4. Privacy-Preserving Setup

Local for sensitive data, cloud for public information:

```toml
default_provider = "ollama"
default_model = "qwen3.6:35b-mlx"

[providers.ollama]
type = "ollama"
base_url = "http://127.0.0.1:11434"

[providers.deepseek]
type = "openai-compatible"
base_url = "https://api.deepseek.com"
api_key = { env = "DEEPSEEK_API_KEY" }

# Private data stays local
[agents.Metis]
provider = "ollama"
max_sensitivity = "Medium"

# Public research can use cloud
[agents.Explore]
provider = "deepseek"
llm_preferences = "CloudOnly"
max_sensitivity = "Low"
```

**Best for:** Healthcare, legal, finance — any domain with data residency requirements.

## 5. Cost-Optimized Setup

Cheapest capable model for each task tier:

```toml
default_provider = "deepseek"
default_model = "deepseek-v4-flash"

[providers.deepseek]
type = "openai-compatible"
base_url = "https://api.deepseek.com"
api_key = { env = "DEEPSEEK_API_KEY" }

[providers.ollama]
type = "ollama"
base_url = "http://127.0.0.1:11434"

# Heavy reasoning: use local (free)
[agents.dispatch]
provider = "ollama"
model = "qwen3.6:35b-mlx"

# Light tasks: cheapest cloud API
[agents.Explore]
provider = "deepseek"
model = "deepseek-v4-flash"

# Synthesis: use best model sparingly
[agents.synthesis]
provider = "deepseek"
model = "deepseek-v4-flash"
fallbacks = [{ provider = "ollama", model = "qwen3.6:35b-mlx" }]
```

**Best for:** Budget-conscious setups, hobbyist use, development.

## 6. Complete Production Config

```toml
default_provider = "anthropic"
default_model = "claude-haiku-4-5"

[providers.ollama]
type = "ollama"
base_url = "http://127.0.0.1:11434"
default_model = "qwen3.6:35b-mlx"

[providers.anthropic]
type = "anthropic"
api_key = { env = "ANTHROPIC_API_KEY" }
default_model = "claude-haiku-4-5"

[providers.openai]
type = "openai"
api_key = { env = "OPENAI_API_KEY" }
default_model = "gpt-4o-mini"

[providers.deepseek]
type = "openai-compatible"
base_url = "https://api.deepseek.com"
api_key = { env = "DEEPSEEK_API_KEY" }
default_model = "deepseek-v4-flash"

# Orchestrator
[agents.Sisyphus]
provider = "anthropic"
model = "claude-sonnet-4-6"
fallbacks = [
    { provider = "openai", model = "gpt-4o" },
    { provider = "deepseek", model = "deepseek-v4-flash" }
]

# Planner
[agents.Prometheus]
provider = "anthropic"
model = "claude-sonnet-4-6"
fallbacks = [{ provider = "openai", model = "gpt-4o" }]

# Knowledge pipeline
[agents.notion_extraction]
provider = "ollama"
model = "qwen3.6:35b-mlx"
fallbacks = [
    { provider = "deepseek", model = "deepseek-v4-flash" },
    { provider = "openai", model = "gpt-4o-mini" }
]

# Fast exploration
[agents.Explore]
provider = "anthropic"
model = "claude-haiku-4-5"
fallbacks = [{ provider = "openai", model = "gpt-4o-mini" }]
llm_preferences = "CloudOnly"

# Privacy-sensitive analysis
[agents.Metis]
provider = "deepseek"
model = "deepseek-v4-flash"
fallbacks = [{ provider = "ollama", model = "qwen3.6:35b-mlx" }]
max_sensitivity = "Medium"

# Worker agents
[agents.Hephaestus]
provider = "anthropic"
model = "claude-sonnet-4-6"
fallbacks = [
    { provider = "openai", model = "gpt-4o" },
    { provider = "deepseek", model = "deepseek-v4-flash" }
]

[agents.Atlas]
provider = "ollama"
model = "qwen3.6:35b-mlx"
fallbacks = [{ provider = "deepseek", model = "deepseek-v4-flash" }]

[agents.Junior]
provider = "ollama"
model = "qwen3.6:35b-mlx"
fallbacks = []
```

---

**Next:** [Introduction](../introduction.md) — back to guide start
