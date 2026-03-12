-- Rose Pine theme for vtx
vtx.prefix = "ctrl-a"
vtx.shell = "/bin/zsh"
vtx.scrollback = 100000

vtx.status_left = {
    { text = " \u{25b6} #{session} ",  fg = "#191724", bg = "#c4a7e7", bold = true },
    { text = " #{windows} ",           fg = "#e0def4", bg = "#26233a" },
    { text = " #{git} ",               fg = "#191724", bg = "#9ccfd8" },
}

vtx.status_right = {
    { text = " #{cwd} ",   fg = "#e0def4", bg = "#26233a" },
    { text = " #{cpu} ",   fg = "#e0def4", bg = "#1f1d2e" },
    { text = " #{mem} ",   fg = "#e0def4", bg = "#26233a" },
    { text = " #{time} ",  fg = "#191724", bg = "#c4a7e7", bold = true },
}

vtx.status_bg = "#191724"
