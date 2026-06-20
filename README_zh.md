# Zenspace

**Zen** 是一个本地优先的 Rust CLI 知识管理工具，支持多 LLM 提供商（Ollama、OpenAI、Anthropic、DeepSeek 等）。

[English README](README.md)

## 产品特性

- **本地知识库** — Markdown 文件作为数据源，SQLite FTS5 + 向量搜索加速检索
- **多协议 LLM 路由** — Ollama、OpenAI、Anthropic、Gemini、Cohere、Mistral 及 OpenAI-compatible API
- **实体提取管道** — 自动从笔记提取实体，生成 Wiki 页面
- **Agentic 会话** — 会话生命周期管理，13 种内置 Agent，4 层架构
- **5 层搜索** — ripgrep → FTS5 → 向量嵌入 → 实体图 → LLM 回退
- **macOS Keychain 集成** — 安全凭证存储，自动降级回退

## 安装

### Homebrew（推荐 macOS）

```bash
brew tap savechina/zenspace
brew install zenspace
```

### 从源码构建

```bash
git clone https://github.com/savechina/zenspace.git
cd zenspace
bin/build
./target/release/zen --help
```

### Cargo 安装（即将推出）

```bash
bin/install
```


### 二进制下载

从 [GitHub Releases](https://github.com/savechina/zenspace/releases) 下载 macOS 预编译二进制文件。

## 快速开始

```bash
# 初始化工作空间
zen workspace init

# 创建笔记
zen note create "设计文档" --tag project

# 搜索知识库
zen search run "设计"

# 查看配置
zen config show
```

## LLM 提供商配置

Zen 支持多种 LLM 提供商，配置文件位于 `config/config.toml`（编译时嵌入）或 `~/.zen/config.toml`（用户覆盖）。

### 支持的协议类型

| 类型 | 描述 | 示例 |
|------|------|------|
| `ollama` | 本地 Ollama 服务 | 无需 API Key |
| `openai` | OpenAI API | `api_key = { env = "OPENAI_API_KEY" }` |
| `anthropic` | Anthropic Messages API | `api_key = { env = "ANTHROPIC_API_KEY" }` |
| `gemini` | Google Gemini API | `api_key = { env = "GEMINI_API_KEY" }` |
| `cohere` | Cohere API | `api_key = { env = "COHERE_API_KEY" }` |
| `mistral` | Mistral API | `api_key = { env = "MISTRAL_API_KEY" }` |
| `openai-compatible` | OpenAI 兼容 API | DeepSeek/Groq/Perplexity/阿里云等 |
| `anthropic-compatible` | Anthropic 兼容 API | Moonshot/MiniMax 等 |

### 配置示例

```toml
# 默认提供商
default_provider = "ollama"
default_model = "qwen3.6:35b-mlx"

# 本地 Ollama（无 API Key）
[providers.ollama]
type = "ollama"
base_url = "http://127.0.0.1:11434"

# OpenAI 兼容 API（DeepSeek）
[providers.deepseek]
type = "openai-compatible"
base_url = "https://api.deepseek.com"
api_key = { env = "DEEPSEEK_API_KEY" }
default_model = "deepseek-v4-flash"

# Anthropic API
[providers.anthropic]
type = "anthropic"
api_key = { env = "ANTHROPIC_API_KEY" }
default_model = "claude-haiku-4-5"
```

### 设置环境变量

```bash
# 设置 API Key
export OPENAI_API_KEY="sk-..."
export DEEPSEEK_API_KEY="sk-..."
export ANTHROPIC_API_KEY="sk-ant-..."
```

## 命令列表

| 命令 | 说明 |
|------|------|
| `zen` | 启动 TUI 交互界面 |
| `zen version` | 显示版本 |
| `zen workspace init` | 初始化 `.zen/` 工作空间 |
| `zen config show` | 显示配置（嵌入式/用户/环境变量） |
| `zen note create "标题"` | 创建笔记 |
| `zen search run "关键词"` | 搜索知识库 |
| `zen consolidate run` | 运行实体提取管道 |
| `zen lint run` | 检查孤立页面、断裂 Wiki 链接 |
| `zen session start` | 启动 Agentic 会话 |
| `zen llm providers` | 列出可用 LLM 提供商 |
| `zen llm test <provider>` | 测试提供商连接 |

完整命令列表见 `zen --help`。

## 项目结构

```
zenspace/
├── crates/               # 12 个 Rust workspace crates
│   ├── zen/              # 二进制入口（13 行）
│   ├── zen-cli/          # CLI 入口 + TUI（24 个命令）
│   ├── zen-core/         # 配置/错误/路径/常量（13 个模块）
│   ├── zen-provider/     # 多协议 LLM 路由（13 个提供商）
│   ├── zen-vault/    # 笔记/Wiki/5 层搜索/实体提取
│   ├── zen-data/         # SQLite 双 API（sqlx + rusqlite）
│   ├── zen-agents/       # 13 个 Agent/4 层架构/黑板系统
│   ├── zen-memory/       # 身份上下文（SOUL.md/MEMORY.md）
│   ├── zen-auth/         # macOS Keychain 凭证管理
│   ├── zen-service/      # 业务逻辑（starter/wps/cleanup）
│   ├── zen-plugin/       # WASM 沙箱 + MCP 服务器
│   └── zen-gateway/      # HTTP 守护进程（axum）
├── config/               # 嵌入式 config.toml
├── templates/            # Tera 模板
└── docs/specs/           # 架构规范文档（~400KB）
```

## 开发

```bash
# 构建
cargo build

# 测试
cargo test

# 代码检查
cargo clippy -- -D warnings
cargo fmt --all --check

# 本地运行
cargo run --bin zen -- --help
```

## 发布流程

### 版本规范

遵循 [Semantic Versioning](https://semver.org/)：MAJOR.MINOR.PATCH

### 发布步骤

```bash
# 使用发布脚本（推荐）
./bin/release patch    # 0.1.0 → 0.1.1
./bin/release minor    # 0.1.0 → 0.2.0
./bin/release major    # 0.1.0 → 1.0.0

# 手动发布
echo "0.1.2" > VERSION
git commit -am "release: v0.1.2"
git tag v0.1.2
git push origin main --tags
```

### 发布产物（macOS）

- `zen-{version}-aarch64-apple-darwin.tar.gz`（Apple Silicon）
- `zen-{version}-x86_64-apple-darwin.tar.gz`（Intel）

## 许可证

MIT License — 见 [LICENSE.txt](LICENSE.txt)

## 文档

- [README.md](README.md) — English documentation
- [AGENTS.md](AGENTS.md) — 项目架构指南
- [config/config.toml](config/config.toml) — 提供商配置示例

---

**GitHub**: https://github.com/savechina/zenspace
