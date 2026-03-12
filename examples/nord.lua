-- Nord theme for vtx
vtx.prefix = "ctrl-a"
vtx.shell = "/bin/zsh"
vtx.scrollback = 100000

vtx.status_left = {
    { text = " \u{25b6} #{session} ",  fg = "#2e3440", bg = "#88c0d0", bold = true },
    { text = " #{windows} ",           fg = "#d8dee9", bg = "#3b4252" },
    { text = " #{git} ",               fg = "#2e3440", bg = "#a3be8c" },
}

vtx.status_right = {
    { text = " #{cpu} ",   fg = "#d8dee9", bg = "#3b4252" },
    { text = " #{mem} ",   fg = "#d8dee9", bg = "#434c5e" },
    { text = " #{time} ",  fg = "#2e3440", bg = "#88c0d0", bold = true },
}

vtx.status_bg = "#2e3440"
