//! Parser for tmux.conf files, converting tmux keybindings and settings
//! into vtx-compatible configuration.
//!
//! Supports a practical subset of tmux directives:
//! - `set -g <option> <value>` for prefix, default-shell, history-limit, base-index, mouse
//! - `bind [-n] <key> <command> [args...]` for keybindings
//! - `unbind <key>` to remove a binding
//! - Comments (`#`) and blank lines are ignored
//! - Unsupported directives produce a warning on stderr but do not cause errors

use std::path::Path;

/// Configuration imported from a tmux.conf file.
#[derive(Debug, Clone, Default)]
pub struct TmuxConfig {
    /// Prefix key in vtx notation, e.g. "ctrl-a".
    pub prefix: Option<String>,
    /// Default shell path.
    pub default_shell: Option<String>,
    /// Scrollback history limit in lines.
    pub scrollback: Option<usize>,
    /// Whether mouse support is enabled.
    pub mouse: Option<bool>,
    /// Base index for windows.
    pub base_index: Option<usize>,
    /// Keybindings as (modifier, key, action) tuples, compatible with
    /// the Lua config system's `vtx.bind(modifier, key, action)` format.
    pub bindings: Vec<(String, String, String)>,
}

/// Parse a tmux.conf file at the given path and return a `TmuxConfig`.
///
/// If the file cannot be read, returns a default (empty) config and prints
/// a warning to stderr.
pub fn parse_tmux_conf(path: &str) -> TmuxConfig {
    let contents = match std::fs::read_to_string(Path::new(path)) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("vtx: warning: could not read tmux config {path}: {e}");
            return TmuxConfig::default();
        }
    };
    parse_tmux_conf_str(&contents)
}

