<p align="center">
  <h1 align="center">vtx</h1>
  <p align="center">A modern terminal multiplexer built in Rust.<br>Drop-in tmux alternative with GPU rendering, Lua scripting, clickable status bars, and a plugin system.</p>
</p>

<p align="center">
  <a href="#installation">Installation</a> &bull;
  <a href="#quick-start">Quick Start</a> &bull;
  <a href="#configuration">Configuration</a> &bull;
  <a href="#keybindings">Keybindings</a> &bull;
  <a href="#status-bar">Status Bar</a> &bull;
  <a href="#plugins">Plugins</a> &bull;
  <a href="#architecture">Architecture</a>
</p>

---

## Why vtx?

tmux is powerful but showing its age. vtx takes the session/window/pane model you already know and rebuilds it with:

- **Lua-native config** instead of a bespoke DSL. Your config is a real programming language.
- **Composable, clickable status bars** with per-segment colors, live template variables, and mouse interaction. Click tabs to switch windows. Click [+] to create one.
- **GPU-accelerated rendering** via wgpu/winit as an optional backend.
- **A plugin system** (Lua + WASM) with hooks for session, window, and pane lifecycle events.
- **First-class mouse support** everywhere: click to focus, drag to select, drag dividers to resize, right-click context menus.
- **Hot-reloadable everything**. Change your theme, keybindings, or status bar layout and apply it instantly without restarting.

If you're comfortable with tmux, you'll feel at home. If you've been frustrated by tmux's limitations, vtx gives you the escape hatch.

---

## Features

| Category | Features |
|----------|----------|
| **Pane Management** | Horizontal/vertical splits, resize by keyboard or mouse-drag dividers, swap panes, zoom (fullscreen toggle), kill & respawn |
| **Windows & Sessions** | Multiple windows (tabs) per session, named sessions, detach/reattach, clickable tab bar |
| **Status Bar** | Fully composable Lua-driven segments with per-segment fg/bg/bold, powerline separators, template variables (git, cpu, mem, time, cwd), clickable regions |
| **Configuration** | Lua config (`~/.config/vtx/config.lua`), hot-reload (prefix+R or `vtx source`), tmux.conf import, 8 built-in themes |
| **Rendering** | Double-buffered diff-only TTY renderer, optional GPU renderer (wgpu + winit), truecolor, unicode |
| **Copy & Search** | Vim-style copy mode (hjkl, w/b, g/G, Ctrl-U/D, visual select, yank via OSC 52), incremental scrollback search |
| **Plugins** | Lua and WASM plugins, auto-loaded from `~/.config/vtx/plugins/`, hook system for lifecycle events, custom commands |
| **Session Persistence** | Auto-save on detach, manual save/restore (`vtx resurrect`), preserves window layout and pane working directories |
| **SSH Panes** | `vtx ssh user@host` opens SSH directly in a pane |
| **Widgets** | Built-in system monitoring panes: cpu, mem, disk, net, sysinfo |
| **Floating Popups** | Overlay panes with box-drawing borders for quick tasks |
| **Layout Presets** | Even-horizontal, even-vertical, main-vertical, main-horizontal, tiled (prefix+Space to cycle) |
| **Mouse** | Click to focus, drag to select/copy, scroll wheel, drag dividers, right-click context menu, clickable status bar |

---

## Installation

### Requirements

- Rust 1.85+ (edition 2024)
- Linux (uses `/proc` for system stats, PTY management via `nix`)

### From source

```bash
git clone https://github.com/bmo/vtx.git
cd vtx
cargo build --release
cp target/release/vtx ~/.local/bin/
```

With GPU renderer support:

```bash
cargo build --release --features gpu
```

### Install script

```bash
chmod +x install.sh
./install.sh
```

The install script:
- Checks for Rust 1.85+
- Builds the release binary (set `ENABLE_GPU=1` for GPU support)
- Installs to `~/.local/bin/` (override with `INSTALL_DIR`)
- Creates `~/.config/vtx/plugins/`
- Writes an example config if none exists
- Warns if the install directory isn't in your `PATH`

```bash
# Examples
ENABLE_GPU=1 ./install.sh              # Build with GPU renderer
INSTALL_DIR=$HOME/bin ./install.sh      # Custom install path
```

