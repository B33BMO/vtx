use mlua::prelude::*;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// A parsed color value from a hex string like "#282828".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Color(pub u8, pub u8, pub u8);

impl Color {
    /// Parse a hex color string like "#282828" or "282828".
    pub fn from_hex(s: &str) -> Option<Color> {
        let s = s.strip_prefix('#').unwrap_or(s);
        if s.len() != 6 {
            return None;
        }
        let r = u8::from_str_radix(&s[0..2], 16).ok()?;
        let g = u8::from_str_radix(&s[2..4], 16).ok()?;
        let b = u8::from_str_radix(&s[4..6], 16).ok()?;
        Some(Color(r, g, b))
    }

    /// Return the color as a (u8, u8, u8) tuple.
    pub fn as_tuple(self) -> (u8, u8, u8) {
        (self.0, self.1, self.2)
    }
}

/// A single keybinding parsed from Lua config.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyBinding {
    pub modifier: String,
    pub key: String,
    pub action: String,
}

/// Configuration values parsed from a Lua config file.
#[derive(Debug, Clone)]
pub struct LuaConfig {
    pub prefix: String,
    pub shell: String,
    pub scrollback: usize,
    pub status_bg: Color,
    pub status_fg: Color,
    pub bindings: Vec<KeyBinding>,
}

impl Default for LuaConfig {
    fn default() -> Self {
        Self {
            prefix: "ctrl-b".into(),
            shell: std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into()),
            scrollback: 10_000,
            status_bg: Color(0x28, 0x28, 0x28),
            status_fg: Color(0xb4, 0xd2, 0xff),
            bindings: Vec::new(),
        }
    }
}

/// Determine the path to the Lua config file.
fn config_path() -> PathBuf {
    let config_dir = std::env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_else(|_| "/root".into());
            PathBuf::from(home).join(".config")
        });
    config_dir.join("vtx").join("config.lua")
}

/// Load the Lua configuration, returning defaults if the file doesn't exist
/// or if any error occurs during parsing.
pub fn load() -> LuaConfig {
    let path = config_path();
    if !path.exists() {
        return LuaConfig::default();
    }

    let source = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(_) => return LuaConfig::default(),
    };

    match eval_lua(&source) {
        Ok(cfg) => cfg,
        Err(e) => {
            eprintln!("vtx: error loading {}: {e}", path.display());
            LuaConfig::default()
        }
    }
}

/// Load from an explicit Lua source string (useful for testing).
pub fn load_from_str(source: &str) -> Result<LuaConfig, String> {
    eval_lua(source).map_err(|e| e.to_string())
}

