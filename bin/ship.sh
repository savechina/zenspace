#!/usr/bin/env bash
# PURPOSE: 通用发布提交：lint 门禁 + 拦截 AI 署名 + 安全暂存；脚本自身永不追加 trailer，默认不 push
# USAGE: ./bin/ship.sh -m "subject" [-b "body"] [--all] [--no-verify] [--no-hooks] [--allow-main] [--allow-ai-trailer] [--dry-run] [--suggest] [--auto-message] [--type T] [-- path...]
#   -m SUBJECT            提交标题（缺省则调起编辑器手写）
#   -b BODY               提交正文（可重复 -b 追加段落）
#   -- path...            额外显式暂存路径（用于点名 untracked 新文件）
#   --all                 把剩余 untracked 也全部暂存（默认：有剩余 untracked 则中止）
#   --no-verify           跳过 lint 门禁（pre-commit hook 照装，单次放行）
#   --no-hooks            不安装/不依赖 git hooks（无 hook 时脚本内直跑 bin/lint）
#   --allow-main          允许在 main/master 分支提交（默认拒绝）
#   --allow-ai-trailer    允许消息中带 AI 署名（默认 commit-msg hook 拦截）
#   --dry-run             只打印将要执行的动作，不改工作区
#   --suggest             只暂存+输出结构化变更摘要（含启发式 Conventional Commits 草稿），不提交（给 agent/summarizer skill 用）
#   --auto-message        用启发式草稿直接当提交标题（快路径；--type 可覆盖类型）
#   --type T              覆盖启发式类型（feat|fix|docs|style|refactor|perf|test|build|ci|chore|revert）
# EXPECTED: 装好 .git/hooks/{pre-commit,commit-msg}（幂等，只碰自家标记的 hook）；1 个无 AI 署名、符合 Conventional Commits 的 commit；最后只提示 push 命令
# ERRORS: lint 失败 → 修完重跑；有剩余 untracked 且无 --all → 中止并列出；消息命中 AI 署名或格式不符 → hook 拦截
# AGENT PROTOCOL (coding agent + summarizer skill):
#   1. agent: ./bin/ship.sh --suggest [--all] [-- path...]  → 暂存 + 输出 [ship-suggest] 结构化摘要
#   2. skill: 按 Conventional Commits 精修标题/正文（type(scope): subject，subject<=120）
#   3. agent: ./bin/ship.sh -m "title" [-b "body"] [同上 flags]  → hooks(QA 门+格式门+AI 拦截)后提交
#   QA 扩展点: SHIP_QA_EXTRA="cmd args..."（pre-commit 在 bin/lint 后追加执行）；SHIP_RUN_TESTS=1 时追加 cargo test --workspace（慢，默认关）
set -euo pipefail

MARKER="# managed by bin/ship.sh (zenspace-ship)"
MSG=""
BODIES=()
INCLUDE_ALL=0
NO_VERIFY=0
NO_HOOKS=0
ALLOW_MAIN=0
ALLOW_AI_TRAILER=0
DRY_RUN=0
SUGGEST=0
AUTO_MESSAGE=0
TYPE_OVERRIDE=""
EXTRA_PATHS=()

usage() { sed -n '2,/^set -euo/p' "$0" | sed '$d'; }

while [ $# -gt 0 ]; do
  case "$1" in
    -m|--message) MSG="${2:?missing value for $1}"; shift 2 ;;
    -b|--body) BODIES+=("${2:?missing value for $1}"); shift 2 ;;
    --all) INCLUDE_ALL=1; shift ;;
    --no-verify) NO_VERIFY=1; shift ;;
    --no-hooks) NO_HOOKS=1; shift ;;
    --allow-main) ALLOW_MAIN=1; shift ;;
    --allow-ai-trailer) ALLOW_AI_TRAILER=1; shift ;;
    --dry-run) DRY_RUN=1; shift ;;
    --suggest) SUGGEST=1; shift ;;
    --auto-message) AUTO_MESSAGE=1; shift ;;
    --type) TYPE_OVERRIDE="${2:?missing value for $1}"; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    --) shift; while [ $# -gt 0 ]; do EXTRA_PATHS+=("$1"); shift; done ;;
    *) echo "unknown flag: $1 (see --help)" >&2; exit 2 ;;
  esac
done
TYPES="feat|fix|docs|style|refactor|perf|test|build|ci|chore|revert"
if [ -n "$TYPE_OVERRIDE" ] && ! printf '%s' "$TYPE_OVERRIDE" | grep -Eq "^($TYPES)$"; then
  echo "ABORT: --type 必须是 $TYPES 之一" >&2; exit 2