---

## Quick Start

```bash
# Start a new session (auto-starts server in background)
vtx

# Start a named session
vtx new --name work

# List active sessions
vtx list

# Attach to a session by name
vtx attach work

# Detach from inside a session
# Press Ctrl-a d

# Kill a session
vtx kill session work

# Kill the server (all sessions)
vtx kill server
```

### GPU Mode

```bash
# Attach with the GPU-accelerated renderer (requires --features gpu build)
vtx --gpu
vtx --gpu new --name dev
```

---

## CLI Reference

```
vtx [OPTIONS] [COMMAND]
```

### Global Options

| Flag | Description |
|------|-------------|
| `--gpu` | Use GPU-accelerated renderer (requires `gpu` feature) |

### Commands

| Command | Description |
|---------|-------------|
| `vtx` / `vtx new` | Create a new session and attach |
| `vtx new --name <name>` | Create a named session |
| `vtx attach <target>` | Attach to an existing session (name or ID) |
| `vtx list` / `vtx ls` | List active sessions |
| `vtx server` | Start the server daemon (usually auto-started) |
| `vtx ssh <destination>` | Open an SSH pane (`[user@]host`, optional `--port`) |
| `vtx widget <kind>` | Open a system widget (`cpu`, `mem`, `disk`, `net`, `sysinfo`) |
| `vtx source` | Reload config from default path |
| `vtx source --file <path>` | Reload config from a specific Lua file |
| `vtx kill session <name>` | Kill a named session |
| `vtx kill server` | Shut down the server and all sessions |
| `vtx resurrect save` | Save current session state to disk |
| `vtx resurrect restore <name>` | Restore a previously saved session |
| `vtx resurrect list` | List all saved sessions |

---

## Keybindings

The default prefix key is **Ctrl-a** (like GNU screen). Press the prefix, release, then press the action key.

### Prefix Keys (Ctrl-a + key)

| Key | Action |
|-----|--------|
| `"` | Split vertical |
| `%` | Split horizontal |
| `\|` | Split horizontal (vim-style default) |
| `-` | Split vertical (vim-style default) |
| `x` | Kill pane |
| `d` | Detach from session |
| `z` | Toggle zoom (fullscreen) |
| `[` | Enter copy mode |
| `/` | Search scrollback |
| `c` | New window (tab) |
| `n` | Next window |
| `p` | Previous window |
| `Space` | Cycle layout preset |
| `P` | Toggle floating popup |
| `R` | Reload config (hot-reload) |
| `m` | Main-vertical layout |
| `t` | Tiled layout |
| `e` | Even-horizontal layout |

### Direct Keys (no prefix needed)

| Key | Action |
|-----|--------|
| `Alt+h` / `Alt+j` / `Alt+k` / `Alt+l` | Focus pane left / down / up / right |
| `Alt+Shift+H` / `J` / `K` / `L` | Resize pane left / down / up / right |
| `Alt+1` through `Alt+9` | Switch to window by number |

### Copy Mode (prefix + [)

Enter copy mode to navigate scrollback with vim-like keys:

| Key | Action |
|-----|--------|
| `h` / `j` / `k` / `l` | Move cursor left / down / up / right |
| `0` / `$` | Start / end of line |
| `w` / `b` | Forward / backward word |
| `g` / `G` | Top / bottom of scrollback |
| `Ctrl-U` / `Ctrl-D` | Page up / page down |
| `v` | Begin visual selection |
| `y` | Yank selection to system clipboard (OSC 52) |
| `q` / `Esc` | Exit copy mode |

### Mouse

| Action | Effect |
|--------|--------|
| Left click on pane | Focus that pane |
| Left click on status bar tab | Switch to that window |
| Left click on `[+]` | Create a new window |
| Left drag | Select text (auto-copies to clipboard on release via OSC 52) |
| Right click | Context menu (split, swap, kill, respawn, zoom) |
| Scroll wheel | Scroll through scrollback buffer (auto-snaps back on new output) |
| Drag a divider | Resize panes by moving the border |

---

## Configuration

vtx is configured with Lua at `~/.config/vtx/config.lua`. The config is a real Lua script with access to the `vtx` table.

