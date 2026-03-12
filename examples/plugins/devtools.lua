-- DevTools Plugin
-- Provides commands for common development workflows
-- Usage: Copy to ~/.config/vtx/plugins/devtools.lua

-- Command: open a monitoring dashboard
vtx.register_command("dashboard", function(args)
    vtx.new_window("dashboard")
    vtx.run("htop")
    vtx.split(true)
    vtx.run("watch -n1 df -h")
    vtx.set_layout("main-vertical")
    vtx.notify("[devtools] Dashboard opened")
end)

-- Command: open a git workflow window
vtx.register_command("git-view", function(args)
    vtx.new_window("git")
    vtx.run("git log --oneline --graph --all -20")
    vtx.split(false)
    vtx.run("git status")
    vtx.set_layout("main-horizontal")
    vtx.notify("[devtools] Git view opened")
end)

-- Command: open a quick terminal popup
vtx.register_command("scratch", function(args)
    vtx.popup(nil)
    vtx.notify("[devtools] Scratch popup opened")
end)

-- Auto-name windows based on session
vtx.register_hook("on_window_create", function(ctx)
    vtx.notify("[devtools] New window created")
end)
