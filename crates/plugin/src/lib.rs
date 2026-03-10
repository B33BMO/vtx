pub mod hooks;
pub mod lua_plugin;
pub mod wasm_plugin;

use std::collections::HashMap;
use std::path::Path;

use tracing::info;

use hooks::{HookContext, HookEvent};
use lua_plugin::{LuaPlugin, PluginAction};
use wasm_plugin::WasmPlugin;

/// Which runtime a loaded plugin uses.
enum PluginRuntime {
    Lua(LuaPlugin),
    Wasm(WasmPlugin),
}

impl PluginRuntime {
    #[allow(dead_code)]
    fn name(&self) -> &str {
        match self {
            Self::Lua(p) => &p.name,
            Self::Wasm(p) => &p.name,
        }
    }
}

/// Manages all loaded plugins (Lua and WASM).
pub struct PluginManager {
    plugins: HashMap<String, PluginRuntime>,
}

impl PluginManager {
    /// Create a new, empty plugin manager.
    pub fn new() -> Self {
        Self {
            plugins: HashMap::new(),
        }
    }

    /// Load a Lua plugin from a file path. The plugin name is derived from the filename.
    pub fn load_lua_plugin(&mut self, path: &Path) -> Result<String, String> {
        let plugin = LuaPlugin::load(path)?;
        let name = plugin.name.clone();
        info!(name = %name, "loaded Lua plugin");
        self.plugins
            .insert(name.clone(), PluginRuntime::Lua(plugin));
        Ok(name)
    }

    /// Load a WASM plugin from a `.wasm` file. The plugin name is derived from the filename.
    pub fn load_wasm_plugin(&mut self, path: &Path) -> Result<String, String> {
        let plugin = WasmPlugin::load(path)?;
        let name = plugin.name.clone();
        info!(name = %name, "loaded WASM plugin");
        self.plugins
            .insert(name.clone(), PluginRuntime::Wasm(plugin));
        Ok(name)
    }

    /// Unload a plugin by name.
    pub fn unload(&mut self, name: &str) -> bool {
        let removed = self.plugins.remove(name).is_some();
        if removed {
            info!(name = %name, "unloaded plugin");
        }
        removed
    }

    /// Dispatch a hook event to all loaded plugins.
    /// Returns all actions requested by all plugins.
    pub fn dispatch_hook(&mut self, event: HookEvent, ctx: &HookContext) -> Vec<PluginAction> {
        let mut all_actions = Vec::new();

        for plugin in self.plugins.values_mut() {
            let actions = match plugin {
                PluginRuntime::Lua(p) => p.dispatch_hook(event, ctx),
                PluginRuntime::Wasm(p) => p.dispatch_hook(event, ctx),
            };
            all_actions.extend(actions);
        }

        all_actions
    }

    /// Invoke a custom command by name. Searches all plugins for a matching command.
    /// Returns actions from the first plugin that handles the command.
    pub fn invoke_command(&self, command: &str, args: &[String]) -> Vec<PluginAction> {
        for plugin in self.plugins.values() {
            if let PluginRuntime::Lua(p) = plugin {
                if p.commands.contains(command) {
                    return p.invoke_command(command, args);
                }
            }
            // WASM commands go through the on_hook(Command) path instead.
        }
        Vec::new()
    }

    /// List loaded plugin names.
    pub fn list_plugins(&self) -> Vec<&str> {
        self.plugins.keys().map(|s| s.as_str()).collect()
    }

    /// Check if a plugin with the given name is loaded.
    pub fn is_loaded(&self, name: &str) -> bool {
        self.plugins.contains_key(name)
    }
}

impl Default for PluginManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_plugin_manager_lua() {
        let mut mgr = PluginManager::new();

        // Load a Lua plugin from string (via LuaPlugin directly, then insert).
        let plugin = LuaPlugin::load_from_str(
            "test_plugin",
            r#"
            vtx.register_hook("on_pane_create", function(ctx)
                vtx.notify("pane created: " .. ctx.pane_id)
            end)

            vtx.register_command("greet", function(args)
                vtx.notify("hi " .. (args[1] or "world"))
            end)
        "#,
        )
        .unwrap();

        mgr.plugins
            .insert("test_plugin".into(), PluginRuntime::Lua(plugin));

        assert!(mgr.is_loaded("test_plugin"));
        assert_eq!(mgr.list_plugins(), vec!["test_plugin"]);

        // Dispatch a hook.
        let ctx = HookContext {
            pane_id: Some(7),
            ..Default::default()
        };
        let actions = mgr.dispatch_hook(HookEvent::PaneCreate, &ctx);
        assert_eq!(actions.len(), 1);
        match &actions[0] {
            PluginAction::Notify { message } => assert_eq!(message, "pane created: 7"),
            other => panic!("expected Notify, got {other:?}"),
        }

        // Invoke a command.
        let actions = mgr.invoke_command("greet", &["alice".into()]);
        assert_eq!(actions.len(), 1);
        match &actions[0] {
            PluginAction::Notify { message } => assert_eq!(message, "hi alice"),
            other => panic!("expected Notify, got {other:?}"),
        }

        // Unload.
        assert!(mgr.unload("test_plugin"));
        assert!(!mgr.is_loaded("test_plugin"));
    }

    #[test]
    fn test_dispatch_no_plugins() {
        let mut mgr = PluginManager::new();
        let actions = mgr.dispatch_hook(
            HookEvent::PaneCreate,
            &HookContext::default(),
        );
        assert!(actions.is_empty());
    }
}
