-- Gruvbox Dark theme for vtx
vtx.prefix = "ctrl-a"
vtx.shell = "/bin/zsh"
vtx.scrollback = 100000

vtx.status_left = {
    { text = " \u{25b6} #{session} ",  fg = "#282828", bg = "#fabd2f", bold = true },
    { text = " #{windows} ",           fg = "#ebdbb2", bg = "#504945" },
    { text = " #{git} ",               fg = "#282828", bg = "#b8bb26" },
}

vtx.status_right = {
    { text = " #{cpu} ",   fg = "#ebdbb2", bg = "#504945" },
    { text = " #{mem} ",   fg = "#ebdbb2", bg = "#3c3836" },
    { text = " #{time} ",  fg = "#282828", bg = "#fabd2f", bold = true },
}

vtx.status_bg = "#282828"
