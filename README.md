# Zenspace

**Zen** is a local-first Rust CLI knowledge management tool with multi-LLM provider support (Ollama, OpenAI, Anthropic, DeepSeek, and more).

[中文文档](README_zh.md)

## Features

- **Local Knowledge Base** — Markdown files as data source, SQLite FTS5 + vector search for fast retrieval
- **Multi-Protocol LLM Routing** — Ollama, OpenAI, Anthropic, Gemini, Cohere, Mistral, and OpenAI-compatible APIs
- **Entity Extraction Pipeline** — Automatic entity extraction from notes, auto-generated Wiki pages
- **Agentic Sessions** — Session lifecycle management with 13 built-in agents across 4 tiers
- **5-Tier Search** — ripgrep → FTS5 → vector embeddings → entity graph → LLM fallback
- **macOS Keychain Integration** — Secure credential storage with automatic fallback

## Installation

### Homebrew (macOS, recommended)

```bash
brew tap savechina/zenspace
brew install zenspace
```

### From Source

```bash
git clone https://github.com/savechina/zenspace.git
cd zenspace
bin/build 
./target/release/zen --help
```

### Cargo Install (coming soon)

```bash
bin/install
```

### Binary Download

Download pre-built macOS binaries from [GitHub Releases](https://github.com/savechina/zenspace/releases).

## Quick Start

```bash
# Initialize workspace
zen workspace init

# Create a note
zen note create "Design Doc" --tag project

# Search knowledge base
zen search run "design"

# View configuration
zen config show
```

## Documentation

- [README_zh.md](README_zh.md) — 中文文档
- [AGENTS.md](AGENTS.md) — Architecture guide
- [config/config.toml](config/config.toml) — Provider configuration examples

---

**GitHub**: https://github.com/savechina/zenspace
