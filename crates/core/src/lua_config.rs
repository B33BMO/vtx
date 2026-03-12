use mlua::prelude::*;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// A parsed color value from a hex string like "#282828".
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct KeyBinding {
    pub modifier: String,
    pub key: String,
    pub action: String,
}

/// A status bar segment definition from Lua config.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SegmentDef {
    pub text: String,
    pub fg: Color,
    pub bg: Color,
    pub bold: bool,
}

/// User-configurable status bar layout.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct StatusBarConfig {
    pub left: Vec<SegmentDef>,
    pub right: Vec<SegmentDef>,
    pub bg: Color,
}

impl Default for StatusBarConfig {
    fn default() -> Self {
        Self {
            left: vec![
                SegmentDef {
                    text: " ▶ #{session} ".into(),
                    fg: Color(0x1a, 0x1b, 0x26),
                    bg: Color(0x7a, 0xa2, 0xf7),
                    bold: true,
                },
                SegmentDef {
                    text: " #{windows} ".into(),
                    fg: Color(0xc0, 0xca, 0xf5),
                    bg: Color(0x41, 0x48, 0x68),
                    bold: false,
                },
                SegmentDef {
                    text: " #{git} ".into(),
                    fg: Color(0x1a, 0x1b, 0x26),
                    bg: Color(0x9e, 0xce, 0x6a),
                    bold: false,
                },
            ],
            right: vec![
                SegmentDef {
                    text: " #{cpu} ".into(),
                    fg: Color(0xc0, 0xca, 0xf5),
                    bg: Color(0x41, 0x48, 0x68),
                    bold: false,
                },
                SegmentDef {
                    text: " #{mem} ".into(),
                    fg: Color(0xc0, 0xca, 0xf5),
                    bg: Color(0x3b, 0x42, 0x61),
                    bold: false,
                },
                SegmentDef {
                    text: " #{time} ".into(),
                    fg: Color(0x1a, 0x1b, 0x26),
                    bg: Color(0x7a, 0xa2, 0xf7),
                    bold: true,
                },
            ],
            bg: Color(0x1a, 0x1b, 0x26),
        }
    }
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
    pub status_bar: StatusBarConfig,
}

impl Default for LuaConfig {
    fn default() -> Self {
        Self {
            prefix: "ctrl-a".into(),
            shell: std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into()),
            scrollback: 50_000,
            status_bg: Color(0x1a, 0x1b, 0x26),  // Tokyo Night bg
            status_fg: Color(0x7a, 0xa2, 0xf7),  // Tokyo Night blue
            bindings: default_bindings(),
            status_bar: StatusBarConfig::default(),
        }
    }
}

