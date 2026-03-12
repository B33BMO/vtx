-- Catppuccin Mocha theme for vtx
vtx.prefix = "ctrl-space"
vtx.shell = "/bin/zsh"
vtx.scrollback = 100000

vtx.status_left = {
    { text = " \u{25b6} #{session} ",  fg = "#1e1e2e", bg = "#cba6f7", bold = true },
    { text = " #{windows} ",           fg = "#cdd6f4", bg = "#45475a" },
    { text = " #{git} ",               fg = "#1e1e2e", bg = "#a6e3a1" },
}

vtx.status_right = {
    { text = " #{cwd} ",   fg = "#cdd6f4", bg = "#45475a" },
    { text = " #{cpu} ",   fg = "#cdd6f4", bg = "#313244" },
    { text = " #{mem} ",   fg = "#cdd6f4", bg = "#45475a" },
    { text = " #{time} ",  fg = "#1e1e2e", bg = "#cba6f7", bold = true },
}

vtx.status_bg = "#1e1e2e"
