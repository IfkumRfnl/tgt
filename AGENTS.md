# Agent notes for tgt

`tgt` is a Rust Telegram TUI. Debug builds keep config/data/state under `/workspace/.tgt-dev/` so they never touch `~/.tgt`.

## Cursor Cloud specific instructions

This Cloud Agent environment ships a **pre-authenticated TDLib session**. Do not type a phone number, OTP, or send messages unless the user explicitly asks.

### Layout

| What | Path |
| --- | --- |
| Baked session (source of truth) | `$HOME/.tgt-session/tg/` (`td.binlog` + `db.sqlite`) |
| Debug data dir (what `./target/debug/tgt` reads) | `/workspace/.tgt-dev/data/.data/tg/` |
| Release/XDG data dir | `$HOME/.local/share/tgt/.data/tg/` |
| Debug config | `/workspace/.tgt-dev/config/` |
| Debug TDLib log | `/workspace/.tgt-dev/state/.data/tdlib_rs/tdlib_rs.log` |
| Binary (after `cargo build`) | `/workspace/target/debug/tgt` |
| Desktop terminal | `xfce4-terminal` (1920×1200 XFCE, Plank dock) |
| Rust | 1.91.0 at `CARGO_HOME=/usr/local/cargo` (not `~/.cargo`) |

Restore the session before every TUI run (install already copies it; re-copy if the workspace was reset):

```bash
./scripts/tgt-cloud-prep.sh
```

Or by hand:

```bash
mkdir -p "$HOME/.local/share/tgt/.data" /workspace/.tgt-dev/data/.data
rm -rf "$HOME/.local/share/tgt/.data/tg" /workspace/.tgt-dev/data/.data/tg
cp -r "$HOME/.tgt-session/tg" "$HOME/.local/share/tgt/.data/tg"
cp -r "$HOME/.tgt-session/tg" /workspace/.tgt-dev/data/.data/tg
```

A valid session reaches `authorizationStateWaitTdlibParameters` then Ready with `DcId{2} … [state:OK]` in the TDLib log. A phone-number prompt means the session is missing or expired — restore from `$HOME/.tgt-session` and retry. Do **not** invent `TGT_TDLIB_SESSION_B64`; the session is too large for env vars.

### Build and automated tests

```bash
cd /workspace
cargo build                          # ~2 min cold; default features (static TDLib + voice)
./target/debug/tgt --version         # tgt 1.0.0
./target/debug/tgt init-config       # writes /workspace/.tgt-dev/config
cargo test -- --test-threads=1       # 163 passed, 2 ignored
```

Do not use `make test` / `make build` unless you pass features; Makefile defaults to `--no-default-features`.

### Hard rules for running the TUI

1. **The TUI is drawn on stderr**, not stdout (`CrosstermBackend<Stderr>` in `src/tui_backend.rs`). Never launch with `2>file` or `2>/dev/null`. That is the usual cause of a “blank terminal.” Logs already go to files.
2. **One process only.** A second instance fails with `Can't lock file '…/tg/td.binlog'`. Kill the leftover by **PID** (`pgrep -a tgt`, then `kill <pid>`). Do not `pkill -f`.
3. **CWD must be `/workspace`** for debug path resolution (`.tgt-dev` is relative to cwd).
4. **Do not send messages** in demos unless asked. Read-only: list, open, scroll, popups, quit.

Optional, for a cleaner TUI (TDLib trace off the screen): in `/workspace/.tgt-dev/config/telegram.toml` set `redirect_stderr = true`. That file is gitignored.

### How to interact (the UI will ignore you if you skip this)

On launch **nothing is focused**. Chat-list keys (`Down`/`Enter`) do nothing useful until Chat List is focused. `confirm_selection` bails out unless `focused_component == ChatList`, so Enter on an unfocused list leaves `Open chat:` empty and the right pane black.

**Mouse (preferred in computer-use):**

1. Click the **left** “Chat List” pane once → focuses it (border highlight).
2. Click the same chat row again → opens it. Status bar must show `Open chat: <name>`.
3. Wait ~5–12s for history. Right pane should fill with messages.

**Keyboard:**

| Key | Effect | Notes |
| --- | --- | --- |
| `Alt+1` or `Alt+Left` | Focus chat list | Required before `Down`/`Enter` |
| `Down` / `Up` | Move selection | Only while chat list is focused |
| `Enter` or `Right` | Open selected chat | Then focus jumps to the **Prompt** |
| `Esc` | Unfocus | Press after opening a chat so later keys are not typed as message text |
| `Alt+2` | Focus chat (messages) | Then `Up`/`Down` scroll messages |
| `Alt+3` or `Alt+Down` | Focus prompt | Do not type here in a read-only demo |
| `Alt+T` | Theme selector | Hide the xfce4-terminal menubar first (see below) |
| `Alt+F1` | Command guide | **Broken on this desktop** — XFCE/Terminal steal F1 / Alt+F1 |
| `q` or `Ctrl+C` | Quit | |