/// Evaluate a Lua source string and extract the config values.
fn eval_lua(source: &str) -> LuaResult<LuaConfig> {
    let lua = Lua::new();
    let cfg = Arc::new(Mutex::new(LuaConfig::default()));

    // Build the `vtx` table that the Lua script will interact with.
    let vtx = lua.create_table()?;

    // vtx.bind(modifier, key, action)
    let bindings_ref = Arc::clone(&cfg);
    let bind_fn = lua.create_function(move |_lua, (modifier, key, action): (String, String, String)| {
        let mut c = bindings_ref.lock().unwrap();
        c.bindings.push(KeyBinding { modifier, key, action });
        Ok(())
    })?;
    vtx.set("bind", bind_fn)?;

    // Use a metatable so that `vtx.prefix = "ctrl-a"` etc. are captured.
    let cfg_for_newindex = Arc::clone(&cfg);
    let meta = lua.create_table()?;
    let newindex = lua.create_function(
        move |_lua, (_table, key, value): (LuaTable, String, LuaValue)| {
            let mut c = cfg_for_newindex.lock().unwrap();
            match key.as_str() {
                "prefix" => {
                    if let LuaValue::String(s) = value {
                        c.prefix = s.to_string_lossy().to_string();
                    }
                }
                "shell" => {
                    if let LuaValue::String(s) = value {
                        c.shell = s.to_string_lossy().to_string();
                    }
                }
                "scrollback" => {
                    if let LuaValue::Integer(n) = value {
                        if n > 0 {
                            c.scrollback = n as usize;
                        }
                    }
                }
                "status_bg" => {
                    if let LuaValue::String(s) = value {
                        let s = s.to_string_lossy();
                        if let Some(color) = Color::from_hex(&s) {
                            c.status_bg = color;
                        }
                    }
                }
                "status_fg" => {
                    if let LuaValue::String(s) = value {
                        let s = s.to_string_lossy();
                        if let Some(color) = Color::from_hex(&s) {
                            c.status_fg = color;
                        }
                    }
                }
                _ => {
                    // Unknown keys are silently ignored.
                }
            }
            Ok(())
        },
    )?;
    meta.set("__newindex", newindex)?;

    // __index so that `vtx.bind` still works even though we override __newindex.
    // Store the raw table as a backing store for reads.
    let raw_vtx = vtx.clone();
    let index_fn = lua.create_function(move |_lua, (_table, key): (LuaTable, String)| {
        let val: LuaValue = raw_vtx.raw_get(key)?;
        Ok(val)
    })?;
    meta.set("__index", index_fn)?;

    vtx.set_metatable(Some(meta));

    lua.globals().set("vtx", vtx)?;

    lua.load(source).exec()?;

    let result = cfg.lock().unwrap().clone();
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_color_from_hex() {
        assert_eq!(Color::from_hex("#282828"), Some(Color(0x28, 0x28, 0x28)));
        assert_eq!(Color::from_hex("b4d2ff"), Some(Color(0xb4, 0xd2, 0xff)));
        assert_eq!(Color::from_hex("#ff0000"), Some(Color(255, 0, 0)));
        assert_eq!(Color::from_hex("nope"), None);
        assert_eq!(Color::from_hex("#gg0000"), None);
    }

    #[test]
    fn test_defaults() {
        let cfg = LuaConfig::default();
        assert_eq!(cfg.prefix, "ctrl-b");
        assert_eq!(cfg.scrollback, 10_000);
        assert!(cfg.bindings.is_empty());
    }

    #[test]
    fn test_load_from_str_basic() {
        let lua_src = r###"
            vtx.prefix = "ctrl-a"
            vtx.shell = "/bin/zsh"
            vtx.scrollback = 100000
            vtx.status_bg = "#282828"
            vtx.status_fg = "#b4d2ff"
        "###;
        let cfg = load_from_str(lua_src).unwrap();
        assert_eq!(cfg.prefix, "ctrl-a");
        assert_eq!(cfg.shell, "/bin/zsh");
        assert_eq!(cfg.scrollback, 100_000);
        assert_eq!(cfg.status_bg, Color(0x28, 0x28, 0x28));
        assert_eq!(cfg.status_fg, Color(0xb4, 0xd2, 0xff));
    }

    #[test]
    fn test_load_from_str_bindings() {
        let lua_src = r#"
            vtx.bind("prefix", "|", "split-horizontal")
            vtx.bind("alt", "h", "focus-left")
        "#;
        let cfg = load_from_str(lua_src).unwrap();
        assert_eq!(cfg.bindings.len(), 2);
        assert_eq!(cfg.bindings[0], KeyBinding {
            modifier: "prefix".into(),
            key: "|".into(),
            action: "split-horizontal".into(),
        });
        assert_eq!(cfg.bindings[1], KeyBinding {
            modifier: "alt".into(),
            key: "h".into(),
            action: "focus-left".into(),
        });
    }

    #[test]
    fn test_load_from_str_invalid_returns_error() {
        let result = load_from_str("this is not valid lua {{{");
        assert!(result.is_err());
    }

    #[test]
    fn test_load_from_str_empty() {
        let cfg = load_from_str("").unwrap();
        assert_eq!(cfg.prefix, "ctrl-b");
    }
}
