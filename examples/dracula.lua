-- Dracula theme for vtx
vtx.prefix = "ctrl-a"
vtx.shell = "/bin/zsh"
vtx.scrollback = 100000

vtx.status_left = {
    { text = " \u{25b6} #{session} ",  fg = "#282a36", bg = "#bd93f9", bold = true },
    { text = " #{windows} ",           fg = "#f8f8f2", bg = "#44475a" },
    { text = " #{git} ",               fg = "#282a36", bg = "#50fa7b" },
}

vtx.status_right = {
    { text = " #{cwd} ",   fg = "#f8f8f2", bg = "#44475a" },
    { text = " #{cpu} ",   fg = "#f8f8f2", bg = "#383a59" },
    { text = " #{mem} ",   fg = "#f8f8f2", bg = "#44475a" },
    { text = " #{time} ",  fg = "#282a36", bg = "#bd93f9", bold = true },
}

vtx.status_bg = "#282a36"