### Minimal Config

```lua
vtx.prefix = "ctrl-a"
vtx.shell = "/bin/zsh"
vtx.scrollback = 50000
```

### Full Example

```lua
-- ~/.config/vtx/config.lua

-- ── Core Settings ─────────────────────────────────────────
vtx.prefix = "ctrl-a"           -- Prefix key (ctrl-a, ctrl-b, ctrl-space, etc.)
vtx.shell = "/bin/zsh"          -- Default shell for new panes
vtx.scrollback = 50000          -- Scrollback buffer (lines)

-- ── Status Bar ────────────────────────────────────────────
-- Each segment: { text, fg, bg, bold }
-- Template variables are resolved live every render cycle.

vtx.status_left = {
    { text = " ▶ #{session} ",  fg = "#1a1b26", bg = "#7aa2f7", bold = true },
    { text = " #{windows} ",    fg = "#c0caf5", bg = "#414868" },
    { text = " #{git} ",        fg = "#1a1b26", bg = "#9ece6a" },
}

vtx.status_right = {
    { text = " #{cpu} ",   fg = "#c0caf5", bg = "#414868" },
    { text = " #{mem} ",   fg = "#c0caf5", bg = "#3b4261" },
    { text = " #{time} ",  fg = "#1a1b26", bg = "#7aa2f7", bold = true },
}

vtx.status_bg = "#1a1b26"       -- Fill color between left and right sides

-- ── Keybindings ───────────────────────────────────────────
-- vtx.bind(modifier, key, action)
-- Modifiers: "prefix", "alt", "ctrl"

vtx.bind("prefix", "|", "split-horizontal")
vtx.bind("prefix", "-", "split-vertical")
vtx.bind("prefix", "x", "kill-pane")

vtx.bind("alt", "h", "focus-left")
vtx.bind("alt", "j", "focus-down")
vtx.bind("alt", "k", "focus-up")
vtx.bind("alt", "l", "focus-right")

vtx.bind("alt", "H", "resize-left")
vtx.bind("alt", "J", "resize-down")
vtx.bind("alt", "K", "resize-up")
vtx.bind("alt", "L", "resize-right")

vtx.bind("prefix", "m", "layout-main-v")
vtx.bind("prefix", "t", "layout-tiled")
vtx.bind("prefix", "e", "layout-even-h")

vtx.bind("prefix", "f", "zoom")
vtx.bind("prefix", "p", "popup")
vtx.bind("prefix", "/", "search")
vtx.bind("prefix", "[", "copy-mode")
vtx.bind("prefix", "r", "source-config")
```

### Configurable Settings

| Setting | Type | Default | Description |
|---------|------|---------|-------------|
| `vtx.prefix` | string | `"ctrl-a"` | Prefix key combo |
| `vtx.shell` | string | `"/bin/zsh"` | Default shell for new panes |
| `vtx.scrollback` | number | `50000` | Scrollback buffer size in lines |
| `vtx.status_bg` | hex string | `"#1a1b26"` | Status bar background fill color |
| `vtx.status_fg` | hex string | `"#7aa2f7"` | Status bar default foreground |
| `vtx.status_left` | table | Tokyo Night theme | Left status bar segments |
| `vtx.status_right` | table | Tokyo Night theme | Right status bar segments |

### Available Binding Actions

| Action | Description |
|--------|-------------|
| `split-horizontal` | Split pane horizontally (side by side) |
| `split-vertical` | Split pane vertically (top/bottom) |
| `kill-pane` | Kill the focused pane |
| `detach` | Detach from the session |
| `focus-left` / `focus-right` / `focus-up` / `focus-down` | Move focus to adjacent pane |
| `resize-left` / `resize-right` / `resize-up` / `resize-down` | Resize focused pane (5 cells) |
| `scroll-up` / `scroll-down` | Scroll scrollback buffer |
| `zoom` | Toggle pane zoom (fullscreen) |
| `copy-mode` | Enter vim-style copy mode |
| `search` | Enter incremental scrollback search |
| `new-window` | Create a new window (tab) |
| `next-window` / `prev-window` | Switch to next/previous window |
| `layout-cycle` | Cycle through layout presets |
| `layout-even-h` | Even-horizontal layout |
| `layout-even-v` | Even-vertical layout |
| `layout-main-v` | Main-vertical layout (65% left, stacked right) |
| `layout-main-h` | Main-horizontal layout (65% top, side-by-side bottom) |
| `layout-tiled` | Tiled grid layout |
| `popup` / `close-popup` | Open/close floating popup pane |
| `source-config` | Reload config from disk |

