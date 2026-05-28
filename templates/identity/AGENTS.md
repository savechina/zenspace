# AGENTS.md — Operating Instructions / 操作指令

This file contains operating instructions for how the agent should behave in this
project/workspace. It defines workflows, conventions, and project-specific rules.

本文件包含智能体在此项目/工作空间中行为的指令。
它定义工作流、惯例和项目特定规则。

---

## Project Info / 项目信息

- **Project / 项目:** (to be filled / 待填写)
- **Stack / 技术栈:** (to be filled / 待填写)

## Workflow / 工作流

<!--
  How to approach common tasks in this project.
  如何处理此项目中的常见任务。
-->

### Creating a New Feature / 创建新功能

1. Check existing patterns first / 首先检查现有模式
2. Make minimal changes / 进行最小更改
3. Add tests for new behavior / 为新行为添加测试
4. Verify lint passes / 验证 lint 通过

### Bug Fixes / Bug 修复

1. Reproduce the issue
2. Write a test that fails
3. Fix the code
4. Verify test passes
5. Run full test suite

## Conventions / 惯例

<!--
  Project-specific coding conventions and rules.
  项目特定的编码惯例和规则。
-->

- Follow existing code style / 遵循现有代码风格
- Use `snake_case` for Rust modules / Rust 模块使用 `snake_case`
- Errors: use `Result<T, E>` with thiserror / 错误：对 thiserror 使用 `Result<T, E>`

## Rules / 规则

<!--
  Hard constraints that must always be followed.
  必须始终遵循的硬性约束。
-->

- Never remove tests without replacing them
  永远不要在没有替换它们的情况下删除测试
- Never commit directly to main / 永远不要直接提交到 main
- Always run lint before committing / 提交前始终运行 lint
