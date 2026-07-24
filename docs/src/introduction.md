# Introduction

**Zenspace** is your **personal AI agentic workspace** — a self-learning knowledge system that builds your wiki, remembers across sessions, and evolves with your decisions.

Most AI tools are chat boxes. You talk, they respond, and tomorrow it's gone. Zenspace takes a different path: **your notes grow into a wiki. Your sessions build memory. Your patterns become wisdom.**

## The 10 Things That Make Zenspace Different

### 1. 📝 Notes That Build Themselves
Write normally — agents extract entities, auto-generate wiki pages, detect contradictions, and weave everything into a knowledge graph. Your notes become a living wiki, not a pile of files.

### 2. 🧠 Three Brains, One System
Short-term memory ("what just happened?") → Mid-term knowledge ("what do I know?") → Long-term wisdom ("what have I learned?"). Session context flows naturally upward. No cold starts.

### 3. 📈 It Learns From You
Daily reflection, weekly synthesis, Bayesian belief updates. The system spots recurring patterns, surfaces blind spots, and adapts to your workflow. Every session makes the next one sharper.

### 4. 🔄 Information → Knowledge → Wisdom
5-stage pipeline: **Capture → Tidy → Organize → Distill → Fuse**. From raw notes to polished wiki to deep wisdom. Data in, decisions out.

### 5. 🏛️ 13 Agents, Each an Expert
A Greek pantheon of AI specialists: **Sisyphus** orchestrates, **Prometheus** plans, **Momus** gatekeeps quality, **Hephaestus** executes, **Hermes** validates, **Zeus** judges. Each with the right model for the job.

### 6. 🎯 Smart Model Routing — No Manual Picking
Private data → local Ollama. Complex reasoning → Anthropic. Cost-sensitive → DeepSeek. Per-task fallback chains so nothing fails silently. One config, zero guesswork.

### 7. 💡 Built-in Decision Engine
Log decisions, compute expected value, set stop-loss lines, detect anti-patterns. The system doesn't just remember what you decided — it helps you decide better next time.

### 8. 📚 Seed Wisdom — 12 Mental Models + 21 Anti-Patterns
Pre-loaded thinking frameworks: Map ≠ Territory, Circle of Competence, Second-Order Thinking, Hanlon's Razor. Plus behavioral anti-pattern detection. Thinking tools, not just storage.

### 9. 🔗 **Obsidian + OKF Dual Format**
All notes are **Obsidian-compatible Markdown** with `[[wikilinks]]` and YAML frontmatter — open `~/.zen/vault/` directly in Obsidian, edit in both directions, no import/export. Under the hood, wiki pages follow **OKF v0.1 (Open Knowledge Format)**: typed frontmatter (`type: concept|reference|tool|...`), bundle-relative links, and structured index files. Two formats, one knowledge base.

### 10. 🏠 Your Data, Your Rules
Local-first by default. macOS Keychain for secrets. Sensitive data never touches the cloud unless you explicitly allow it. Zero vendor lock-in.

## How It Works (The 5-Stage Pipeline)

```
RAW NOTES ──► TIDY ──► ORGANIZE ──► DISTILL ──► FUSE
                                   │
                           Entity Extraction
                           Wiki Generation
                           Contradiction Detection
                                   │
                              WISDOM
                          (MEMORY.md + beliefs)
```

1. **Capture** — Notes, RSS feeds, raw files all go into inbox
2. **Tidy** — Clean, chunk, normalize into structured Markdown
3. **Organize** — Embed, index, classify — make everything searchable
4. **Distill** — Extract entities, compile wiki pages, detect contradictions
5. **Fuse** — Synthesize wisdom, update beliefs, promote to long-term memory

## Who Is It For?

- **Knowledge workers** who want their notes to actively work for them
- **Thinkers** who value structured decision-making and mental models
- **Privacy-conscious users** who want AI without surrendering data
- **Power users** who want to customize model routing across providers

---

Next: [Installation](installation.md)