### Hot Reload

Change your config and apply it without restarting:

```bash
# From the CLI
vtx source
vtx source --file ~/themes/gruvbox.lua

# From inside vtx
# Press Ctrl-a R
```

Hot reload updates: prefix key, keybindings, status bar segments, and colors. Active sessions and panes are not affected.

### tmux.conf Compatibility

vtx understands common tmux configuration directives. You can import your existing setup:

```tmux
set -g prefix C-a
set -g default-shell /bin/zsh
set -g history-limit 50000
set -g mouse on
set -g base-index 1

bind | split-window -h
bind - split-window -v
bind -n M-h select-pane -L
bind -n M-l select-pane -R
bind -n M-j select-pane -D
bind -n M-k select-pane -U
unbind C-b
```

Supported directives: `set -g prefix`, `set -g default-shell`, `set -g history-limit`, `set -g mouse`, `set -g base-index`, `bind`, `bind -n`, `unbind`.

---

## Status Bar

The status bar is fully composable. Each side (left and right) is an array of segments, and each segment has its own text, foreground color, background color, and bold flag. Segments are joined with powerline arrow separators automatically.

### Template Variables

Variables are written as `#{name}` inside segment text and resolved live on every render:

| Variable | Description | Example Output |
|----------|-------------|----------------|
| `#{session}` | Current session name | `work` |
| `#{windows}` | Clickable window tab list (active marked with `*`) | `0:zsh* \| 1:vim \| 2:htop` |
| `#{git}` | Git branch, dirty flag, ahead/behind counts | ` main*↑2` |
| `#{cpu}` | CPU usage percentage | `cpu 23%` |
| `#{mem}` | Memory used / total | `3.2G/16G` |
| `#{time}` | Local time | `14:30` |
| `#{pane}` | Focused pane ID | `3` |
| `#{cwd}` | Working directory of focused pane | `~/projects/vtx` |

- `#{git}` is automatically hidden when the focused pane isn't inside a git repository.
- `#{windows}` is special: when used as the sole content of a segment (`" #{windows} "`), it expands into individual clickable tab segments per window. The active window is highlighted; inactive windows are dimmed. Clicking a tab switches to that window.
- A clickable **[+]** button is automatically appended after the left segments for creating new windows.

### System Stats

CPU and memory stats are read directly from `/proc/stat` and `/proc/meminfo` (no external dependencies). Stats are cached and refreshed every 2 seconds. Git information is gathered by shelling out to `git` from the focused pane's working directory.

### Segment Anatomy

```lua
{ text = " ▶ #{session} ", fg = "#1a1b26", bg = "#7aa2f7", bold = true }
--  ^                        ^               ^               ^
--  |                        |               |               |
--  Template text            Foreground      Background      Bold flag
--  (resolved live)          (hex color)     (hex color)     (optional)
```

Colors are specified as hex strings (`"#rrggbb"`). The renderer converts them to truecolor escape sequences.

---

## Themes

vtx ships with 8 ready-to-use themes in the `examples/` directory:

| Theme | File | Description |
|-------|------|-------------|
| **Tokyo Night** | (default) | Dark blue theme with vibrant accents |
| **Catppuccin Mocha** | `catppuccin.lua` | Warm pastel palette |
| **Gruvbox Dark** | `gruvbox.lua` | Retro earthy tones |
| **Dracula** | `dracula.lua` | Dark theme with purple accents |
| **Nord** | `nord.lua` | Arctic blue palette |
| **Rose Pine** | `rose-pine.lua` | Romantic dark palette |
| **DevOps** | `devops.lua` | SRE-focused with heavy system monitoring, 200k scrollback |
| **Minimal** | `minimal.lua` | Clean and simple, no powerline glyphs |

### Switching Themes

