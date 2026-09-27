# Configuration Overview

Zen uses a **4-layer configuration system** that merges settings from multiple sources. Higher-priority layers override lower ones, and you only need to specify the values you want to change.

## Configuration Layers

| Priority | Layer | File / Source | Typical Use |
|----------|-------|--------------|-------------|
| 1 (highest) | **Environment** | `ZEN_*` environment variables | Temporary overrides, CI/CD, secrets |
| 2 | **Global User** | `~/.zen/config.toml` | User-wide preferences |
| 3 | **Embedded Default** | `config/config.toml` (compiled in) | Shipped defaults |
| 4 (lowest) | **Built-in Default** | Rust `Default` impls | Last-resort fallbacks |

> Configuration is **global-only**: the `.zen/` directory marks a project context (and sandbox allowlist) but is **not** a configuration layer. All config lives under `~/.zen/config.toml`.

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
| `[providers.*]` | Named provider definitions: connection, model catalog, per-million token costs |
| `[agents]` / `[agents.*]` | Agent tool grants + per-agent model routing (distill tasks and agent profiles) |
| `[tui]` | TUI theme and knowledge-search mode (`fast`/`full`/`off`) |
| `[history]` | Input history size cap |
| `[embeddings]` | Local embedding provider, model, cache |
| `[cron]` | Background schedules, timezone, LLM cost cap, TUI scheduler kill switch |
| `[plugin]` | Plugin registry (learning, finance, custom plugins) |
| `[mcp_servers.*]` | MCP client server connections |
| `[sandbox]` | Network access + WASM sandbox permissions |
| `[agentic.*]` | Agent engine tuning: loop, tool loop, review, delegation, retention, … |
| `[channels.qqbot]` | QQ official-bot channel (WS gateway + REST sender) |
| `[skills.auto_route]` | Automatic skill routing |

## Viewing Effective Configuration

```bash
# Show the complete merged configuration
zen config show

# List available providers
zen provider list

# Test a provider connection
zen provider test <provider-name>
```

The `zen config show` command displays the fully merged configuration from all layers, so you can always see exactly what's in effect.

---

Next: [Provider Definitions](providers.md)
