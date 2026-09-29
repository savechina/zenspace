# 全格式文档 → Markdown 转换（ingest 管线）— 设计决策记录

**日期**：2026-09-29 · **状态**：T2/T3 已落地（PR-1 `1e8d73c` PDF、PR-2 `c5d8123` Office），T4（pandoc sidecar 长尾）保留待办 · **依据**：Constitution XI（Design-First & Reuse）

## 1. 问题

zen 的知识摄取入口只认 UTF-8 文本：

- `zen-vault/src/ingest/web.rs::ingest_local_file` = `fs::read_to_string` — PDF/DOCX 等二进制格式直接读入失败（invalid UTF-8）；
- host_sources 暂存（zen-agents scheduler，`stage_host_dir`/`sweep_host_sources`）只收 `md`/`txt` 扩展名。

目标：任意常见文档格式（pdf/docx/pptx/xlsx/html/epub…）落入 inbox 或受管 host 目录后，自动转为 Markdown 进入既有 distill 管线。

## 2. 调研结论（2026-09-29，双路检索：Rust PDF crates + 全格式转换工具）

### 2.1 转换工具全景

| 工具 | 格式覆盖 | 权重 | License | 采用度 | 关键短板 |
|---|---|---|---|---|---|
| **pandoc** | docx/pptx/xlsx/html/epub/odt/rtf → md（**不能读 PDF**） | 单静态二进制，零运行时 | GPL-2.0+（外部二进制，不链接） | 20 年事实标准 | 复杂表格有损；PDF 缺位 |
| **markitdown** (Microsoft) | 六格式全覆盖 + 图片/音频 | 需 Python ≥3.10 运行时 | MIT | ~186k stars，~14.9M 下载/月 | README 自述非高保真；PDF 走 pdfminer 纯文本抽取 |
| **docling** (IBM) | 六格式全覆盖，PDF 布局模型最佳 | 重 ML（100-500MB 模型，4-8GB RAM） | MIT | 63k stars，70M 下载 | 模型下载与内存，对简单 DOCX/HTML 杀鸡用牛刀 |
| **marker / MinerU / unstructured** | PDF 专精/企业 ETL | GPU/重模型 | OpenRAIL-M / AGPL / Apache | 高 | 不适合轻量 CLI 内嵌 |

### 2.2 PDF（Rust 原生）依赖审计

| Crate | 采用度 | 纯 Rust | 文本抽取 | 结论 |
|---|---|---|---|---|
| **pdf-extract 0.12.1** | 5.56M 总 / 2.89M 90天，598★，MIT，活跃（2026-09 仍发版） | ✅（基于 lopdf） | ✅ 基础、按页 | **首选** |
| pdf_oxide | 1.06M / 859K（10 个月内），1053★ | ✅ | ✅ 最强（阅读序/CJK/标题感知 Markdown） | 观察名单：单维护者、<1 年，暂不作首选 |
| lopdf 0.45 | 21.5M / 10.4M，2255★ | ✅ | ❌ 需自建字体/CMap 解析 | 地基层，非方案 |
| pdf (pdf-rs) | 616K | ✅ | ❌ 无 text 模块 | 排除 |
| pdfium-render | 2.70M | ❌ C++（需随附 libpdfium） | ✅ 最佳 | 排除（违反 <50MB/纯 Rust 姿态） |
| mupdf-rs | 1.69M | ❌ C | ✅ | 排除：**AGPL-3.0 vs 本项目 MIT** |
| rga/paperless-ngx 路线 | — | — | — | shell out 到 poppler `pdftotext`（外部二进制依赖） |

**决定性事实（本地 `cargo tree` 验证）**：`pdf-extract 0.10.0` 与 `lopdf 0.38/0.39` **已经在 zen 依赖树内**——`memvid-core 2.0.140` 依赖 `pdf-extract 0.10`。同 minor 直接依赖 = **零新增版本岛**；若上最新 0.12 会新增 lopdf 0.42 第三岛（T121 反模式）。已知缺陷：pdf-extract 对畸形输入有 panic 史（#160/#154/#133，上游活跃修复中）——摄取路径必须 fail-open 进隔离区。