```bash
# Copy a theme to your config
cp examples/dracula.lua ~/.config/vtx/config.lua

# Hot-reload (no restart needed)
vtx source

# Or from inside vtx: Ctrl-a R
```

### Writing Your Own Theme

A theme is just a Lua config file. At minimum, define `status_left`, `status_right`, and `status_bg`:

```lua
vtx.prefix = "ctrl-a"
vtx.shell = "/bin/zsh"
vtx.scrollback = 50000

vtx.status_left = {
    { text = " #{session} ", fg = "#ffffff", bg = "#5f00af", bold = true },
    { text = " #{windows} ", fg = "#d0d0d0", bg = "#303030" },
}

vtx.status_right = {
    { text = " #{time} ", fg = "#ffffff", bg = "#5f00af", bold = true },
}

vtx.status_bg = "#1c1c1c"
```

---

## Session Resurrect

vtx can save and restore your session state, including window names, pane layout, and working directories.

```bash
# Save the current session
vtx resurrect save

# List saved sessions
vtx resurrect list

# Restore a session by name
vtx resurrect restore work
```

Sessions auto-save when you detach. Saved state is stored as JSON in `~/.config/vtx/sessions/`.

### What Gets Saved

- Session name
- Window names and order
- Active window index
- Pane layout tree (split directions and ratios)
- Each pane's working directory (read from `/proc/<pid>/cwd`)
- Focused pane per window

### What Gets Restored

- Windows are recreated with their original names
- Panes are spawned in their saved working directories
- Layout structure is rebuilt
- The previously active window is selected

---

## Plugins

Plugins extend vtx with custom behavior. They're auto-loaded from `~/.config/vtx/plugins/` on server start. Both Lua (`.lua`) and WASM (`.wasm`) plugins are supported.

### Example: IDE Layout Plugin

```lua
-- ~/.config/vtx/plugins/ide.lua

vtx.register_hook("on_session_create", function(ctx)
    vtx.split(true)                    -- horizontal split
    vtx.set_layout("main-vertical")    -- 65% main + stacked right
    vtx.notify("[ide] Dev layout ready")
end)
```

### Example: Custom Command Plugin

```lua
-- ~/.config/vtx/plugins/devtools.lua

vtx.register_command("dashboard", function(args)
    vtx.new_window("dashboard")
    vtx.run("htop")
    vtx.split(true)
    vtx.run("watch -n1 df -h")
    vtx.set_layout("main-vertical")
end)

vtx.register_command("git-view", function(args)
    vtx.new_window("git")
    vtx.run("git log --oneline --graph --all")
    vtx.split(false)
    vtx.run("git status")
end)
```

### Example: Auto-Save Plugin

```lua
-- ~/.config/vtx/plugins/resurrect.lua

vtx.register_hook("on_session_detach", function(ctx)
    vtx.notify("[resurrect] Session auto-saved")
end)
```

### Plugin API Reference

#### Action Functions

| Function | Description |
|----------|-------------|
| `vtx.split(horizontal)` | Split the focused pane. `true` = side-by-side, `false` = top/bottom |
| `vtx.new_window(name)` | Create a new window. Name is optional (`nil` for default) |
| `vtx.run(command)` | Split and run a shell command in the new pane |
| `vtx.set_layout(preset)` | Apply a layout preset: `"main-vertical"`, `"main-horizontal"`, `"tiled"`, `"even-h"`, `"even-v"` |
| `vtx.rename_window(name)` | Rename the active window |
| `vtx.zoom()` | Toggle zoom on the focused pane |
| `vtx.select_window(index)` | Switch to a window by index (0-based) |
| `vtx.kill_pane()` | Kill the focused pane |
| `vtx.popup(command)` | Open a floating popup pane. Command is optional |
| `vtx.send_keys(pane_id, data)` | Send raw keystrokes to a specific pane |
| `vtx.notify(message)` | Log a notification message |

#### Registration Functions

| Function | Description |
|----------|-------------|
| `vtx.register_hook(event, fn)` | Register a callback for a lifecycle event |
| `vtx.register_command(name, fn)` | Register a named command callable from other plugins |

#### Hook Events

