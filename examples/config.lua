-- vtx example configuration
-- Copy to ~/.config/vtx/config.lua

-- Use Ctrl-A as prefix (like screen)
vtx.prefix = "ctrl-a"

-- Default shell
vtx.shell = "/bin/zsh"

-- Scrollback buffer (50k lines)
vtx.scrollback = 50000

-- ── Status Bar ─────────────────────────────────────────────
-- Each segment has: text, fg, bg, bold
-- Template variables: #{session}, #{windows}, #{git}, #{cpu}, #{mem}, #{time}, #{pane}, #{cwd}

vtx.status_left = {
    { text = " \u{25b6} #{session} ",  fg = "#1a1b26", bg = "#7aa2f7", bold = true },
    { text = " #{windows} ",           fg = "#c0caf5", bg = "#414868" },
    { text = " #{git} ",               fg = "#1a1b26", bg = "#9ece6a" },
}

vtx.status_right = {
    { text = " #{cpu} ",   fg = "#c0caf5", bg = "#414868" },
    { text = " #{mem} ",   fg = "#c0caf5", bg = "#3b4261" },
    { text = " #{time} ",  fg = "#1a1b26", bg = "#7aa2f7", bold = true },
}

vtx.status_bg = "#1a1b26"

-- ── Keybindings ────────────────────────────────────────────
-- Vim-style splits
vtx.bind("prefix", "|", "split-horizontal")
vtx.bind("prefix", "-", "split-vertical")
vtx.bind("prefix", "x", "kill-pane")

-- Seamless pane navigation (no prefix needed)
vtx.bind("alt", "h", "focus-left")
vtx.bind("alt", "j", "focus-down")
vtx.bind("alt", "k", "focus-up")
vtx.bind("alt", "l", "focus-right")

-- Resize with Alt+Shift
vtx.bind("alt", "H", "resize-left")
vtx.bind("alt", "J", "resize-down")
vtx.bind("alt", "K", "resize-up")
vtx.bind("alt", "L", "resize-right")

-- Quick window management
vtx.bind("alt", "c", "new-window")
vtx.bind("alt", "n", "next-window")
vtx.bind("alt", "p", "prev-window")

-- Layouts
vtx.bind("prefix", "m", "layout-main-v")
vtx.bind("prefix", "t", "layout-tiled")
vtx.bind("prefix", "e", "layout-even-h")

-- Features
vtx.bind("prefix", "f", "zoom")
vtx.bind("prefix", "p", "popup")
vtx.bind("prefix", "/", "search")
vtx.bind("prefix", "[", "copy-mode")
vtx.bind("prefix", "r", "source-config")
