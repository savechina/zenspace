# Configuration Examples

## 1. Local-Only Setup (Ollama Only)

```toml
default_provider = "ollama"
default_model = "qwen3.6:35b-mlx"

[providers.ollama]
type = "ollama"
base_url = "http://127.0.0.1:11434"
default_model = "qwen3.6:35b-mlx"

# No other providers needed
```

For complete offline operation with no cloud dependencies.

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
sensitivity = "Private"

# Public research can use cloud
[agents.Explore]
provider = "deepseek"
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

# Privacy-sensitive analysis
[agents.Metis]
provider = "deepseek"
model = "deepseek-v4-flash"
fallbacks = [{ provider = "ollama", model = "qwen3.6:35b-mlx" }]
sensitivity = "Private"

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

## 7. Self-Learning Loop Tuning

The background distill loop and its guards are tuned under `[agentic.*]`. All keys are optional — unset keys use built-in defaults:

```toml
# Distill loop: how fast the inbox drains
[agentic.loop]
interval = "0 */5 * * * *"       # every 5 minutes (cron, [cron] timezone applies)
max_steps = 10                   # per-cycle budget, clamp 1..=100 (env ZEN_LOOP_MAX_STEPS)
max_tokens = 8000                # per-cycle LLM token budget
max_ingest_bytes = 67108864      # skip+quarantine files > 64 MiB
reverify_older_than_days = 7     # re-verify hypotheses older than this

# Agent tool loop: rounds per conversational turn
[agentic.tool_loop]
max_rounds = 8                   # clamp 1..=16 (env ZEN_TOOL_MAX_ROUNDS)

# Sub-agent delegation from the orchestrator
[agentic.delegate]
enabled = true
max_depth = 1                    # how deep delegation can nest, clamp 1..=3
max_concurrent = 4               # parallel fan-out, clamp 1..=8
timeout_secs = 300               # clamp 30..=1800

# Daily retention sweep (log/archive rotation + age deletes)
[agentic.retention]
enabled = true
dry_run = false                  # true = report only, delete nothing
```

**Best for:** Tuning throughput vs cost, or taming the loop on low-power machines.

## 8. QQ Bot Channel

Enable the QQ official-bot channel so allowlisted chats converse with your persistent agent sessions. Requires `zen serve start` (the channel runs inside the gateway daemon):

```toml
[channels.qqbot]
app_id = "your-qq-app-id"              # or env ZEN_QQBOT_APP_ID
client_secret = "your-qq-secret"       # or env ZEN_QQBOT_CLIENT_SECRET
allowed_users = [                      # deny-by-default; group member openids
    "ABCDEF1234567890",                # and C2C user ids may be mixed
]
outbox_drain_interval_secs = 300       # morning-brief active-send tick, clamp 60..=3600
```

In chat, `/new` resets the session and `/status` renders gateway health. `zen doctor` fails the `outbox` probe with an actionable message when briefs are staged but the channel is unconfigured.

**Best for:** Talking to your knowledge base from QQ, receiving the morning brief.

## 9. Cost Tracking & Budget Cap

Per-provider token costs (USD per million tokens) make the `[cron]` budget cap real, and `zen discover report` surfaces actual spend:

```toml
[cron]
llm_cost_cap_usd = 10.0          # monthly cap across scheduled workers (default 10.0)

[providers.deepseek]
type = "openai-compatible"
base_url = "https://api.deepseek.com"
api_key = { env = "DEEPSEEK_API_KEY" }
default_model = "deepseek-v4-flash"
input_cost_per_million = 0.27
output_cost_per_million = 1.10

[providers.anthropic]
type = "anthropic"
api_key = { env = "ANTHROPIC_API_KEY" }
default_model = "claude-haiku-4-5"
input_cost_per_million = 0.80
output_cost_per_million = 4.00
```

Local (Ollama) usage is structurally free — cost 0.0. Check actual spend with `zen discover report` (worker costs persist under `~/.zen/logs/worker-costs.json`).

**Best for:** Keeping scheduled background learning inside a predictable budget.

---

**Next:** [Introduction](../introduction.md) — back to guide start
