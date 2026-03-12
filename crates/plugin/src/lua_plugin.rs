use std::collections::HashSet;
use std::path::Path;
use std::sync::{Arc, Mutex};

use mlua::prelude::*;
use tracing::{error, warn};

use crate::hooks::{HookContext, HookEvent};

/// Actions that a Lua plugin has requested via the `vtx` API.
/// These are queued and drained by the plugin manager after each call.
#[derive(Debug, Clone)]
pub enum PluginAction {
    SendKeys { pane_id: u32, data: Vec<u8> },
    Split { horizontal: bool },
    Notify { message: String },
    NewWindow { name: Option<String> },
    RunCommand { command: String },
    SetLayout { preset: String },
    RenameWindow { name: String },
    ZoomPane,
    SelectWindow { index: usize },
    KillPane,
    Popup { command: Option<String> },
}

/// A single loaded Lua plugin.
pub struct LuaPlugin {
    pub name: String,
    lua: Lua,
    /// Which hook events this plugin has registered for.
    pub hooks: HashSet<HookEvent>,
    /// Custom command names registered by this plugin.
    /// The callbacks themselves are stored in Lua's `__vtx_cmds` table.
    pub commands: HashSet<String>,
    /// Pending actions requested by the plugin during the last call.
    actions: Arc<Mutex<Vec<PluginAction>>>,
}

impl LuaPlugin {
    /// Load a Lua plugin from a file path.
    pub fn load(path: &Path) -> Result<Self, String> {
        let name = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown")
            .to_string();

        let source = std::fs::read_to_string(path)
            .map_err(|e| format!("failed to read {}: {e}", path.display()))?;

        Self::load_from_str(&name, &source)
    }

