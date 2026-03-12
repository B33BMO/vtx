-- Status Plugin
-- Shows notifications on key events
-- Usage: Copy to ~/.config/vtx/plugins/status.lua

vtx.register_hook("on_pane_create", function(ctx)
    vtx.notify("[status] Pane " .. ctx.pane_id .. " created")
end)

vtx.register_hook("on_pane_close", function(ctx)
    vtx.notify("[status] Pane " .. ctx.pane_id .. " closed")
end)

vtx.register_hook("on_session_create", function(ctx)
    vtx.notify("[status] Session " .. ctx.session_id .. " created")
end)