### 2.3 Office 格式（Rust 原生，2026 新生态）

`office_oxide`（6 格式 DOC/XLS/PPT/DOCX/XLSX/PPTX，MIT/Apache-2.0，~712k 下载）、`undoc`（DOCX/XLSX/PPTX，MIT，活跃）、`anytomd-rs`（DOCX/PPTX/XLSX/HTML/CSV/JSON/XML）。**均为 2026 新生代，采用度尚未经受时间检验——采纳前需独立迷你审计（同本节方法）**。

### 2.4 集成先例

- **rga (Rust)**：sidecar 模式典范——$PATH 探测转换器（pandoc 处理 epub/odt/docx/html），缺失二进制→逐 adapter 报错；
- **paperless-ngx**：可配置二进制路径 + 失败 `ParseError` 指名命令；
- **obsidian-import**（Python）：混合先例——Rust wheel 默认 + markitdown/docling 可选后端。

## 3. 决策（PROPOSED）：三层渐进，纯 Rust 优先，sidecar 兜底

| 层 | 格式 | 机制 | 依赖成本 |
|---|---|---|---|
| **T1 原生-已有** | md/txt（现状） | 直读 | 0 |
| **T2 原生-新增** | **pdf**：`pdf-extract 0.10`（钉在 memvid-core 同版）；html：复用既有 `extract_readable_content` | 进程内 crate | **零新增版本岛** |
| **T3 原生-新增（已落地）** | docx/xlsx/pptx：`office_oxide 0.1`（2026-09-29 迷你审计 GO：719k 下载/600k 近90天/15 个真实消费者/fuzz+制度级 CI；undoc 20k、anytomd 9k 因采用不足 NO-GO） | 进程内 crate + catch_unwind | 新依赖（纯 Rust） |
| **T3 原生-待审计** | docx/pptx/xlsx | `office_oxide`/`undoc` 迷你审计通过后加入 | 新依赖（各 ~百 KB） |
| **T4 sidecar-可选** | epub/odt/rtf 等长尾 | $PATH 探测 pandoc（rga 模式）；缺失→`zen doctor` 提示安装命令，**不阻塞** T1-T3 | 外部可选 |

**挂钩点**（两处，均为现状缺口）：`ingest_local_file` 按扩展名分派转换；host_sources 暂存过滤器 `md|txt` → `md|txt|pdf|(docx…)`，转换产物进 inbox 暂存树，原件按既有 `raw_policy` 处置。

**横切不变式**：
- 转换失败/panic → **隔离区 + warn**（复用 `max_ingest_bytes` 隔离路径；`catch_unwind` 边界包住 pdf-extract panic 史）；
- 纯文本格式行为逐字节不变（默认路径零回归，T046 同款纪律）；
- 每格式独立单元测试（最小样张入库 `tests/fixtures/`）；
- 分期实施：**PR-1 PDF（T2）→ PR-2 Office（T3，先审计）→ PR-3 pandoc sidecar（T4）**，一项一 PR。

## 4. 已否决方案

- **markitdown 为主**：引入 Python 运行时，违反轻量 CLI 姿态；
- **docling**：模型权重与内存超预算（Constitution X）；
- **pdfium/mupdf/poppler**：原生二进制随附 / AGPL / 系统库依赖；
- **自研解析器**：Constitution XI 直接禁止（pdf-rs 无 text 模块即前车之鉴）。

## 5. 来源

crates.io/GitHub API（2026-09-29 实抓：pdf-extract/lopdf/pdf/pdfium-render/mupdf/pdf_oxide/office_oxide/undoc/anytomd-rs/kreuzberg/omniparse/pdf-inspector）、pandoc 手册、markitdown/docling 官方仓库与 benchmark（actualize、olmocr-bench）、rga README/config、paperless-ngx parsers.py、本地 `cargo tree` 验证。pdf_oxide 100% pass 基准为厂商自发布数据，仅作方向参考。
