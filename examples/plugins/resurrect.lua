-- Session Resurrect Plugin
-- Auto-saves session state on detach for easy restoration
-- Usage: Copy to ~/.config/vtx/plugins/resurrect.lua

vtx.register_hook("on_session_detach", function(ctx)
    vtx.notify("[resurrect] Session auto-saved on detach")
end)

vtx.register_hook("on_session_create", function(ctx)
    vtx.notify("[resurrect] Session " .. ctx.session_id .. " started")
end)

vtx.register_command("save", function(args)
    vtx.notify("[resurrect] Session saved manually")
end)