/// Sensible default keybindings shipped out of the box.
fn default_bindings() -> Vec<KeyBinding> {
    vec![
        // Intuitive splits
        KeyBinding { modifier: "prefix".into(), key: "|".into(), action: "split-horizontal".into() },
        KeyBinding { modifier: "prefix".into(), key: "-".into(), action: "split-vertical".into() },
        // Vim-style pane navigation (no prefix needed)
        KeyBinding { modifier: "alt".into(), key: "h".into(), action: "focus-left".into() },
        KeyBinding { modifier: "alt".into(), key: "j".into(), action: "focus-down".into() },
        KeyBinding { modifier: "alt".into(), key: "k".into(), action: "focus-up".into() },
        KeyBinding { modifier: "alt".into(), key: "l".into(), action: "focus-right".into() },
        // Resize with Alt+Shift
        KeyBinding { modifier: "alt".into(), key: "H".into(), action: "resize-left".into() },
        KeyBinding { modifier: "alt".into(), key: "J".into(), action: "resize-down".into() },
        KeyBinding { modifier: "alt".into(), key: "K".into(), action: "resize-up".into() },
        KeyBinding { modifier: "alt".into(), key: "L".into(), action: "resize-right".into() },
        // Quick layout access
        KeyBinding { modifier: "prefix".into(), key: "m".into(), action: "layout-main-v".into() },
        KeyBinding { modifier: "prefix".into(), key: "t".into(), action: "layout-tiled".into() },
        KeyBinding { modifier: "prefix".into(), key: "e".into(), action: "layout-even-h".into() },
    ]
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

/// Load config from an explicit file path.
pub fn load_from_path(path: &std::path::Path) -> Result<LuaConfig, String> {
    if !path.exists() {
        return Err(format!("Config file not found: {}", path.display()));
    }
    let source = std::fs::read_to_string(path)
        .map_err(|e| format!("Failed to read {}: {e}", path.display()))?;
    eval_lua(&source).map_err(|e| format!("Error in {}: {e}", path.display()))
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
                "status_left" => {
                    if let LuaValue::Table(tbl) = value {
                        if let Ok(segments) = parse_segment_defs(&tbl) {
                            c.status_bar.left = segments;
                        }
                    }
                }
                "status_right" => {
                    if let LuaValue::Table(tbl) = value {
                        if let Ok(segments) = parse_segment_defs(&tbl) {
                            c.status_bar.right = segments;
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

fn parse_segment_defs(tbl: &LuaTable) -> LuaResult<Vec<SegmentDef>> {
    let mut defs = Vec::new();
    let len = tbl.raw_len();
    for i in 1..=len {
        let entry: LuaTable = tbl.raw_get(i)?;
        let text: String = entry.get::<String>("text").unwrap_or_default();
        if text.is_empty() {
            continue;
        }
        let fg = entry
            .get::<String>("fg")
            .ok()
            .and_then(|s| Color::from_hex(&s))
            .unwrap_or(Color(0xc0, 0xca, 0xf5));
        let bg = entry
            .get::<String>("bg")
            .ok()
            .and_then(|s| Color::from_hex(&s))
            .unwrap_or(Color(0x1a, 0x1b, 0x26));
        let bold: bool = entry.get::<bool>("bold").unwrap_or(false);
        defs.push(SegmentDef { text, fg, bg, bold });
    }
    Ok(defs)
}

/// A built-in theme definition with full status bar colors.
#[derive(Debug, Clone)]
pub struct Theme {
    pub name: &'static str,
    pub status_bg: Color,
    pub status_fg: Color,
    pub bar: StatusBarConfig,
}

/// All built-in themes.
pub fn builtin_themes() -> Vec<Theme> {
    vec![
        Theme {
            name: "Tokyo Night",
            status_bg: Color(0x1a, 0x1b, 0x26),
            status_fg: Color(0x7a, 0xa2, 0xf7),
            bar: StatusBarConfig::default(),
        },
        Theme {
            name: "Catppuccin",
            status_bg: Color(0x1e, 0x1e, 0x2e),
            status_fg: Color(0x89, 0xb4, 0xfa),
            bar: StatusBarConfig {
                left: vec![
                    SegmentDef { text: " ▶ #{session} ".into(), fg: Color(0x1e, 0x1e, 0x2e), bg: Color(0x89, 0xb4, 0xfa), bold: true },
                    SegmentDef { text: " #{windows} ".into(), fg: Color(0xcd, 0xd6, 0xf4), bg: Color(0x45, 0x47, 0x5a), bold: false },
                    SegmentDef { text: " #{git} ".into(), fg: Color(0x1e, 0x1e, 0x2e), bg: Color(0xa6, 0xe3, 0xa1), bold: false },
                ],
                right: vec![
                    SegmentDef { text: " #{cpu} ".into(), fg: Color(0xcd, 0xd6, 0xf4), bg: Color(0x45, 0x47, 0x5a), bold: false },
                    SegmentDef { text: " #{mem} ".into(), fg: Color(0xcd, 0xd6, 0xf4), bg: Color(0x31, 0x32, 0x44), bold: false },
                    SegmentDef { text: " #{time} ".into(), fg: Color(0x1e, 0x1e, 0x2e), bg: Color(0x89, 0xb4, 0xfa), bold: true },
                ],
                bg: Color(0x1e, 0x1e, 0x2e),
            },
        },
        Theme {
            name: "Gruvbox",
            status_bg: Color(0x28, 0x28, 0x28),
            status_fg: Color(0xfe, 0x80, 0x19),
            bar: StatusBarConfig {
                left: vec![
                    SegmentDef { text: " ▶ #{session} ".into(), fg: Color(0x28, 0x28, 0x28), bg: Color(0xfe, 0x80, 0x19), bold: true },
                    SegmentDef { text: " #{windows} ".into(), fg: Color(0xeb, 0xdb, 0xb2), bg: Color(0x3c, 0x38, 0x36), bold: false },
                    SegmentDef { text: " #{git} ".into(), fg: Color(0x28, 0x28, 0x28), bg: Color(0xb8, 0xbb, 0x26), bold: false },
                ],
                right: vec![
                    SegmentDef { text: " #{cpu} ".into(), fg: Color(0xeb, 0xdb, 0xb2), bg: Color(0x3c, 0x38, 0x36), bold: false },
                    SegmentDef { text: " #{mem} ".into(), fg: Color(0xeb, 0xdb, 0xb2), bg: Color(0x50, 0x49, 0x45), bold: false },
                    SegmentDef { text: " #{time} ".into(), fg: Color(0x28, 0x28, 0x28), bg: Color(0xfe, 0x80, 0x19), bold: true },
                ],
                bg: Color(0x28, 0x28, 0x28),
            },
        },
        Theme {
            name: "Dracula",
            status_bg: Color(0x28, 0x2a, 0x36),
            status_fg: Color(0xbd, 0x93, 0xf9),
            bar: StatusBarConfig {
                left: vec![
                    SegmentDef { text: " ▶ #{session} ".into(), fg: Color(0x28, 0x2a, 0x36), bg: Color(0xbd, 0x93, 0xf9), bold: true },
                    SegmentDef { text: " #{windows} ".into(), fg: Color(0xf8, 0xf8, 0xf2), bg: Color(0x44, 0x47, 0x5a), bold: false },
                    SegmentDef { text: " #{git} ".into(), fg: Color(0x28, 0x2a, 0x36), bg: Color(0x50, 0xfa, 0x7b), bold: false },
                ],
                right: vec![
                    SegmentDef { text: " #{cpu} ".into(), fg: Color(0xf8, 0xf8, 0xf2), bg: Color(0x44, 0x47, 0x5a), bold: false },
                    SegmentDef { text: " #{mem} ".into(), fg: Color(0xf8, 0xf8, 0xf2), bg: Color(0x38, 0x3a, 0x4c), bold: false },
                    SegmentDef { text: " #{time} ".into(), fg: Color(0x28, 0x2a, 0x36), bg: Color(0xbd, 0x93, 0xf9), bold: true },
                ],
                bg: Color(0x28, 0x2a, 0x36),
            },
        },
        Theme {
            name: "Nord",
            status_bg: Color(0x2e, 0x34, 0x40),
            status_fg: Color(0x88, 0xc0, 0xd0),
            bar: StatusBarConfig {
                left: vec![
                    SegmentDef { text: " ▶ #{session} ".into(), fg: Color(0x2e, 0x34, 0x40), bg: Color(0x88, 0xc0, 0xd0), bold: true },
                    SegmentDef { text: " #{windows} ".into(), fg: Color(0xec, 0xef, 0xf4), bg: Color(0x43, 0x4c, 0x5e), bold: false },
                    SegmentDef { text: " #{git} ".into(), fg: Color(0x2e, 0x34, 0x40), bg: Color(0xa3, 0xbe, 0x8c), bold: false },
                ],
                right: vec![
                    SegmentDef { text: " #{cpu} ".into(), fg: Color(0xec, 0xef, 0xf4), bg: Color(0x43, 0x4c, 0x5e), bold: false },
                    SegmentDef { text: " #{mem} ".into(), fg: Color(0xec, 0xef, 0xf4), bg: Color(0x3b, 0x42, 0x52), bold: false },
                    SegmentDef { text: " #{time} ".into(), fg: Color(0x2e, 0x34, 0x40), bg: Color(0x88, 0xc0, 0xd0), bold: true },
                ],
                bg: Color(0x2e, 0x34, 0x40),
            },
        },
        Theme {
            name: "Rose Pine",
            status_bg: Color(0x19, 0x17, 0x24),
            status_fg: Color(0xc4, 0xa7, 0xe7),
            bar: StatusBarConfig {
                left: vec![
                    SegmentDef { text: " ▶ #{session} ".into(), fg: Color(0x19, 0x17, 0x24), bg: Color(0xc4, 0xa7, 0xe7), bold: true },
                    SegmentDef { text: " #{windows} ".into(), fg: Color(0xe0, 0xde, 0xf4), bg: Color(0x26, 0x23, 0x3a), bold: false },
                    SegmentDef { text: " #{git} ".into(), fg: Color(0x19, 0x17, 0x24), bg: Color(0x9c, 0xcf, 0xd8), bold: false },
                ],
                right: vec![
                    SegmentDef { text: " #{cpu} ".into(), fg: Color(0xe0, 0xde, 0xf4), bg: Color(0x26, 0x23, 0x3a), bold: false },
                    SegmentDef { text: " #{mem} ".into(), fg: Color(0xe0, 0xde, 0xf4), bg: Color(0x1f, 0x1d, 0x2e), bold: false },
                    SegmentDef { text: " #{time} ".into(), fg: Color(0x19, 0x17, 0x24), bg: Color(0xc4, 0xa7, 0xe7), bold: true },
                ],
                bg: Color(0x19, 0x17, 0x24),
            },
        },
        Theme {
            name: "Minimal",
            status_bg: Color(0x18, 0x18, 0x18),
            status_fg: Color(0xa0, 0xa0, 0xa0),
            bar: StatusBarConfig {
                left: vec![
                    SegmentDef { text: " #{session} ".into(), fg: Color(0xff, 0xff, 0xff), bg: Color(0x33, 0x33, 0x33), bold: true },
                    SegmentDef { text: " #{windows} ".into(), fg: Color(0xcc, 0xcc, 0xcc), bg: Color(0x22, 0x22, 0x22), bold: false },
                ],
                right: vec![
                    SegmentDef { text: " #{time} ".into(), fg: Color(0xff, 0xff, 0xff), bg: Color(0x33, 0x33, 0x33), bold: false },
                ],
                bg: Color(0x18, 0x18, 0x18),
            },
        },
        Theme {
            name: "Solarized Dark",
            status_bg: Color(0x00, 0x2b, 0x36),
            status_fg: Color(0x26, 0x8b, 0xd2),
            bar: StatusBarConfig {
                left: vec![
                    SegmentDef { text: " ▶ #{session} ".into(), fg: Color(0x00, 0x2b, 0x36), bg: Color(0x26, 0x8b, 0xd2), bold: true },
                    SegmentDef { text: " #{windows} ".into(), fg: Color(0x93, 0xa1, 0xa1), bg: Color(0x07, 0x36, 0x42), bold: false },
                    SegmentDef { text: " #{git} ".into(), fg: Color(0x00, 0x2b, 0x36), bg: Color(0x85, 0x99, 0x00), bold: false },
                ],
                right: vec![
                    SegmentDef { text: " #{cpu} ".into(), fg: Color(0x93, 0xa1, 0xa1), bg: Color(0x07, 0x36, 0x42), bold: false },
                    SegmentDef { text: " #{mem} ".into(), fg: Color(0x93, 0xa1, 0xa1), bg: Color(0x00, 0x2b, 0x36), bold: false },
                    SegmentDef { text: " #{time} ".into(), fg: Color(0x00, 0x2b, 0x36), bg: Color(0x26, 0x8b, 0xd2), bold: true },
                ],
                bg: Color(0x00, 0x2b, 0x36),
            },
        },
    ]
}

/// Find a built-in theme by name (case-insensitive).
pub fn find_theme(name: &str) -> Option<Theme> {
    builtin_themes().into_iter().find(|t| t.name.eq_ignore_ascii_case(name))
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
        assert_eq!(cfg.prefix, "ctrl-a");
        assert_eq!(cfg.scrollback, 50_000);
        assert!(!cfg.bindings.is_empty()); // ships with default bindings
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
        // Default bindings + 2 user bindings
        let defaults = default_bindings().len();
        assert_eq!(cfg.bindings.len(), defaults + 2);
        // User bindings are appended after defaults
        assert_eq!(cfg.bindings[defaults], KeyBinding {
            modifier: "prefix".into(),
            key: "|".into(),
            action: "split-horizontal".into(),
        });
        assert_eq!(cfg.bindings[defaults + 1], KeyBinding {
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
        assert_eq!(cfg.prefix, "ctrl-a");
    }
}
