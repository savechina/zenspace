# Zenspace

**Your Personal AI Agentic Workspace** — a self-learning knowledge system that builds your wiki, remembers everything, and gets smarter over time.

> 🧠 **You think. It builds.** Write notes like you always do. Zenspace runs in the background — extracting entities, connecting ideas, building wiki pages, detecting patterns, and learning from your decisions. Short-term memory, long-term wisdom, and an agent team of 13 working for you. Markdown in, Markdown out. Your data stays yours.

[中文文档](README_zh.md) | [Full User Guide →](https://savechina.github.io/zenspace/)

---

## 10 Things Zenspace Does Differently

### 1. 📝 **Notes That Build Themselves**
Write normally — agents extract entities, auto-generate wiki pages, detect contradictions, and weave everything into a knowledge graph. Your notes become a living wiki, not a pile of files.

### 2. 🧠 **Three Brains, One System**
Short-term memory ("what just happened?") → Mid-term knowledge ("what do I know?") → Long-term wisdom ("what have I learned?"). Session context flows naturally upward. No cold starts.

### 3. 📈 **It Learns From You**
Daily reflection, weekly synthesis, Bayesian belief updates. The system spots recurring patterns, surfaces blind spots, and adapts to your workflow. Every session makes the next one sharper.

### 4. 🔄 **Information → Knowledge → Wisdom**
5-stage pipeline: **Capture → Tidy → Organize → Distill → Fuse**. From raw notes to polished wiki to deep wisdom. Data in, decisions out.

### 5. 🏛️ **13 Agents, Each an Expert**
A Greek pantheon of AI specialists: **Sisyphus** orchestrates, **Prometheus** plans, **Momus** gatekeeps quality, **Hephaestus** executes, **Hermes** validates. Each with the right model for the job.

### 6. 🎯 **Smart Model Routing — No Manual Picking**
Private data → local Ollama. Complex reasoning → Anthropic. Cost-sensitive → DeepSeek. Per-task fallback chains. One config, zero guesswork.

### 7. 💡 **Built-in Decision Engine**
Log decisions, compute expected value, set stop-loss lines, detect anti-patterns. The system doesn't just remember what you decided — it helps you decide better next time.

### 8. 📚 **Seed Wisdom — 12 Mental Models + 21 Anti-Patterns**
Pre-loaded thinking frameworks: Map ≠ Territory, Circle of Competence, Second-Order Thinking, Hanlon's Razor, and more. Plus behavioral anti-pattern detection. Thinking tools, not just storage.

### 9. 🔗 **Obsidian + OKF Dual Format**
All notes are **Obsidian-compatible Markdown** with `[[wikilinks]]` and YAML frontmatter — open `~/.zen/vault/` directly in Obsidian, no import/export. Under the hood, wiki pages follow **OKF v0.1 (Open Knowledge Format)**: typed frontmatter (`type: concept|reference|tool|...`), bundle-relative links, structured index files. Two formats, one knowledge base.

### 10. 🏠 **Your Data, Your Rules**
Local-first by default. macOS Keychain for secrets. Sensitive data never touches the cloud unless you explicitly allow it. Zero vendor lock-in.

---

## 🚀 Quick Start

```bash
# Install (macOS)
brew install savechina/tap/zenspace

# Initialize
zen workspace init

# Write a note — the wiki builds itself
zen note create "Q3 Planning" --tag project

# Ask your knowledge graph
zen search run "Q3 planning"
```

[Quick Start Guide →](https://savechina.github.io/zenspace/quickstart.html) | [Installation →](https://savechina.github.io/zenspace/installation.html)

---

## 📖 Documentation

| Section | Description |
|---------|-------------|
| [Installation](https://savechina.github.io/zenspace/installation.html) | Homebrew, source, binary |
| [Quick Start](https://savechina.github.io/zenspace/quickstart.html) | 5-minute walkthrough |
| [CLI Commands](https://savechina.github.io/zenspace/cli-commands.html) | All 29 commands |
| [Providers & Auth](https://savechina.github.io/zenspace/configuration/providers.html) | 9 protocol types, API keys |
| [Agent Routing](https://savechina.github.io/zenspace/configuration/agent-routing.html) | Per-agent model assignment |
| [System Overview](https://savechina.github.io/zenspace/architecture/overview.html) | Architecture & data flow |

---

**GitHub:** https://github.com/savechina/zenspace
**License:** MIT