fi

export CI=true GIT_PAGER=cat PAGER=cat
HOOK_DIR="$(git rev-parse --git-dir)/hooks"

# 0. 分支护栏（main/master 默认拒绝）
BRANCH="$(git branch --show-current)"
case "$BRANCH" in
  main|master)
    if [ "$ALLOW_MAIN" != "1" ]; then
      echo "ABORT: 当前在 $BRANCH 分支，提交请用特性分支，或加 --allow-main" >&2
      exit 1
    fi
    ;;
esac
echo "branch: $BRANCH"

# 1. 安装 git hooks（幂等：缺失或带自家标记才写，不覆盖用户手写 hook）
install_hook() {
  local name="$1" content="$2" target="$HOOK_DIR/$1"
  if [ -f "$target" ] && ! grep -qF "$MARKER" "$target" 2>/dev/null; then
    echo "hooks: $name 已存在且非本脚本管理，跳过（手动处理）"
    return 0
  fi
  if [ "$DRY_RUN" = "1" ]; then
    echo "hooks: [dry-run] 将写入 $target"
    return 0
  fi
  printf '%s\n' "$content" > "$target"
  chmod +x "$target"
  echo "hooks: $name 已安装"
}
if [ "$NO_HOOKS" != "1" ]; then
  install_hook "pre-commit" "$MARKER