/// Parse tmux.conf content from a string. Useful for testing.
pub fn parse_tmux_conf_str(source: &str) -> TmuxConfig {
    let mut config = TmuxConfig::default();

    for (line_no, raw_line) in source.lines().enumerate() {
        let line = strip_comment(raw_line).trim();
        if line.is_empty() {
            continue;
        }

        let tokens = tokenize(line);
        if tokens.is_empty() {
            continue;
        }

        match tokens[0].as_str() {
            "set" | "set-option" => parse_set(&tokens[1..], &mut config, line_no),
            "bind" | "bind-key" => parse_bind(&tokens[1..], &mut config, line_no),
            "unbind" | "unbind-key" => parse_unbind(&tokens[1..], &mut config, line_no),
            other => {
                eprintln!(
                    "vtx: tmux-compat: ignoring unsupported directive \
                     '{other}' at line {ln}",
                    ln = line_no + 1,
                );
            }
        }
    }

    config
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Strip a trailing `# comment` portion from a line, respecting quotes.
fn strip_comment(line: &str) -> &str {
    let mut in_single = false;
    let mut in_double = false;
    for (i, ch) in line.char_indices() {
        match ch {
            '\'' if !in_double => in_single = !in_single,
            '"' if !in_single => in_double = !in_double,
            '#' if !in_single && !in_double => return &line[..i],
            _ => {}
        }
    }
    line
}

/// Simple shell-style tokeniser: splits on whitespace, respects double quotes.
fn tokenize(line: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut in_quote = false;
    let mut chars = line.chars().peekable();

    while let Some(ch) = chars.next() {
        match ch {
            '"' => in_quote = !in_quote,
            ' ' | '\t' if !in_quote => {
                if !current.is_empty() {
                    tokens.push(std::mem::take(&mut current));
                }
            }
            _ => current.push(ch),
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

/// Handle `set [-g] <option> <value>`.
fn parse_set(args: &[String], config: &mut TmuxConfig, line_no: usize) {
    // Skip flags like -g, -s, -w, -u, etc.
    let mut i = 0;
    while i < args.len() && args[i].starts_with('-') {
        i += 1;
    }

    if i + 1 >= args.len() {
        // Not enough arguments; silently skip (could be `set -u option`).
        return;
    }

    let option = &args[i];
    let value = &args[i + 1];

    match option.as_str() {
        "prefix" | "prefix2" => {
            config.prefix = Some(tmux_key_to_vtx_key(value));
        }
        "default-shell" | "default-command" => {
            config.default_shell = Some(value.clone());
        }
        "history-limit" => {
            if let Ok(n) = value.parse::<usize>() {
                config.scrollback = Some(n);
            }
        }
        "base-index" => {
            if let Ok(n) = value.parse::<usize>() {
                config.base_index = Some(n);
            }
        }
        "mouse" => match value.as_str() {
            "on" | "yes" | "true" | "1" => config.mouse = Some(true),
            "off" | "no" | "false" | "0" => config.mouse = Some(false),
            _ => {}
        },
        _ => {
            eprintln!(
                "vtx: tmux-compat: ignoring unsupported set option \
                 '{option}' at line {ln}",
                ln = line_no + 1,
            );
        }
    }
}

/// Handle `bind [-n] [-r] <key> <command> [args...]`.
fn parse_bind(args: &[String], config: &mut TmuxConfig, line_no: usize) {
    let mut no_prefix = false;
    let mut i = 0;

    // Parse flags. A bare "-" is a key, not a flag.
    while i < args.len() && args[i].starts_with('-') && args[i].len() > 1 {
        match args[i].as_str() {
            "-n" => no_prefix = true,
            "-r" | "-T" => {
                // -r means repeatable; -T names a key table; skip next arg for -T.
                if args[i] == "-T" {
                    i += 1; // skip table name
                    // If the table is "root", it's equivalent to -n.
                    if i < args.len() && args[i] == "root" {
                        no_prefix = true;
                    }
                }
            }
            _ => {} // ignore unknown flags
        }
        i += 1;
    }

    if i >= args.len() {
        return;
    }

    let tmux_key = &args[i];
    i += 1;

    if i >= args.len() {
        return;
    }

    // Remaining tokens form the tmux command + arguments.
    let command_parts = &args[i..];

    let (modifier, key) = parse_tmux_key_spec(tmux_key, no_prefix);
    let action = map_tmux_command_to_action(command_parts);

    match action {
        Some(act) => {
            config.bindings.push((modifier, key, act));
        }
        None => {
            let cmd_str = command_parts.join(" ");
            eprintln!(
                "vtx: tmux-compat: ignoring unsupported bind command \
                 '{cmd_str}' at line {ln}",
                ln = line_no + 1,
            );
        }
    }
}

/// Handle `unbind [-n] <key>`.
fn parse_unbind(args: &[String], config: &mut TmuxConfig, _line_no: usize) {
    let mut no_prefix = false;
    let mut i = 0;

    while i < args.len() && args[i].starts_with('-') {
        if args[i] == "-n" {
            no_prefix = true;
        }
        i += 1;
    }

    if i >= args.len() {
        return;
    }

    let tmux_key = &args[i];
    let (modifier, key) = parse_tmux_key_spec(tmux_key, no_prefix);

    // Remove any existing binding that matches this modifier+key.
    config
        .bindings
        .retain(|(m, k, _)| !(m == &modifier && k == &key));
}

/// Convert a tmux key specification and the no-prefix flag into
/// a (modifier, key) pair in vtx format.
///
/// Examples:
///   `C-a`, prefix mode  -> ("prefix", "a")   [but C-a is usually set as prefix itself]
///   `|`, prefix mode    -> ("prefix", "|")
///   `M-h`, no-prefix    -> ("alt", "h")
///   `M-Left`, no-prefix -> ("alt", "Left")
///   `M-S-H`, no-prefix  -> ("alt-shift", "h")
///   `C-h`, prefix mode  -> ("prefix", "ctrl-h")
fn parse_tmux_key_spec(key_str: &str, no_prefix: bool) -> (String, String) {
    let base_modifier = if no_prefix {
        "none".to_string()
    } else {
        "prefix".to_string()
    };

    // Parse modifier prefixes: C- (ctrl), M- (alt/meta), S- (shift).
    let mut remaining = key_str;
    let mut has_ctrl = false;
    let mut has_alt = false;
    let mut has_shift = false;

    loop {
        if remaining.starts_with("C-") && remaining.len() > 2 {
            has_ctrl = true;
            remaining = &remaining[2..];
        } else if remaining.starts_with("M-") && remaining.len() > 2 {
            has_alt = true;
            remaining = &remaining[2..];
        } else if remaining.starts_with("S-") && remaining.len() > 2 {
            has_shift = true;
            remaining = &remaining[2..];
        } else {
            break;
        }
    }

    // Build the modifier string.
    let modifier = if has_alt && has_shift && !no_prefix {
        "prefix-alt-shift".to_string()
    } else if has_alt && has_shift {
        "alt-shift".to_string()
    } else if has_alt && has_ctrl {
        if no_prefix {
            "ctrl-alt".to_string()
        } else {
            "prefix-ctrl-alt".to_string()
        }
    } else if has_alt {
        "alt".to_string()
    } else if has_ctrl && no_prefix {
        "ctrl".to_string()
    } else if has_ctrl && !no_prefix {
        // ctrl + prefix: the key itself includes ctrl
        "prefix".to_string()
    } else if has_shift && no_prefix {
        "shift".to_string()
    } else if has_shift {
        "prefix-shift".to_string()
    } else {
        base_modifier
    };

    // Normalise the key name.
    let key = normalise_key_name(remaining);

    // For ctrl bindings under prefix, embed ctrl in the key name.
    let key = if has_ctrl && !no_prefix && !has_alt {
        format!("ctrl-{key}")
    } else {
        key
    };

    (modifier, key)
}

/// Normalise a tmux key name to vtx conventions (lowercase single chars,
/// named keys keep their casing).
fn normalise_key_name(name: &str) -> String {
    match name {
        // Named keys stay as-is.
        "Left" | "Right" | "Up" | "Down" | "Home" | "End" | "PageUp" | "PageDown"
        | "BSpace" | "Delete" | "Insert" | "Escape" | "Enter" | "Tab" | "Space" => {
            name.to_string()
        }
        // Function keys.
        s if s.starts_with('F') && s[1..].parse::<u32>().is_ok() => s.to_string(),
        // Single character: lowercase it for consistency.
        s if s.len() == 1 => s.to_lowercase(),
        // Anything else: pass through.
        s => s.to_string(),
    }
}

/// Convert a tmux key-name like "C-a" into vtx notation like "ctrl-a".
/// Used for the prefix key setting.
fn tmux_key_to_vtx_key(tmux_key: &str) -> String {
    let mut remaining = tmux_key;
    let mut parts = Vec::new();

    loop {
        if remaining.starts_with("C-") && remaining.len() > 2 {
            parts.push("ctrl");
            remaining = &remaining[2..];
        } else if remaining.starts_with("M-") && remaining.len() > 2 {
            parts.push("alt");
            remaining = &remaining[2..];
        } else if remaining.starts_with("S-") && remaining.len() > 2 {
            parts.push("shift");
            remaining = &remaining[2..];
        } else {
            break;
        }
    }

    parts.push(remaining);
    parts.join("-").to_lowercase()
}

/// Map a tmux command (with arguments) to a vtx action string.
///
/// Returns `None` for commands we don't recognise, so the caller can
/// emit a warning.
fn map_tmux_command_to_action(parts: &[String]) -> Option<String> {
    if parts.is_empty() {
        return None;
    }

    let cmd = parts[0].as_str();
    let args = &parts[1..];

    match cmd {
        "split-window" => {
            if args.iter().any(|a| a == "-h") {
                Some("split-horizontal".into())
            } else {
                // Default (or -v) is vertical split.
                Some("split-vertical".into())
            }
        }
        "select-pane" => {
            for arg in args {
                match arg.as_str() {
                    "-L" => return Some("focus-left".into()),
                    "-R" => return Some("focus-right".into()),
                    "-U" => return Some("focus-up".into()),
                    "-D" => return Some("focus-down".into()),
                    _ => {}
                }
            }
            Some("focus-next-pane".into())
        }
        "resize-pane" => {
            // Optionally capture the size argument.
            let amount = args
                .iter()
                .filter(|a| !a.starts_with('-'))
                .next()
                .map(|s| s.as_str())
                .unwrap_or("1");
            for arg in args {
                match arg.as_str() {
                    "-L" => return Some(format!("resize-left {amount}")),
                    "-R" => return Some(format!("resize-right {amount}")),
                    "-U" => return Some(format!("resize-up {amount}")),
                    "-D" => return Some(format!("resize-down {amount}")),
                    _ => {}
                }
            }
            None
        }
        "kill-pane" => Some("close-pane".into()),
        "kill-window" => Some("close-window".into()),
        "new-window" => Some("new-window".into()),
        "next-window" => Some("next-window".into()),
        "previous-window" => Some("previous-window".into()),
        "last-window" => Some("last-window".into()),
        "select-window" => {
            // `select-window -t :1` etc.
            for (i, arg) in args.iter().enumerate() {
                if arg == "-t" {
                    if let Some(target) = args.get(i + 1) {
                        let win_idx = target.trim_start_matches(':');
                        return Some(format!("select-window {win_idx}"));
                    }
                }
            }
            None
        }
        "copy-mode" => Some("enter-copy-mode".into()),
        "paste-buffer" => Some("paste".into()),
        "detach-client" | "detach" => Some("detach".into()),
        "send-prefix" => Some("send-prefix".into()),
        "display-message" => Some("display-message".into()),
        "command-prompt" => Some("command-prompt".into()),
        "list-keys" => Some("list-keys".into()),
        "source-file" | "source" => Some("reload-config".into()),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_strip_comment() {
        assert_eq!(strip_comment("set -g prefix C-a  # prefix key").trim(), "set -g prefix C-a");
        assert_eq!(strip_comment("# full line comment"), "");
        assert_eq!(strip_comment("bind | split-window -h"), "bind | split-window -h");
    }

    #[test]
    fn test_tokenize() {
        let tokens = tokenize(r#"set -g default-shell "/bin/my shell""#);
        assert_eq!(tokens, vec!["set", "-g", "default-shell", "/bin/my shell"]);
    }

    #[test]
    fn test_parse_prefix() {
        let cfg = parse_tmux_conf_str("set -g prefix C-a");
        assert_eq!(cfg.prefix, Some("ctrl-a".into()));
    }

    #[test]
    fn test_parse_default_shell() {
        let cfg = parse_tmux_conf_str("set -g default-shell /bin/zsh");
        assert_eq!(cfg.default_shell, Some("/bin/zsh".into()));
    }

    #[test]
    fn test_parse_history_limit() {
        let cfg = parse_tmux_conf_str("set -g history-limit 100000");
        assert_eq!(cfg.scrollback, Some(100_000));
    }

    #[test]
    fn test_parse_mouse() {
        let cfg = parse_tmux_conf_str("set -g mouse on");
        assert_eq!(cfg.mouse, Some(true));

        let cfg = parse_tmux_conf_str("set -g mouse off");
        assert_eq!(cfg.mouse, Some(false));
    }

    #[test]
    fn test_parse_base_index() {
        let cfg = parse_tmux_conf_str("set -g base-index 1");
        assert_eq!(cfg.base_index, Some(1));
    }

    #[test]
    fn test_bind_split_horizontal() {
        let cfg = parse_tmux_conf_str("bind | split-window -h");
        assert_eq!(cfg.bindings.len(), 1);
        assert_eq!(cfg.bindings[0], ("prefix".into(), "|".into(), "split-horizontal".into()));
    }

    #[test]
    fn test_bind_split_vertical() {
        let cfg = parse_tmux_conf_str("bind - split-window -v");
        assert_eq!(cfg.bindings.len(), 1);
        assert_eq!(cfg.bindings[0], ("prefix".into(), "-".into(), "split-vertical".into()));
    }

    #[test]
    fn test_bind_select_pane() {
        let cfg = parse_tmux_conf_str(
            "bind h select-pane -L\n\
             bind j select-pane -D\n\
             bind k select-pane -U\n\
             bind l select-pane -R",
        );
        assert_eq!(cfg.bindings.len(), 4);
        assert_eq!(cfg.bindings[0].2, "focus-left");
        assert_eq!(cfg.bindings[1].2, "focus-down");
        assert_eq!(cfg.bindings[2].2, "focus-up");
        assert_eq!(cfg.bindings[3].2, "focus-right");
        // All should be prefix-bound.
        for b in &cfg.bindings {
            assert_eq!(b.0, "prefix");
        }
    }

    #[test]
    fn test_bind_no_prefix_alt() {
        let cfg = parse_tmux_conf_str("bind -n M-h select-pane -L");
        assert_eq!(cfg.bindings.len(), 1);
        assert_eq!(cfg.bindings[0], ("alt".into(), "h".into(), "focus-left".into()));
    }

    #[test]
    fn test_bind_no_prefix_alt_arrow() {
        let cfg = parse_tmux_conf_str("bind -n M-Left select-pane -L");
        assert_eq!(cfg.bindings.len(), 1);
        assert_eq!(cfg.bindings[0], ("alt".into(), "Left".into(), "focus-left".into()));
    }

    #[test]
    fn test_bind_alt_shift_resize() {
        let cfg = parse_tmux_conf_str("bind -n M-S-H resize-pane -L 5");
        assert_eq!(cfg.bindings.len(), 1);
        assert_eq!(cfg.bindings[0].0, "alt-shift");
        assert_eq!(cfg.bindings[0].1, "h");
        assert_eq!(cfg.bindings[0].2, "resize-left 5");
    }

    #[test]
    fn test_bind_kill_pane() {
        let cfg = parse_tmux_conf_str("bind x kill-pane");
        assert_eq!(cfg.bindings[0], ("prefix".into(), "x".into(), "close-pane".into()));
    }

    #[test]
    fn test_unbind() {
        let cfg = parse_tmux_conf_str(
            "bind x kill-pane\n\
             unbind x",
        );
        assert!(cfg.bindings.is_empty());
    }

    #[test]
    fn test_unbind_ctrl_b() {
        // `unbind C-b` should remove any prefix binding for ctrl-b.
        let cfg = parse_tmux_conf_str("unbind C-b");
        // Just ensure it doesn't panic; there's nothing to remove.
        assert!(cfg.bindings.is_empty());
    }

    #[test]
    fn test_comments_and_blank_lines() {
        let input = "\
# This is a comment
set -g prefix C-a

# Another comment
bind | split-window -h
";
        let cfg = parse_tmux_conf_str(input);
        assert_eq!(cfg.prefix, Some("ctrl-a".into()));
        assert_eq!(cfg.bindings.len(), 1);
    }

    #[test]
    fn test_unsupported_directives_ignored() {
        // These should not panic, just emit warnings.
        let cfg = parse_tmux_conf_str(
            "set -g status-style bg=green\n\
             set-window-option -g automatic-rename on\n\
             run-shell ~/something.sh",
        );
        // Only verifiable side-effect: no bindings or settings set.
        assert!(cfg.prefix.is_none());
        assert!(cfg.bindings.is_empty());
    }

    #[test]
    fn test_full_config() {
        let input = "\
# Remap prefix
set -g prefix C-a
unbind C-b

# Shell & scrollback
set -g default-shell /bin/zsh
set -g history-limit 100000
set -g base-index 1
set -g mouse on

# Splits
bind | split-window -h
bind - split-window -v

# Navigation
bind h select-pane -L
bind j select-pane -D
bind k select-pane -U
bind l select-pane -R

# No-prefix alt navigation
bind -n M-h select-pane -L
bind -n M-j select-pane -D

# Resize
bind -n M-S-H resize-pane -L 5

# Close
bind x kill-pane
";
        let cfg = parse_tmux_conf_str(input);
        assert_eq!(cfg.prefix, Some("ctrl-a".into()));
        assert_eq!(cfg.default_shell, Some("/bin/zsh".into()));
        assert_eq!(cfg.scrollback, Some(100_000));
        assert_eq!(cfg.base_index, Some(1));
        assert_eq!(cfg.mouse, Some(true));
        // 4 prefix pane nav + 2 splits + 2 alt nav + 1 resize + 1 kill = 10
        assert_eq!(cfg.bindings.len(), 10);
    }

    #[test]
    fn test_tmux_key_to_vtx_key() {
        assert_eq!(tmux_key_to_vtx_key("C-a"), "ctrl-a");
        assert_eq!(tmux_key_to_vtx_key("C-b"), "ctrl-b");
        assert_eq!(tmux_key_to_vtx_key("M-a"), "alt-a");
        assert_eq!(tmux_key_to_vtx_key("C-M-a"), "ctrl-alt-a");
    }

    #[test]
    fn test_empty_input() {
        let cfg = parse_tmux_conf_str("");
        assert!(cfg.prefix.is_none());
        assert!(cfg.bindings.is_empty());
    }
}
