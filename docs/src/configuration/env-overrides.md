# Environment Variable Overrides

Environment variables provide the highest-priority configuration layer. They're useful for temporary overrides, CI/CD environments, and sensitive values.

## Global Overrides

```bash
# Default provider and model
export ZEN_DEFAULT_PROVIDER="deepseek"
export ZEN_DEFAULT_MODEL="deepseek-v4-flash"
```

## Per-Task Overrides

Override knowledge-pipeline task routing without modifying any config files. Only these four distill tasks have env overrides:

```bash
export ZEN_AGENT_NOTION_EXTRACTION_PROVIDER="ollama"
export ZEN_AGENT_SYNTHESIS_MODEL="claude-sonnet-4-6"
export ZEN_AGENT_DISPATCH_PROVIDER="anthropic"
export ZEN_AGENT_CONTRADICTION_DETECTION_MODEL="qwen3.6:35b-mlx"
```

Supported task env var targets:

| Env Var | Effect |
|---------|--------|
| `ZEN_AGENT_NOTION_EXTRACTION_PROVIDER` / `_MODEL` | Override notion extraction provider / model |
| `ZEN_AGENT_CONTRADICTION_DETECTION_PROVIDER` / `_MODEL` | Override contradiction detection provider / model |
| `ZEN_AGENT_SYNTHESIS_PROVIDER` / `_MODEL` | Override synthesis provider / model |
| `ZEN_AGENT_DISPATCH_PROVIDER` / `_MODEL` | Override dispatch provider / model |

## Agent Engine Overrides

The `[agentic.*]` sections are env-overridable — handy for scripts and CI:

```bash
# Distill loop
export ZEN_LOOP_MAX_STEPS=20            # per-cycle distill budget (default 10)
export ZEN_LOOP_MAX_TOKENS=16000        # per-cycle token budget
export ZEN_LOOP_MAX_INGEST_BYTES=134217728  # skip files > 128 MiB

# Tool loop & delegation
export ZEN_TOOL_MAX_ROUNDS=12           # conversational tool rounds (default 8)
export ZEN_DELEGATE_MAX_DEPTH=2         # delegation depth (default 1)
export ZEN_DELEGATE_MAX_CONCURRENT=6    # fan-out width (default 4)

# Quality review
export ZEN_REVIEW_MAX_MOMUS_RETRIES=3
export ZEN_REVIEW_LLM_HIGH_BLAST=true

# Calibrated decision gates (absent = gate closed — pre-calibration behavior)
export ZEN_INTENT_L1_THRESHOLD=0.85
export ZEN_REVIEW_ESCALATE_THRESHOLD=0.9

# Retention sweep
export ZEN_RETENTION_DRY_RUN=true       # report-only, delete nothing
```

Full list by section:

| Section | Env Vars |
|---------|----------|
| `[agentic.loop]` | `ZEN_LOOP_ENABLED`, `_INTERVAL`, `_MERGE_THRESHOLD`, `_MAX_ATTEMPTS`, `_MIN_FREE_BYTES`, `_SKIP_EXTENSIONS`, `_HOST_STAGE_TIMEOUT_SECS`, `_MAX_STEPS`, `_MAX_TOKENS`, `_MAX_INGEST_BYTES`, `_COMMUNITY_RESOLUTION`, `_COMMUNITY_MIN_SIZE` |
| `[agentic.review]` | `ZEN_REVIEW_MAX_MOMUS_RETRIES`, `_MAX_HERMES_REVISIONS`, `_LLM_HIGH_BLAST`, `_ESCALATE_THRESHOLD` |
| `[agentic.delegate]` | `ZEN_DELEGATE_ENABLED`, `_TIMEOUT_SECS`, `_MAX_DEPTH`, `_MAX_CONCURRENT` |
| `[agentic.orchestrator]` | `ZEN_ORCHESTRATOR_SURFACE` (`full` \| `delegation-only`) |
| `[agentic.intent]` | `ZEN_INTENT_SHADOW_EMBEDDING`, `ZEN_INTENT_L1_THRESHOLD` |
| `[agentic.classifiers]` | `ZEN_CLASSIFIER_CORRECTION_THRESHOLD`, `ZEN_CLASSIFIER_CITATION_THRESHOLD` |
| `[agentic.audit]` | `ZEN_AUDIT_DECISION_EXCERPT_CHARS` (0 = off, ≤500) |
| `[agentic.retention]` | `ZEN_RETENTION_ENABLED`, `ZEN_RETENTION_DRY_RUN` |
| `[skills.auto_route]` | `ZEN_SKILLS_AUTO_ROUTE` |
| `[channels.qqbot]` | `ZEN_QQBOT_APP_ID`, `ZEN_QQBOT_CLIENT_SECRET` |

## Cron & Scheduler Overrides

```bash
export ZEN_CRON_CONSOLIDATION_TIME="03:00"
export ZEN_CRON_TIMEZONE="America/New_York"          # IANA name; bad values fall back to Utc with a warning
export ZEN_CRON_SUBCONSCIOUS_INTERVAL_MINUTES=10
export ZEN_CRON_WISDOM_SYNTHESIS="0 0 2 * * 7"
export ZEN_TUI_SCHEDULER=false                       # disable all background learning outside `zen serve start`
```

## Plugin & Embeddings Overrides

```bash
export ZEN_PLUGIN_BASE_PATH="/custom/plugin/path"
export ZEN_LEARNING_AUTO_RESEARCH="true"
export ZEN_LEARNING_INTERVAL="weekly"
export ZEN_FINANCE_BASE_CURRENCY="USD"

export ZEN_EMBEDDINGS_PROVIDER="local"
export ZEN_EMBEDDINGS_LOCAL_PROVIDER="fastembed"
export ZEN_EMBEDDINGS_MODEL="BGEM3"
export ZEN_EMBEDDINGS_CACHE_DIR="/custom/cache"
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
2. **Environment variables** override config files
3. **Per-task env vars** override that task's provider/model settings
4. Set env vars in `.env` file (loaded automatically) or export them in your shell profile

---

Next: [Configuration Examples](examples.md)