#!/usr/bin/env bash
# zenspace-ship: 提交前跑仓库标准门禁
set -e
if [ \"\${SHIP_SKIP_VERIFY:-0}\" = \"1\" ]; then echo '[ship] verify skipped (SHIP_SKIP_VERIFY=1)'; exit 0; fi
export CI=true GIT_PAGER=cat PAGER=cat
bin/lint
if [ -n \"\${SHIP_QA_EXTRA:-}\" ]; then echo \"[ship] extra QA: \$SHIP_QA_EXTRA\"; eval \"\$SHIP_QA_EXTRA\"; fi
if [ \"\${SHIP_RUN_TESTS:-0}\" = \"1\" ]; then cargo test --workspace; fi"
  install_hook "commit-msg" "$MARKER
#!/usr/bin/env bash
# zenspace-ship: Conventional Commits 格式门 + AI 署名拦截（人工合著不受影响）
F=\"\$1\"
SUBJECT=\$(head -n1 \"\$F\")
case \"\$SUBJECT\" in Merge\\ *) exit 0;; esac
if [ \"\${SHIP_ALLOW_AI_TRAILER:-0}\" != \"1\" ] && grep -Ei 'co-authored-by:[[:space:]]*.*(sisyphus|clio-agent|sisyphuslabs|claude|anthropic|openai|chatgpt|copilot|gemini|codex|cursor|aider|assistant@|\\[bot\\]|noreply)' \"\$F\"; then
  echo '[ship] BLOCKED: 检测到 AI 署名 trailer，拒绝提交。' >&2
  echo '[ship] 人工合著署名不受影响；确需放行请用 ./bin/ship.sh --allow-ai-trailer 重试。' >&2
  exit 1
fi
if ! printf '%s' \"\$SUBJECT\" | grep -Eq '^(feat|fix|docs|style|refactor|perf|test|build|ci|chore|revert)(\\([^)]+\\))?!?: [^ ].*'; then
  echo '[ship] BLOCKED: 标题不符合 Conventional Commits：type(scope): subject' >&2
  echo '[ship] type 限 feat|fix|docs|style|refactor|perf|test|build|ci|chore|revert；示例: feat(zen-agents): add skill auto-route' >&2
  exit 1
fi
if [ \"\${#SUBJECT}\" -gt 120 ]; then echo \"[ship] BLOCKED: 标题超 120 字符（\${#SUBJECT}）\" >&2; exit 1; fi"
else
  echo "hooks: skipped (--no-hooks)"
fi

# 2. 暂存：tracked 全收；untracked 必须点名（-- path...）或 --all，否则中止
run() { if [ "$DRY_RUN" = "1" ]; then echo "[dry-run] $*"; else "$@"; fi }
run git add -u
if [ "${#EXTRA_PATHS[@]}" -gt 0 ]; then
  run git add -- "${EXTRA_PATHS[@]}"
fi
LEFTOVER="$(git status --porcelain | grep -c '^??' || true)"
if [ "$LEFTOVER" != "0" ] && [ "$DRY_RUN" != "1" ]; then
  if [ "$INCLUDE_ALL" = "1" ]; then
    git add -A
    echo "staged all incl. untracked (--all)"
  else
    echo "ABORT: 仍有 $LEFTOVER 个 untracked 未暂存（git add -u 碰不到它们，漏提则提交树可能编译不过）：" >&2
    git status --porcelain | grep '^??' >&2
    echo "处理：点名追加 ./bin/ship.sh ... -- <paths>，或确认无误后加 --all" >&2
    exit 1
  fi
fi
if [ "$DRY_RUN" = "1" ]; then echo "[dry-run] 将检查暂存区并提交"; exit 0; fi
if git diff --cached --quiet; then
  echo "ABORT: 暂存区为空，无可提交内容" >&2
  exit 1
fi
echo "staged:"; git diff --cached --stat | tail -1

# 3. 启发式 Conventional Commits 草稿（给 --suggest/--auto-message 用；skill 精修优先）
suggest_message() {
  local files added_cnt mod_cnt del_cnt add_lines del_lines scope type top
  files="$(git diff --cached --name-only)"
  added_cnt="$(git diff --cached --diff-filter=A --name-only | grep -c . || true)"
  mod_cnt="$(git diff --cached --diff-filter=M --name-only | grep -c . || true)"
  del_cnt="$(git diff --cached --diff-filter=D --name-only | grep -c . || true)"
  add_lines="$(git diff --cached --numstat | awk '{a+=$1} END {print a+0}')"
  del_lines="$(git diff --cached --numstat | awk '{d+=$2} END {print d+0}')"
  scope="$(printf '%s' "$files" | awk -F/ '{if ($1=="crates" && NF>1) print $2; else print $1}' | sort | uniq -c | sort -rn | head -1 | awk '{print $2}')"
  if [ -n "$TYPE_OVERRIDE" ]; then
    type="$TYPE_OVERRIDE"
  elif printf '%s' "$files" | grep -Eq '(\.md$|^docs/|README)' && ! printf '%s' "$files" | grep -Eqv '(\.md$|^docs/|README)'; then
    type="docs"
  elif printf '%s' "$files" | grep -Eq '(test|tests/|_test\.|spec)' && ! printf '%s' "$files" | grep -Eqv '(test|tests/|_test\.|spec)'; then
    type="test"
  elif printf '%s' "$files" | grep -Eq '(\.toml$|\.lock$|\.github/|Dockerfile|\.yml$)' && ! printf '%s' "$files" | grep -Eqv '(\.toml$|\.lock$|\.github/|Dockerfile|\.yml$)'; then
    type="chore"
  elif [ "$added_cnt" != "0" ]; then
    type="feat"
  else
    type="fix"
  fi
  top="$(git diff --cached --numstat | awk '{print $1+$2, $3}' | sort -rn | head -3 | awk '{print $2}' | xargs -n1 basename 2>/dev/null | paste -sd, -)"
  printf '[ship-suggest]\ntype=%s\nscope=%s\nsubject=%s(%s): %s files changed (+%s/-%s), top: %s\nfiles_added=%s\nfiles_modified=%s\nfiles_deleted=%s\n' \
    "$type" "$scope" "$type" "$scope" "$((added_cnt + mod_cnt + del_cnt))" "$add_lines" "$del_lines" "$top" "$added_cnt" "$mod_cnt" "$del_cnt"
  echo "---stat---"
  git diff --cached --stat | tail -8
}
if [ "$SUGGEST" = "1" ]; then
  suggest_message
  exit 0
fi
if [ "$AUTO_MESSAGE" = "1" ] && [ -z "$MSG" ]; then
  MSG="$(suggest_message | awk -F= '/^subject=/ {sub(/^subject=/, ""); print}')"
  echo "auto-message: $MSG"
fi

# 4. 门禁直跑（仅 --no-hooks 时需要；有 hook 则由 pre-commit 在提交时执行）
if [ "$NO_HOOKS" = "1" ] && [ "$NO_VERIFY" != "1" ]; then
  bin/lint
  echo "lint OK"
fi

# 5. 提交：脚本自身永不拼接任何 trailer；--no-verify/--allow-ai-trailer 透传给 hook 做单次放行
export SHIP_SKIP_VERIFY="$NO_VERIFY"
export SHIP_ALLOW_AI_TRAILER="$ALLOW_AI_TRAILER"
ARGS=()
if [ -n "$MSG" ]; then
  ARGS+=(-m "$MSG")
  for b in "${BODIES[@]:-}"; do ARGS+=(-m "$b"); done
fi
git commit "${ARGS[@]:-}"
echo "done. push with: git push origin $BRANCH"
