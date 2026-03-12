-- IDE Layout Plugin
-- Auto-splits into a main editor + terminal layout on session create
-- Usage: Copy to ~/.config/vtx/plugins/ide.lua

vtx.register_hook("on_session_create", function(ctx)
    -- Split horizontally for a side panel
    vtx.split(true)
    -- Set main-vertical layout (65% main, stacked right)
    vtx.set_layout("main-vertical")
    vtx.notify("[ide] Dev layout ready")
end)
