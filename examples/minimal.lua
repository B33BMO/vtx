-- Minimal theme — clean, no powerline glyphs needed
vtx.prefix = "ctrl-a"
vtx.shell = "/bin/zsh"
vtx.scrollback = 50000

vtx.status_left = {
    { text = " #{session} | #{windows} ", fg = "#c0c0c0", bg = "#1c1c1c" },
}

vtx.status_right = {
    { text = " #{time} ", fg = "#c0c0c0", bg = "#1c1c1c" },
}

vtx.status_bg = "#1c1c1c"
