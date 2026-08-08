#!/usr/bin/env bash
# scripts/e2e/tauri-devtools-invoke.sh
#
# 通过 Tauri DevTools Console 注入 JavaScript 执行 IPC 命令。
# 依赖 CGEvent 精确输入（cgtype.py）。
#
# 用法：
#   ./tauri-devtools-invoke.sh "await window.__TAURI_INTERNALS__.invoke('ping', {message:'hello'})"
#   ./tauri-devtools-invoke.sh --list-projects
#   ./tauri-devtools-invoke.sh --create-project /tmp/test-proj
#
# 前置条件：
#   - Tauri 窗口在焦点，DevTools 已打开（Cmd+Option+I）
#   - CGEvent 脚本在 /tmp/
#   - 系统输入源为 ABC

set -euo pipefail

E2E_VENV="/tmp/e2e_venv"
CGTYPE="/tmp/cgtype.py"
CGCLICK="/tmp/cgclick.py"

# DevTools Console 坐标（底部面板模式，Tauri 窗口右侧布局）
# 这些坐标是 Tauri 窗口内 DevTools 底部面板的 Console 输入区域
# 需要根据实际 DevTools 位置调整
COORD_CONSOLE_INPUT="1550 910"

switch_ime() {
    osascript -e 'tell application "System Events" to keystroke " " using {command down, control down}' 2>/dev/null
    sleep 0.3
}

click_console() {
    $E2E_VENV/bin/python "$CGCLICK" "$COORD_CONSOLE_INPUT" 80
    sleep 0.2
}

clear_console() {
    # Ctrl+L 清空 Console
    osascript -e 'tell application "System Events" to keystroke "l" using {control down}'
    sleep 0.2
}

invoke() {
    local js="$1"
    click_console
    $E2E_VENV/bin/python "$CGTYPE" "$js"
    sleep 0.1
    # 按 Enter 执行
    osascript -e 'tell application "System Events" to keystroke return'
    sleep 1
}

# ── 预定义命令快捷方式 ──

case "${1:-}" in
    --ping)
        invoke 'await window.__TAURI_INTERNALS__.invoke("ping", {message: "hello"})'
        ;;
    --list-sessions)
        invoke 'await window.__TAURI_INTERNALS__.invoke("list_sessions")'
        ;;
    --list-projects)
        invoke 'await window.__TAURI_INTERNALS__.invoke("list_projects")'
        ;;
    --create-project)
        local path="${2:-}"
        [[ -z "$path" ]] && { echo "Usage: $0 --create-project <path>"; exit 1; }
        invoke "await window.__TAURI_INTERNALS__.invoke('add_project', {rootPath: '$path'})"
        ;;
    --create-session)
        local name="${2:-Test Session}"
        invoke "await window.__TAURI_INTERNALS__.invoke('create_session', {name: '$name'})"
        ;;
    --delete-session)
        local sid="${2:-}"
        [[ -z "$sid" ]] && { echo "Usage: $0 --delete-session <id>"; exit 1; }
        invoke "await window.__TAURI_INTERNALS__.invoke('delete_session', {id: '$sid'})"
        ;;
    --dashboard)
        invoke 'await window.__TAURI_INTERNALS__.invoke("get_dashboard_stats")'
        ;;
    --toggle-sidepanel)
        invoke 'await window.__TAURI_INTERNALS__.invoke("toggle_sidepanel")'
        ;;
    --raw)
        shift
        invoke "$*"
        ;;
    *)
        echo "Usage: $0 <command> [args]"
        echo ""
        echo "Commands:"
        echo "  --ping                    Test ping/pong"
        echo "  --list-sessions           列出所有会话"
        echo "  --list-projects           列出所有项目"
        echo "  --create-project <path>   添加项目"
        echo "  --create-session [name]   创建会话"
        echo "  --delete-session <id>     删除会话"
        echo "  --dashboard               Dashboard 状态"
        echo "  --toggle-sidepanel        切换右侧面板"
        echo "  --raw <js>                执行任意 JavaScript"
        ;;
esac
