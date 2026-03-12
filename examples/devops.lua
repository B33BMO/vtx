-- DevOps / SRE focused config — heavy on system monitoring
vtx.prefix = "ctrl-a"
vtx.shell = "/bin/zsh"
vtx.scrollback = 200000

vtx.status_left = {
    { text = " #{session} ",    fg = "#1a1b26", bg = "#7aa2f7", bold = true },
    { text = " #{windows} ",    fg = "#c0caf5", bg = "#414868" },
    { text = " #{git} ",        fg = "#1a1b26", bg = "#9ece6a" },
}

vtx.status_right = {
    { text = " #{cwd} ",        fg = "#a9b1d6", bg = "#24283b" },
    { text = " cpu #{cpu} ",    fg = "#f7768e", bg = "#414868", bold = true },
    { text = " mem #{mem} ",    fg = "#e0af68", bg = "#3b4261", bold = true },
    { text = " #{time} ",       fg = "#1a1b26", bg = "#7aa2f7", bold = true },
}

vtx.status_bg = "#1a1b26"

-- Quick layout bindings for monitoring dashboards
vtx.bind("prefix", "m", "layout-main-v")
vtx.bind("prefix", "t", "layout-tiled")
vtx.bind("prefix", "e", "layout-even-h")