| Event | Triggered When | Context Fields |
|-------|---------------|----------------|
| `on_session_create` | A new session is created | `session_id` |
| `on_session_close` | A session is destroyed | `session_id` |
| `on_session_detach` | A client detaches | `session_id` |
| `on_window_create` | A new window is created | `session_id` |
| `on_pane_create` | A new pane is spawned | `pane_id`, `session_id` |
| `on_pane_close` | A pane is killed | `pane_id`, `session_id` |
| `on_key` | A key is pressed | `key` |
| `on_pre_render` | Before a frame renders | `session_id` |
| `on_post_render` | After a frame renders | `session_id` |
| `on_command` | A custom command is invoked | `command`, `args` |

---

## Layout Engine

vtx uses a binary split tree for pane layout. Each split has a direction (horizontal or vertical) and a ratio (0.0-1.0) controlling how space is divided between children.

### Layout Presets

Cycle through presets with `prefix + Space` or apply directly:

| Preset | Description |
|--------|-------------|
| **Even Horizontal** | All panes side by side, equal width |
| **Even Vertical** | All panes stacked, equal height |
| **Main Vertical** | First pane gets ~65% width on the left, remaining panes stacked on the right |
| **Main Horizontal** | First pane gets ~65% height on top, remaining panes side by side below |
| **Tiled** | Grid layout based on `ceil(sqrt(n))` columns |

### Mouse-Driven Resize

Drag any border/divider between panes to resize. The layout tree's split ratios update in real time.

---

## SSH Panes

Open a pane with an SSH connection directly:

```bash
vtx ssh user@host
vtx ssh host --port 2222
```

The SSH pane behaves like any other pane: you can split it, resize it, zoom it, or include it in layout presets.

---

## System Widgets

Built-in monitoring panes that display live system information:

```bash
vtx widget cpu       # CPU usage graph
vtx widget mem       # Memory usage
vtx widget disk      # Disk usage
vtx widget net       # Network I/O
vtx widget sysinfo   # Combined system overview
```

Widgets run as regular panes and can be split, moved, or closed like any other pane.

---

## Architecture

vtx uses a client-server architecture over Unix domain sockets (`$XDG_RUNTIME_DIR/vtx.sock`).

```
                              Unix Socket
vtx (client) ◄──────────────────────────────────────► vtx-server
  │                                                       │
  ├── vtx-renderer-tty (double-buffered crossterm)        ├── Session management
  ├── vtx-renderer-gpu (wgpu + winit, optional)           ├── PTY orchestration
  ├── Input handling (keyboard, mouse)                    ├── Plugin dispatch
  └── Keybinding processing                               └── Status bar composition
```

The server runs as a background daemon and manages all sessions. Clients connect to render a session's output and send input. Multiple clients can connect simultaneously.

### Crates

| Crate | Description |
|-------|-------------|
| **vtx** (cli) | Binary entry point, CLI parsing with clap |
| **vtx-core** | Shared types (`PaneId`, `SessionId`), IPC message protocol, config, Lua/tmux config parsing |
| **vtx-server** | Session/window/pane lifecycle, PTY spawning, plugin hook dispatch, status bar template resolution |
| **vtx-client** | Terminal event capture, prefix key state machine, keybinding dispatch, server IPC |
| **vtx-terminal** | VT100/xterm escape sequence parser (via vte), terminal grid with 100k-line scrollback, alternate screen buffer |
| **vtx-renderer-tty** | Double-buffered differential TTY renderer via crossterm. Only writes cells that changed between frames. |
| **vtx-renderer-gpu** | GPU-accelerated renderer using wgpu + winit. Behind the `gpu` cargo feature flag. |
| **vtx-layout** | Binary split tree with ratio-based sizing. Resolves layout trees to screen-coordinate rects. Builds preset layouts. |
| **vtx-plugin** | Plugin manager, Lua runtime (mlua 0.10, Lua 5.4), WASM runtime (wasmtime). Handles hook dispatch and action collection. |

### IPC Protocol

Client and server communicate via newline-delimited JSON messages over the Unix socket. The protocol is defined in `vtx-core/src/ipc.rs`.