    /// Load a Lua plugin from a source string (useful for testing).
    pub fn load_from_str(name: &str, source: &str) -> Result<Self, String> {
        let lua = Lua::new();
        let hooks: Arc<Mutex<HashSet<HookEvent>>> = Arc::new(Mutex::new(HashSet::new()));
        let commands: Arc<Mutex<HashSet<String>>> = Arc::new(Mutex::new(HashSet::new()));
        let actions: Arc<Mutex<Vec<PluginAction>>> = Arc::new(Mutex::new(Vec::new()));

        // Build the `vtx` API table.
        let result = (|| -> LuaResult<()> {
            let vtx = lua.create_table()?;

            // vtx.register_hook(event_name, callback)
            let hooks_ref = Arc::clone(&hooks);
            let register_hook = lua.create_function(move |lua, (event_name, callback): (String, LuaFunction)| {
                let event = HookEvent::from_name(&event_name).ok_or_else(|| {
                    LuaError::runtime(format!("unknown hook event: {event_name}"))
                })?;
                // Store callback in registry keyed by event name
                let key = lua.create_registry_value(callback)?;
                // We store the key in a Lua app data slot keyed by event name.
                // For simplicity, store in a global table __vtx_hooks.
                let hook_table: LuaTable = match lua.globals().get::<LuaTable>("__vtx_hooks") {
                    Ok(t) => t,
                    Err(_) => {
                        let t = lua.create_table()?;
                        lua.globals().set("__vtx_hooks", t.clone())?;
                        t
                    }
                };
                // Each event can have multiple callbacks; store as array.
                let arr: LuaTable = match hook_table.get::<LuaTable>(event.as_str()) {
                    Ok(a) => a,
                    Err(_) => {
                        let a = lua.create_table()?;
                        hook_table.set(event.as_str(), a.clone())?;
                        a
                    }
                };
                let idx = arr.raw_len() + 1;
                let func_from_key: LuaFunction = lua.registry_value(&key)?;
                arr.set(idx, func_from_key)?;
                // Also remove the standalone registry key since it's now in the table
                lua.remove_registry_value(key)?;

                hooks_ref.lock().unwrap().insert(event);
                Ok(())
            })?;
            vtx.set("register_hook", register_hook)?;

            // vtx.register_command(name, callback)
            // Callbacks are stored in a Lua-side table __vtx_cmds[name] = func.
            let cmds_ref = Arc::clone(&commands);
            let register_command = lua.create_function(move |lua, (cmd_name, callback): (String, LuaFunction)| {
                let cmd_table: LuaTable = match lua.globals().get::<LuaTable>("__vtx_cmds") {
                    Ok(t) => t,
                    Err(_) => {
                        let t = lua.create_table()?;
                        lua.globals().set("__vtx_cmds", t.clone())?;
                        t
                    }
                };
                cmd_table.set(cmd_name.as_str(), callback)?;
                cmds_ref.lock().unwrap().insert(cmd_name);
                Ok(())
            })?;
            vtx.set("register_command", register_command)?;

            // vtx.send_keys(pane_id, data)
            let actions_ref = Arc::clone(&actions);
            let send_keys = lua.create_function(move |_, (pane_id, data): (u32, String)| {
                actions_ref.lock().unwrap().push(PluginAction::SendKeys {
                    pane_id,
                    data: data.into_bytes(),
                });
                Ok(())
            })?;
            vtx.set("send_keys", send_keys)?;

            // vtx.split(horizontal)
            let actions_ref = Arc::clone(&actions);
            let split = lua.create_function(move |_, horizontal: bool| {
                actions_ref
                    .lock()
                    .unwrap()
                    .push(PluginAction::Split { horizontal });
                Ok(())
            })?;
            vtx.set("split", split)?;

            // vtx.notify(message)
            let actions_ref = Arc::clone(&actions);
            let notify = lua.create_function(move |_, message: String| {
                actions_ref
                    .lock()
                    .unwrap()
                    .push(PluginAction::Notify { message });
                Ok(())
            })?;
            vtx.set("notify", notify)?;

            // vtx.new_window(name) — name is optional (nil = default)
            let actions_ref = Arc::clone(&actions);
            let new_window = lua.create_function(move |_, name: Option<String>| {
                actions_ref
                    .lock()
                    .unwrap()
                    .push(PluginAction::NewWindow { name });
                Ok(())
            })?;
            vtx.set("new_window", new_window)?;

            // vtx.run(command) — split and run a command
            let actions_ref = Arc::clone(&actions);
            let run_command = lua.create_function(move |_, command: String| {
                actions_ref
                    .lock()
                    .unwrap()
                    .push(PluginAction::RunCommand { command });
                Ok(())
            })?;
            vtx.set("run", run_command)?;

            // vtx.set_layout(preset) — apply a layout preset by name
            let actions_ref = Arc::clone(&actions);
            let set_layout = lua.create_function(move |_, preset: String| {
                actions_ref
                    .lock()
                    .unwrap()
                    .push(PluginAction::SetLayout { preset });
                Ok(())
            })?;
            vtx.set("set_layout", set_layout)?;

            // vtx.rename_window(name)
            let actions_ref = Arc::clone(&actions);
            let rename_window = lua.create_function(move |_, name: String| {
                actions_ref
                    .lock()
                    .unwrap()
                    .push(PluginAction::RenameWindow { name });
                Ok(())
            })?;
            vtx.set("rename_window", rename_window)?;

            // vtx.zoom() — toggle zoom on focused pane
            let actions_ref = Arc::clone(&actions);
            let zoom = lua.create_function(move |_, ()| {
                actions_ref
                    .lock()
                    .unwrap()
                    .push(PluginAction::ZoomPane);
                Ok(())
            })?;
            vtx.set("zoom", zoom)?;

            // vtx.select_window(index) — 0-based index
            let actions_ref = Arc::clone(&actions);
            let select_window = lua.create_function(move |_, index: usize| {
                actions_ref
                    .lock()
                    .unwrap()
                    .push(PluginAction::SelectWindow { index });
                Ok(())
            })?;
            vtx.set("select_window", select_window)?;

            // vtx.kill_pane()
            let actions_ref = Arc::clone(&actions);
            let kill_pane = lua.create_function(move |_, ()| {
                actions_ref
                    .lock()
                    .unwrap()
                    .push(PluginAction::KillPane);
                Ok(())
            })?;
            vtx.set("kill_pane", kill_pane)?;

            // vtx.popup(command) — command is optional
            let actions_ref = Arc::clone(&actions);
            let popup = lua.create_function(move |_, command: Option<String>| {
                actions_ref
                    .lock()
                    .unwrap()
                    .push(PluginAction::Popup { command });
                Ok(())
            })?;
            vtx.set("popup", popup)?;

            // vtx.get_panes() — returns empty list for now (wired up when integrated with server)
            let get_panes = lua.create_function(|lua, ()| {
                lua.create_table()
            })?;
            vtx.set("get_panes", get_panes)?;

            // vtx.get_focused_pane() — returns nil for now
            let get_focused_pane = lua.create_function(|_, ()| -> LuaResult<LuaValue> {
                Ok(LuaValue::Nil)
            })?;
            vtx.set("get_focused_pane", get_focused_pane)?;

            lua.globals().set("vtx", vtx)?;

            // Execute the plugin source.
            lua.load(source).exec()?;

            Ok(())
        })();

        if let Err(e) = result {
            return Err(format!("failed to load Lua plugin '{name}': {e}"));
        }

        let hooks_final = {
            let guard = hooks.lock().unwrap();
            guard.clone()
        };
        let commands_final = {
            let guard = commands.lock().unwrap();
            guard.clone()
        };

        Ok(Self {
            name: name.to_string(),
            lua,
            hooks: hooks_final,
            commands: commands_final,
            actions,
        })
    }

