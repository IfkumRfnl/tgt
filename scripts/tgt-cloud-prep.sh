#!/usr/bin/env bash
# Prepare this Cloud Agent VM to demo tgt: restore the baked Telegram
# session, ensure a single process, and hide the xfce4-terminal menubar
# so Alt+T reaches the TUI instead of the Terminal menu.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SESSION_SRC="${HOME}/.tgt-session/tg"
DEBUG_TG="${ROOT}/.tgt-dev/data/.data/tg"
XDG_TG="${HOME}/.local/share/tgt/.data/tg"

if [[ ! -d "$SESSION_SRC" ]]; then
  echo "error: baked session missing at $SESSION_SRC" >&2
  exit 1
fi

if pgrep -x tgt >/dev/null 2>&1; then
  echo "stopping leftover tgt: $(pgrep -x tgt | tr '\n' ' ')"
  # Kill by PID only (never pkill -f).
  kill $(pgrep -x tgt) || true
  sleep 1
  if pgrep -x tgt >/dev/null 2>&1; then
    echo "error: tgt still running: $(pgrep -x tgt | tr '\n' ' ')" >&2
    exit 1
  fi
fi

mkdir -p "$(dirname "$DEBUG_TG")" "$(dirname "$XDG_TG")"
rm -rf "$DEBUG_TG" "$XDG_TG"
cp -r "$SESSION_SRC" "$DEBUG_TG"
cp -r "$SESSION_SRC" "$XDG_TG"
echo "restored session -> $DEBUG_TG"
echo "restored session -> $XDG_TG"

if command -v xfconf-query >/dev/null 2>&1; then
  xfconf-query -c xfce4-terminal -p /misc-menubar-default -s false
  echo "xfce4-terminal menubar default: off"
fi

if [[ ! -x "${ROOT}/target/debug/tgt" ]]; then
  echo "note: ${ROOT}/target/debug/tgt is missing; run: cargo build"
else
  echo "binary: ${ROOT}/target/debug/tgt"
fi

echo
echo "Next (GUI demo):"
echo "  1. Maximize xfce4-terminal, cd ${ROOT}"
echo "  2. Run:  ./target/debug/tgt     # do NOT redirect stderr"
echo "  3. Click Chat List, click a chat twice, Esc, Alt+T, q"
echo "See AGENTS.md → Cursor Cloud specific instructions."