**Client -> Server messages** include: `NewSession`, `Attach`, `Input`, `Resize`, `Split`, `FocusDirection`, `KillPane`, `NewWindow`, `SelectWindow`, `ScrollBack`, `SearchScrollback`, `SaveSession`, `SourceConfig`, `KillServer`, and more.

**Server -> Client messages** include: `SessionReady`, `Render` (full frame: pane contents, borders, styled status bar), `Sessions` (list), `SearchResult`, `Error`, `Detached`, `ConfigReloaded`, `ServerShutdown`.

### Key Design Decisions

- **Cancel-safe IPC**: Tokio's `select!` can cancel a future mid-execution. Using `read_line` inside `select!` would corrupt the read buffer on cancellation. Both client and server use dedicated reader tasks that own the read half of the socket, forwarding complete messages through channels. The `select!` only receives from channels, which is always cancel-safe.

- **Double-buffered rendering**: The renderer maintains two cell buffers (front = what's on screen, back = what we want). Each frame, it fills the back buffer, diffs against front, and only emits escape sequences for changed cells. This minimizes terminal I/O. Sentinel cells (impossible values) in the initial front buffer guarantee a full draw on the first frame.

- **Dedicated PTY threads**: PTY reads are blocking syscalls. Rather than using async wrappers (which add complexity and overhead), each pane gets a dedicated `std::thread` that reads in a tight loop and sends data through an `mpsc` channel. The server's async event loop drains these channels with `try_recv()`.

- **Session hierarchy**: Session > Windows > Panes, matching tmux's model. This makes the mental model familiar to tmux users and enables features like window tabs, per-window layouts, and per-window zoom state.

- **Binary split tree layout**: Each window's pane layout is a binary tree where leaves are panes and internal nodes are splits with a direction and ratio. This naturally supports recursive splitting, and layout presets can be built by constructing the tree programmatically.

### Terminal Compatibility

The terminal parser handles:
- CSI sequences (cursor movement, erase, scroll regions, SGR attributes)
- OSC sequences (window title, OSC 52 clipboard)
- Full SGR: bold, dim, italic, underline, blink, reverse, hidden, strikethrough
- 256-color and truecolor (24-bit RGB)
- Alternate screen buffer (for vim, htop, etc.)
- Bracketed paste mode
- Application cursor key mode
- Scroll regions (DECSTBM)
- Cursor save/restore (DECSC/DECRC)
- Tab stops (8-column default)
- Compatible with oh-my-zsh, starship, and modern shell prompts

### Dependencies

| Dependency | Purpose |
|------------|---------|
| tokio | Async runtime (server, IPC) |
| crossterm | Terminal input/output, raw mode |
| vte | VT100/xterm escape sequence parsing |
| portable-pty | Cross-platform PTY creation |
| clap | CLI argument parsing |
| serde / serde_json | IPC message serialization |
| mlua | Lua 5.4 runtime for config and plugins |
| nix | Unix signal handling, PTY management |
| bitflags | Text attribute flags |
| unicode-width | Character width calculation |
| tracing | Structured logging |
| winit / wgpu | GPU renderer (optional) |

---

## Troubleshooting

### Server won't start
The server socket is at `$XDG_RUNTIME_DIR/vtx.sock` (usually `/run/user/<uid>/vtx.sock`). If a stale socket exists from a crashed server, remove it:
```bash
rm $XDG_RUNTIME_DIR/vtx.sock
```

### Powerline separators look wrong
vtx uses powerline glyphs (U+E0B0, U+E0B2) in the status bar. Install a Nerd Font or powerline-patched font and configure your terminal to use it. The **Minimal** theme (`examples/minimal.lua`) avoids powerline glyphs entirely.

### Colors look off
vtx uses truecolor (24-bit) escape sequences. Make sure your terminal supports truecolor:
```bash
echo $COLORTERM    # Should be "truecolor" or "24bit"
```

### Mouse not working
Mouse support requires a terminal that supports SGR mouse reporting. Most modern terminals (kitty, alacritty, wezterm, iTerm2, Windows Terminal) support this.

### GPU renderer issues
The GPU renderer requires a working Vulkan, Metal, or DX12 backend. Check that your system has GPU drivers installed. The GPU feature is behind a cargo feature flag and not built by default.

---

## License

MIT