    /// Dispatch a hook event to this plugin. Returns any actions the plugin requested.
    pub fn dispatch_hook(&self, event: HookEvent, ctx: &HookContext) -> Vec<PluginAction> {
        if !self.hooks.contains(&event) {
            return Vec::new();
        }

        // Clear pending actions.
        self.actions.lock().unwrap().clear();

        let result: LuaResult<()> = (|| {
            let hook_table: LuaTable = self.lua.globals().get("__vtx_hooks")?;
            let arr: LuaTable = hook_table.get(event.as_str())?;

            let ctx_table = self.context_to_table(ctx)?;

            for pair in arr.pairs::<i64, LuaFunction>() {
                let (_, func) = pair?;
                if let Err(e) = func.call::<()>(ctx_table.clone()) {
                    error!(plugin = %self.name, event = event.as_str(), "hook error: {e}");
                }
            }
            Ok(())
        })();

        if let Err(e) = result {
            warn!(plugin = %self.name, event = event.as_str(), "dispatch error: {e}");
        }

        self.drain_actions()
    }

    /// Invoke a custom command registered by this plugin.
    pub fn invoke_command(&self, cmd_name: &str, args: &[String]) -> Vec<PluginAction> {
        if !self.commands.contains(cmd_name) {
            return Vec::new();
        }

        self.actions.lock().unwrap().clear();

        let result: LuaResult<()> = (|| {
            let cmd_table: LuaTable = self.lua.globals().get("__vtx_cmds")?;
            let func: LuaFunction = cmd_table.get(cmd_name)?;
            let lua_args = self.lua.create_table()?;
            for (i, arg) in args.iter().enumerate() {
                lua_args.set(i as i64 + 1, arg.as_str())?;
            }
            func.call::<()>(lua_args)?;
            Ok(())
        })();

        if let Err(e) = result {
            error!(plugin = %self.name, command = cmd_name, "command error: {e}");
        }

        self.drain_actions()
    }

    /// Convert a HookContext into a Lua table.
    fn context_to_table(&self, ctx: &HookContext) -> LuaResult<LuaTable> {
        let table = self.lua.create_table()?;
        if let Some(pane_id) = ctx.pane_id {
            table.set("pane_id", pane_id)?;
        }
        if let Some(session_id) = ctx.session_id {
            table.set("session_id", session_id)?;
        }
        if let Some(ref key) = ctx.key {
            table.set("key", key.as_str())?;
        }
        if let Some(ref command) = ctx.command {
            table.set("command", command.as_str())?;
        }
        if let Some(ref args) = ctx.args {
            let args_table = self.lua.create_table()?;
            for (i, arg) in args.iter().enumerate() {
                args_table.set(i as i64 + 1, arg.as_str())?;
            }
            table.set("args", args_table)?;
        }
        Ok(table)
    }

