use serde::{Deserialize, Serialize};

/// Events that plugins can register hooks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookEvent {
    PaneCreate,
    PaneClose,
    KeyPress,
    PreRender,
    PostRender,
    Command,
    SessionCreate,
    SessionClose,
    WindowCreate,
    SessionDetach,
}

impl HookEvent {
    /// Convert from a string name used in Lua/WASM plugin registration.
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "on_pane_create" | "pane_create" => Some(Self::PaneCreate),
            "on_pane_close" | "pane_close" => Some(Self::PaneClose),
            "on_key" | "key_press" => Some(Self::KeyPress),
            "on_pre_render" | "pre_render" => Some(Self::PreRender),
            "on_render" | "on_post_render" | "post_render" => Some(Self::PostRender),
            "on_command" | "command" => Some(Self::Command),
            "on_session_create" | "session_create" => Some(Self::SessionCreate),
            "on_session_close" | "session_close" => Some(Self::SessionClose),
            "on_window_create" | "window_create" => Some(Self::WindowCreate),
            "on_session_detach" | "session_detach" => Some(Self::SessionDetach),
            _ => None,
        }
    }

    /// Canonical string name for the event.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::PaneCreate => "on_pane_create",
            Self::PaneClose => "on_pane_close",
            Self::KeyPress => "on_key",
            Self::PreRender => "on_pre_render",
            Self::PostRender => "on_post_render",
            Self::Command => "on_command",
            Self::SessionCreate => "on_session_create",
            Self::SessionClose => "on_session_close",
            Self::WindowCreate => "on_window_create",
            Self::SessionDetach => "on_session_detach",
        }
    }
}

/// Context data passed to hook callbacks.
///
/// Serialized to JSON when crossing the WASM boundary;
/// converted to a Lua table for Lua plugins.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HookContext {
    /// The pane id relevant to this event, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pane_id: Option<u32>,

    /// The session id relevant to this event, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<u32>,

    /// Key data for KeyPress events.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,

    /// Command name for Command events.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,

    /// Command arguments for Command events.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub args: Option<Vec<String>>,
}
