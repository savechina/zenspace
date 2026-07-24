# Configuration Overview

Zen uses a **5-layer configuration system** that merges settings from multiple sources. Higher-priority layers override lower ones, and you only need to specify the values you want to change.

## Configuration Layers

| Priority | Layer | File / Source | Typical Use |
|----------|-------|--------------|-------------|
| 1 (highest) | **Environment** | `ZEN_*` environment variables | Temporary overrides, CI/CD, secrets |
| 2 | **Workspace** | `.zen/config.toml` | Per-project settings |
| 3 | **Global User** | `~/.zen/config.toml` | User-wide preferences |
| 4 (lowest) | **Embedded Default** | `config/config.toml` (compiled in) | Shipped defaults |

## How Merging Works

Each layer merges cleanly into the previous one. A higher layer only overrides keys it explicitly sets — so you can override just `default_model` without copying the entire config.

```toml
# Example: ~/.zen/config.toml — just override what you need
default_provider = "deepseek"
default_model = "deepseek-v4-flash"

# Only the providers you want to customize
[providers.deepseek]
type = "openai-compatible"
base_url = "https://api.deepseek.com"
api_key = { env = "DEEPSEEK_API_KEY" }
default_model = "deepseek-v4-flash"
```

This minimal config merges with the embedded defaults — all other providers (Ollama, OpenAI, Anthropic, etc.) remain available from the embedded config.

## Config Structure

The configuration is organized into these top-level sections:

| Section | Description |
|---------|-------------|
| `default_provider` | Default provider name (references a `[providers.*]` key) |
| `default_model` | Default model when no task-specific model is set |
| `[providers.*]` | Named provider definitions (connection settings) |
| `[agents.*]` | Agent task routing (provider/model per task) |
| `[tui]` | TUI theme settings |
| `[features]` | Feature flags |
| `[plugin.*]` | Plugin system configuration |
| `[cron]` | Scheduled task configuration |
| `[history]` | Command history settings |

## Viewing Effective Configuration

```bash
# Show the complete merged configuration
zen config show

# List available providers
zen provider list

# Test a provider connection
zen provider test <provider-name>
```

The `zen config show` command displays the fully merged configuration from all 5 layers, so you can always see exactly what's in effect.

---

Next: [Provider Definitions](providers.md)