    /// Drain and return all pending plugin actions.
    fn drain_actions(&self) -> Vec<PluginAction> {
        let mut actions = self.actions.lock().unwrap();
        actions.drain(..).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_load_simple_plugin() {
        let source = r#"
            vtx.register_hook("on_pane_create", function(ctx)
                vtx.notify("New pane: " .. ctx.pane_id)
            end)
        "#;
        let plugin = LuaPlugin::load_from_str("test", source).unwrap();
        assert_eq!(plugin.name, "test");
        assert!(plugin.hooks.contains(&HookEvent::PaneCreate));
    }

    #[test]
    fn test_dispatch_hook() {
        let source = r#"
            vtx.register_hook("on_pane_create", function(ctx)
                vtx.notify("New pane: " .. ctx.pane_id)
            end)
        "#;
        let plugin = LuaPlugin::load_from_str("test", source).unwrap();
        let ctx = HookContext {
            pane_id: Some(42),
            ..Default::default()
        };
        let actions = plugin.dispatch_hook(HookEvent::PaneCreate, &ctx);
        assert_eq!(actions.len(), 1);
        match &actions[0] {
            PluginAction::Notify { message } => assert_eq!(message, "New pane: 42"),
            _ => panic!("expected Notify action"),
        }
    }

    #[test]
    fn test_register_command() {
        let source = r#"
            vtx.register_command("hello", function(args)
                vtx.notify("Hello! " .. table.concat(args, " "))
            end)
        "#;
        let plugin = LuaPlugin::load_from_str("test", source).unwrap();
        assert!(plugin.commands.contains("hello"));

        let actions = plugin.invoke_command("hello", &["world".into(), "foo".into()]);
        assert_eq!(actions.len(), 1);
        match &actions[0] {
            PluginAction::Notify { message } => assert_eq!(message, "Hello! world foo"),
            _ => panic!("expected Notify action"),
        }
    }

    #[test]
    fn test_unknown_hook_event_is_error() {
        let source = r#"
            vtx.register_hook("on_nonexistent", function(ctx) end)
        "#;
        let result = LuaPlugin::load_from_str("test", source);
        assert!(result.is_err());
    }

    #[test]
    fn test_vtx_run_command() {
        let source = r#"
            vtx.register_hook("on_pane_create", function(ctx)
                vtx.run("htop")
            end)
        "#;
        let plugin = LuaPlugin::load_from_str("test", source).unwrap();
        let ctx = HookContext {
            pane_id: Some(1),
            ..Default::default()
        };
        let actions = plugin.dispatch_hook(HookEvent::PaneCreate, &ctx);
        assert_eq!(actions.len(), 1);
        match &actions[0] {
            PluginAction::RunCommand { command } => assert_eq!(command, "htop"),
            other => panic!("expected RunCommand, got {:?}", other),
        }
    }

    #[test]
    fn test_vtx_new_window() {
        // Test with a name
        let source = r#"
            vtx.register_hook("on_pane_create", function(ctx)
                vtx.new_window("my-window")
            end)
        "#;
        let plugin = LuaPlugin::load_from_str("test_named", source).unwrap();
        let ctx = HookContext {
            pane_id: Some(1),
            ..Default::default()
        };
        let actions = plugin.dispatch_hook(HookEvent::PaneCreate, &ctx);
        assert_eq!(actions.len(), 1);
        match &actions[0] {
            PluginAction::NewWindow { name } => assert_eq!(name.as_deref(), Some("my-window")),
            other => panic!("expected NewWindow, got {:?}", other),
        }

        // Test without a name (nil)
        let source_nil = r#"
            vtx.register_hook("on_pane_create", function(ctx)
                vtx.new_window()
            end)
        "#;
        let plugin2 = LuaPlugin::load_from_str("test_nil", source_nil).unwrap();
        let actions2 = plugin2.dispatch_hook(HookEvent::PaneCreate, &ctx);
        assert_eq!(actions2.len(), 1);
        match &actions2[0] {
            PluginAction::NewWindow { name } => assert_eq!(*name, None),
            other => panic!("expected NewWindow with None, got {:?}", other),
        }
    }
}
