#!/bin/sh
#@tags: domain:cleanup, type:nightly, schedule:weekly
set -u

# ============================ Claude Code ============================
# 清历史会话 transcript（/resume 列表来源）。删闲置满 30 天的 *.jsonl。
# memory/*.md 持久记忆不是 .jsonl，天然不受影响。
_claude_dir="$HOME/.claude/projects"
if [ -d "$_claude_dir" ]; then
    find "$_claude_dir" -type f -name "*.jsonl" -mtime +30 -delete 2>/dev/null || true
fi

# ============================ Codex ==================================
# 清旧会话 rollout 文件（本地 sessions 历史；config 已 disable_response_storage，
# 服务端无历史）。不碰 ~/.codex/.tmp（插件缓存）、不碰 sqlite（记忆/状态）。
_codex_dir="$HOME/.codex/sessions"
if [ -d "$_codex_dir" ]; then
    find "$_codex_dir" -type f -name "*.jsonl" -mtime +30 -delete 2>/dev/null || true
fi

# ============================ Cursor ================================
# 清纯缓存/日志（与聊天记忆无关）。不动 state.vscdb（聊天/状态，单文件数据库）、
# 不动 snapshots/（代码快搜缓存，避免重新索引）、不动 User/globalStorage。
_cursor_dir="$HOME/.config/Cursor"
if [ -d "$_cursor_dir" ]; then
    find "$_cursor_dir/logs"     -type f -mtime +30 -delete 2>/dev/null || true
    find "$_cursor_dir/Cache"    -type f -mtime +30 -delete 2>/dev/null || true
    find "$_cursor_dir/GPUCache" -type f -mtime +30 -delete 2>/dev/null || true
fi
