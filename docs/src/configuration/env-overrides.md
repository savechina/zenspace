# Environment Variable Overrides

Environment variables provide the highest-priority configuration layer. They're useful for temporary overrides, CI/CD environments, and sensitive values.

## Global Overrides

```bash
# Default provider and model
export ZEN_DEFAULT_PROVIDER="deepseek"
export ZEN_DEFAULT_MODEL="deepseek-v4-flash"
```

## Per-Agent Overrides

Override specific agent routing without modifying any config files:

```bash
# Force notion_extraction to use local Ollama
export ZEN_AGENT_NOTION_EXTRACTION_PROVIDER="ollama"

# Override synthesis agent's model
export ZEN_AGENT_SYNTHESIS_MODEL="claude-sonnet-4-6"

# Override dispatch agent
export ZEN_AGENT_DISPATCH_PROVIDER="anthropic"
export ZEN_AGENT_DISPATCH_MODEL="claude-sonnet-4-6"
```

Supported agent env var targets:

| Env Var | Effect |
|---------|--------|
| `ZEN_AGENT_NOTION_EXTRACTION_PROVIDER` | Override notion_extraction provider |
| `ZEN_AGENT_NOTION_EXTRACTION_MODEL` | Override notion_extraction model |
| `ZEN_AGENT_CONTRADICTION_DETECTION_PROVIDER` | Override contradiction detection provider |
| `ZEN_AGENT_CONTRADICTION_DETECTION_MODEL` | Override contradiction detection model |
| `ZEN_AGENT_SYNTHESIS_PROVIDER` | Override synthesis provider |
| `ZEN_AGENT_SYNTHESIS_MODEL` | Override synthesis model |
| `ZEN_AGENT_DISPATCH_PROVIDER` | Override dispatch provider |
| `ZEN_AGENT_DISPATCH_MODEL` | Override dispatch model |

## Cron Overrides

```bash
export ZEN_CRON_CONSOLIDATION_TIME="03:00"
export ZEN_CRON_TIMEZONE="America/New_York"
export ZEN_CRON_SUBCONSCIOUS_INTERVAL_MINUTES=10
```

## Plugin Overrides

```bash
export ZEN_PLUGIN_BASE_PATH="/custom/plugin/path"
export ZEN_PLUGIN_WASM_CACHE_PATH="/custom/cache/path"
export ZEN_LEARNING_AUTO_RESEARCH="true"
export ZEN_LEARNING_INTERVAL="weekly"
export ZEN_FINANCE_BASE_CURRENCY="USD"
```

## API Key Environment Variables

```bash
# Set API keys for providers
export OPENAI_API_KEY="sk-..."
export ANTHROPIC_API_KEY="sk-ant-..."
export DEEPSEEK_API_KEY="sk-..."
export DASHSCOPE_API_KEY="sk-..."
export GEMINI_API_KEY="..."
export COHERE_API_KEY="..."
export MISTRAL_API_KEY="..."
export GROQ_API_KEY="gsk_..."
export MOONSHOT_API_KEY="..."
export XAI_API_KEY="..."
export PERPLEXITY_API_KEY="..."
```

## Priority Rules

1. **Explicit `api_key.env`** in provider config takes precedence over auto-derived names
2. **Environment variables** override both global and workspace config files
3. **Per-agent env vars** override the agent's provider/model settings
4. Set env vars in `.env` file (loaded automatically) or export them in your shell profile

---

Next: [Configuration Examples](examples.md)