After open, tgt focuses the prompt (`FocusComponent(Prompt)`). If you then press `t`/`T` you will see `tT` in the prompt. Press `Esc` before `Alt+T`.

### Desktop / video capture

The GUI terminal is **xfce4-terminal**, not gnome-terminal. Plank dock has a Terminal icon.

**xfce4-terminal steals keys** while its menubar is visible:

- `Alt+T` opens the **Terminal** menu (and can insert `t` into the prompt).
- `F1` asks to open the Xfce Terminal manual.
- `Alt+F1` opens the XFCE applications menu.

Hide the menubar **before** recording (the prep script does this):

```bash
xfconf-query -c xfce4-terminal -p /misc-menubar-default -s false
```

In an already-open window: right-click → uncheck **Show Menubar**, or View → Show Menubar.

**Demo video recipe (computer-use + RecordScreen):**

1. Run `./scripts/tgt-cloud-prep.sh` (session restore, menubar off, no leftover tgt).
2. Open maximized xfce4-terminal, `cd /workspace`, type `./target/debug/tgt` — **do not Enter yet**.
3. `RecordScreen` START.
4. Enter. Wait ~12s for the chat list (title `Tgt - A TUI for Telegram`).
5. Click `Idle Bank Alpha` (or another chat with a text preview) twice. Wait until the right pane has messages and the status bar shows `Open chat: …`.
6. `Esc`, then `Alt+T` (theme selector). `Esc`, then `q`.
7. `RecordScreen` SAVE. Review the video before treating it as the artifact.

If a screenshot of the terminal is blank but `tmux capture-pane` shows the TUI, you redirected stderr. Fix the launch command; do not assume alternate-screen/VNC is broken.

### Headless TUI check (tmux, no video)

Use this to prove auth/chat list before a GUI demo. Wait ~15–20s; first paint can lag TDLib connect (port 443 often fails, then DC2 `:5222`).

```bash
SESSION_NAME=tgt-tui
tmux -f /exec-daemon/tmux.portal.conf kill-session -t "$SESSION_NAME" 2>/dev/null || true
tmux -f /exec-daemon/tmux.portal.conf new-session -d -s "$SESSION_NAME" -x 180 -y 45 -c /workspace -- bash -l
tmux -f /exec-daemon/tmux.portal.conf send-keys -t "$SESSION_NAME:0.0" 'export TERM=xterm-256color; ./target/debug/tgt' C-m
sleep 18
tmux -f /exec-daemon/tmux.portal.conf capture-pane -t "$SESSION_NAME:0.0" -p
```

`send-keys Down` / `M-1` are **unreliable** in tgt raw mode (you get `Key pressed: Unknown` or `^[[B` in the prompt). Use computer-use key/mouse events for real interaction. Quit with `q`, then `kill <pid>` if needed.

### Network / TDLib quirks

- MTProto `:443` often logs `Expected packet size is too big`; TDLib falls back to **DC2 port 5222** and still works.
- Rapid reconnects can hit `FLOOD_WAIT_30`. Wait 30s before the next launch. Chat list still loads from the local DB.
- ALSA `cannot find card '0'` is harmless (no sound device).
- IPv6 to Telegram is unreachable here; ignore those errors.

### Lessons (blockers → fix)

| Blocker | What it looked like | Fix |
| --- | --- | --- |
| stderr redirected to a log | Maximized terminal stays blank; ANSI of the TUI is in the log file | Run `./target/debug/tgt` with stderr attached to the tty |
| Chat list not focused | `Down`/`Enter` appear in the status bar but `Open chat:` stays empty and the right pane is black | Click the left pane, or `Alt+1`, then Enter/second click |
| Open chat focuses Prompt | `Alt+T` types `tT` into the composer | `Esc` first |
| xfce4-terminal menubar | `Alt+T` / `F1` / `Alt+F1` open XFCE UI, not tgt | Hide menubar before the demo; use `Alt+T` for a popup, not `Alt+F1` |
| Two tgt processes | `Can't lock file '…/td.binlog'` | Kill by PID |
| Session not copied after checkout | Phone-number login screen | Copy `$HOME/.tgt-session/tg` into `.tgt-dev/data/.data/tg` |
| `TERM=dumb` in the Shell tool | TUI/raw mode misbehaves in that shell | Demo in xfce4-terminal or tmux with `TERM=xterm-256color` |
| `FLOOD_WAIT_30` | Slow/missing live updates | Wait 30s; local chat list is enough for a demo |
