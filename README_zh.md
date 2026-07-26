# Zenspace

**你的个人 AI Agentic 工作空间** — 一个自进化的知识系统。自动构建 Wiki、记住一切、越用越聪明。

> 🧠 **你思考，它构建。** 像平常一样写笔记。Zenspace 在后台运行——提取实体、连接想法、构建 Wiki 页面、发现模式、从决策中学习。短期记忆、长期智慧、13 个 Agent 团队为你工作。Markdown 进，Markdown 出。数据永远是你的。

[English README](README.md) | [完整用户指南 →](https://savechina.github.io/zenspace/)

---

## Zenspace 的 10 个独特之处

### 1. 📝 **笔记即 Wiki，自动生成**
正常写笔记即可——Agent 自动提取实体、生成 Wiki 页面、检测矛盾、织入知识图谱。笔记不止是存储，而是自我生长的知识库。

### 2. 🧠 **三重记忆，一个系统**
短期记忆（"刚才说了什么"）→ 中期知识（"我知道什么"）→ 长期智慧（"我学到了什么"）。会话上下文自然流动，没有冷启动。

### 3. 📈 **它会从你身上学习**
每日反思、每周整合、贝叶斯信念更新。系统识别重复模式、发现盲点、适应你的工作流程。每一次会话都让下一次更聪明。

### 4. 🔄 **信息 → 知识 → 智慧**
5 段流水线：**捕获 → 整理 → 组织 → 提炼 → 融合**。从原始笔记到精炼 Wiki 到深层智慧。数据进，决策出。

### 5. 🏛️ **13 位 Agent，各司其职**
一支希腊神祇 AI 专家团队：**Sisyphus** 主编排、**Prometheus** 战略规划、**Momus** 质量门禁、**Hephaestus** 深度执行、**Hermes** 交付验证。各取最强模型。

### 6. 🎯 **智能路由，不用手动选**
隐私数据 → 本地 Ollama。复杂推理 → Anthropic。成本敏感 → DeepSeek。每任务自带回退链。一份配置，最优选择。

### 7. 💡 **内置决策引擎**
记录决策、计算期望值、设置止损线、检测反模式。系统不只记住你做了什么——它帮你下次做得更好。

### 8. 📚 **种子智慧：12 思维模型 + 21 反模式**
预装思维框架：地图≠疆域、能力圈、二阶思维、汉隆剃刀……外加行为反模式检测。开箱即有思考工具，不止是存储工具。

### 9. 🔗 **Obsidian + OKF 双格式**
所有笔记是 **Obsidian 兼容 Markdown**——`[[wikilinks]]` + YAML frontmatter，直接打开 `~/.zen/vault/` 就是 Obsidian 仓库，无需导入导出。底层 Wiki 页面遵循 **OKF v0.1（开放知识格式）**：带类型的前置声明（`type: concept|reference|tool|...`）、包内相对链接、结构化索引文件。两种格式，一个知识库。

### 10. 🏠 **数据主权，云端可选**
默认本地优先。macOS Keychain 安全存储。敏感数据永不碰云端，除非你显式授权。零厂商锁定。

---

## 🚀 快速开始

```bash
# 安装（macOS）
brew install savechina/tap/zenspace

# 初始化
zen workspace init

# 写笔记——Wiki 自动构建
zen note create "Q3 规划" --tag project

# 问你的知识图谱
zen search run "Q3 规划"
```

[快速开始 →](https://savechina.github.io/zenspace/quickstart.html) | [安装 →](https://savechina.github.io/zenspace/installation.html)

---

## 📖 文档目录

| 章节 | 说明 |
|------|------|
| [安装](https://savechina.github.io/zenspace/installation.html) | Homebrew、源码、二进制 |
| [快速开始](https://savechina.github.io/zenspace/quickstart.html) | 5 分钟上手 |
| [CLI 命令](https://savechina.github.io/zenspace/cli-commands.html) | 全部 29 条命令 |
| [提供商与认证](https://savechina.github.io/zenspace/configuration/providers.html) | 9 种协议、API Key |
| [Agent 路由](https://savechina.github.io/zenspace/configuration/agent-routing.html) | 按 Agent 分配模型 |
| [系统架构](https://savechina.github.io/zenspace/architecture/overview.html) | 架构与数据流 |

---

**GitHub:** https://github.com/savechina/zenspace
**许可证:** MIT
