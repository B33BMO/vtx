use serde::{Deserialize, Serialize};

use crate::lua_config::{self, KeyBinding};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Prefix key chord, e.g. "ctrl-a" or "ctrl-b"
    pub prefix: String,
    /// Default shell to spawn
    pub default_shell: String,
    /// Socket path for IPC
    pub socket_path: String,
    /// Scrollback buffer size in lines
    pub scrollback: usize,
    /// Status bar background color (r, g, b)
    pub status_bg: (u8, u8, u8),
    /// Status bar foreground color (r, g, b)
    pub status_fg: (u8, u8, u8),
    /// User-defined keybindings from Lua config
    pub bindings: Vec<KeyBinding>,
    /// Structured status bar configuration
    pub status_bar: lua_config::StatusBarConfig,
}

impl Default for Config {
    fn default() -> Self {
        let socket_path = dirs_socket_path();
        let lua_cfg = lua_config::load();

        Config {
            prefix: lua_cfg.prefix,
            default_shell: lua_cfg.shell,
            socket_path,
            scrollback: lua_cfg.scrollback,
            status_bg: lua_cfg.status_bg.as_tuple(),
            status_fg: lua_cfg.status_fg.as_tuple(),
            bindings: lua_cfg.bindings,
            status_bar: lua_cfg.status_bar,
        }
    }
}

impl Config {
    /// Reload config values from a Lua config, preserving the socket path.
    pub fn reload_from_lua(&mut self, lua_cfg: lua_config::LuaConfig) {
        self.prefix = lua_cfg.prefix;
        self.default_shell = lua_cfg.shell;
        self.scrollback = lua_cfg.scrollback;
        self.status_bg = lua_cfg.status_bg.as_tuple();
        self.status_fg = lua_cfg.status_fg.as_tuple();
        self.bindings = lua_cfg.bindings;
        self.status_bar = lua_cfg.status_bar;
    }
}

fn dirs_socket_path() -> String {
    let runtime_dir = std::env::var("XDG_RUNTIME_DIR")
        .unwrap_or_else(|_| format!("/tmp/vtx-{}", unsafe { libc::getuid() }));
    format!("{runtime_dir}/vtx.sock")
}
