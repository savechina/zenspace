# Provider Definitions

Providers are the connection endpoints to LLM services. Define them once by name in `[providers.*]` sections, then reference them in agent routing.

## Supported Protocol Types

| Type | Auth Required | Description | Examples |
|------|--------------|-------------|----------|
| `ollama` | No | Local Ollama server | qwen3.6, llama3, mistral (local) |
| `openai` | API key | Native OpenAI API | gpt-4o, gpt-4o-mini |
| `anthropic` | API key | Native Anthropic Messages API | claude-sonnet-4-6, claude-haiku-4-5 |
| `gemini` | API key | Google Gemini API | gemini-2.0-flash |
| `cohere` | API key | Cohere API | command-r |
| `mistral` | API key | Mistral API | mistral-large-latest |
| `openai-compatible` | API key | OpenAI-compatible endpoints | DeepSeek, Groq, Perplexity, Aliyun, xAI |
| `anthropic-compatible` | API key | Anthropic-compatible endpoints | Moonshot, MiniMax |
| `mock` | No | Testing mock (no external calls) | — |

## Basic Provider Configuration

```toml
# Local Ollama (no API key needed)
[providers.ollama]
type = "ollama"
base_url = "http://127.0.0.1:11434"
default_model = "qwen3.6:35b-mlx"

# OpenAI
[providers.openai]
type = "openai"
base_url = "https://api.openai.com/v1"
api_key = { env = "OPENAI_API_KEY" }
default_model = "gpt-4o-mini"

# Anthropic
[providers.anthropic]
type = "anthropic"
api_key = { env = "ANTHROPIC_API_KEY" }
default_model = "claude-haiku-4-5"

# OpenAI-compatible (DeepSeek)
[providers.deepseek]
type = "openai-compatible"
base_url = "https://api.deepseek.com"
api_key = { env = "DEEPSEEK_API_KEY" }
default_model = "deepseek-v4-flash"

# OpenAI-compatible (Aliyun/Qwen)
[providers.aliyun]
type = "openai-compatible"
base_url = "https://dashscope.aliyuncs.com/compatible-mode/v1"
api_key = { env = "DASHSCOPE_API_KEY" }
default_model = "qwen3.6-plus"
```

### Dual-Protocol Providers

Some providers support both OpenAI-compatible and Anthropic-compatible protocols:

```toml
# DeepSeek: OpenAI-compatible (default)
[providers.deepseek]
type = "openai-compatible"
base_url = "https://api.deepseek.com"
api_key = { env = "DEEPSEEK_API_KEY" }

# DeepSeek: Anthropic-compatible (alternative)
[providers.deepseek-anthropic]
type = "anthropic-compatible"
base_url = "https://api.deepseek.com/anthropic"
api_key = { env = "DEEPSEEK_API_KEY" }

# Moonshot: either protocol works
[providers.moonshot]
type = "openai-compatible"
base_url = "https://api.moonshot.cn/v1"
api_key = { env = "MOONSHOT_API_KEY" }
default_model = "kimi-k2.5"
```

## API Key Resolution

API keys are resolved lazily at first use (not during startup), following this order per provider:

1. `api_key.env` — named environment variable (e.g., `DEEPSEEK_API_KEY`)
2. `api_key.keychain` — macOS Keychain service name (e.g., `zen-deepseek-api-key`)
3. `api_key_env` — legacy env var field (deprecated)
4. `{PROVIDER}_API_KEY` — auto-derived env var (e.g., `OPENAI_API_KEY`)
5. Ollama/local providers — no auth required

```toml
# Using environment variable
api_key = { env = "DEEPSEEK_API_KEY" }

# Using macOS Keychain
api_key = { keychain = "zen-deepseek-api-key" }

# Direct env var name (auto-derived if not specified)
# ^ will try: DEEPSEEK_API_KEY automatically
```

### Keychain Integration

On macOS, Zen integrates with the system Keychain for secure credential storage:

```bash
# Store an API key in Keychain
security add-generic-password -a "zen" -s "zen-openai-api-key" -w "sk-..."

# Configure provider to use Keychain
[providers.openai]
type = "openai"
api_key = { keychain = "zen-openai-api-key" }
```

If Keychain is unavailable (non-macOS, headless environment), Zen falls back to environment variables automatically.

## Provider Fields Reference

| Field | Required | Description |
|-------|----------|-------------|
| `type` | Yes | Protocol type (see table above) |
| `base_url` | For some types | API endpoint URL (Ollama: `http://127.0.0.1:11434`) |
| `api_key` | For cloud providers | Secret reference (env var or keychain) |
| `api_key_env` | No | Legacy env var name (deprecated) |
| `default_model` | Recommended | Default model name for this provider |
| `models` | No | Per-model catalog with parameters (see [Model Catalog](model-catalog.md)) |
| `wire_api` | No | Wire protocol: `"completions"` (default) or `"responses"` |

---

Next: [Model Catalog & Parameters](model-catalog.md)
