use serde::{Deserialize, Serialize};

use crate::lua_config;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Prefix key chord, e.g. "C-b"
    pub prefix: String,
    /// Default shell to spawn
    pub default_shell: String,
    /// Socket path for IPC
    pub socket_path: String,
    /// Scrollback buffer size in lines
    pub scrollback: usize,
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
        }
    }
}

fn dirs_socket_path() -> String {
    let runtime_dir = std::env::var("XDG_RUNTIME_DIR")
        .unwrap_or_else(|_| format!("/tmp/vtx-{}", unsafe { libc::getuid() }));
    format!("{runtime_dir}/vtx.sock")
}
