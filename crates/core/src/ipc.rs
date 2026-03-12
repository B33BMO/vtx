use serde::{Deserialize, Serialize};

use crate::cell::Cell;
use crate::types::{PaneId, SessionId};

/// Saved state of a single pane for session resurrect.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SavedPane {
    pub id: u32,
    pub cwd: Option<String>,
    pub command: Option<String>,
}

/// Saved state of a window for session resurrect.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SavedWindow {
    pub name: String,
    pub panes: Vec<SavedPane>,
    pub layout: String, // serialized layout JSON
    pub focused_pane: u32,
}

/// Full saved session state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SavedSession {
    pub name: String,
    pub windows: Vec<SavedWindow>,
    pub active_window: usize,
    pub saved_at: u64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum Direction {
    Up,
    Down,
    Left,
    Right,
}

/// Messages sent from client to server.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ClientMsg {
    NewSession { name: Option<String> },
    Attach { session: SessionId },
    ListSessions,
    Input { data: Vec<u8> },
    Resize { cols: u16, rows: u16 },
    Split { horizontal: bool },
    FocusDirection { dir: Direction },
    FocusPane { pane: PaneId },
    /// Resize the focused pane in a direction by `amount` cells.
    ResizePane { dir: Direction, amount: u16 },
    /// Kill the focused pane.
    KillPane,
    /// Open a new pane running an SSH connection.
    SshPane {
        host: String,
        user: Option<String>,
        port: Option<u16>,
    },
    /// Request scrollback content for the focused pane.
    /// offset=0 means current view, positive means lines back.
    ScrollBack { offset: i32 },
    /// Open a widget pane (cpu, mem, disk, net, sysinfo).
    Widget { kind: String },
    /// Toggle zoom on the focused pane (fullscreen / restore).
    ZoomPane,
    /// Open a floating popup pane. If command is None, spawn default shell.
    PopupPane { command: Option<String> },
    /// Close the active popup pane.
    ClosePopup,
    /// Search the focused pane's scrollback buffer for a query string.
    SearchScrollback { query: String },
    /// Create a new window (tab) in the current session.
    NewWindow { name: Option<String> },
    /// Switch to the next window.
    NextWindow,
    /// Switch to the previous window.
    PrevWindow,
    /// Switch to a window by index.
    SelectWindow { index: usize },
    /// Rename the current window.
    RenameWindow { name: String },
    /// Cycle to the next layout preset.
    LayoutCycle,
    /// Apply a specific layout preset.
    SelectLayout { preset: LayoutPreset },
    /// Drag a border/divider to resize panes. Identified by border position.
    DragBorder {
        border_x: u16,
        border_y: u16,
        horizontal: bool,
        delta: i16,
    },
    /// Swap the focused pane with a neighbor in the given direction.
    SwapPane { dir: Direction },
    /// Kill and respawn the focused pane's shell process.
    RespawnPane,
    Detach,
    /// Save the current session to disk for later resurrection.
    SaveSession,
    /// Restore a previously saved session from disk.
    RestoreSession { name: String },
    /// List all saved sessions on disk.
    ListSavedSessions,
    /// Reload config from a file (None = default config path).
    SourceConfig { path: Option<String> },
    /// Kill a named session.
    KillSession { name: String },
    /// Switch to a named built-in theme.
    SwitchTheme { name: String },
    /// List available built-in themes.
    ListThemes,
    /// Shut down the server.
    KillServer,
}

/// Preset layout arrangements that can be cycled through.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LayoutPreset {
    EvenHorizontal,
    EvenVertical,
    MainVertical,
    MainHorizontal,
    Tiled,
}

impl LayoutPreset {
    /// Cycle to the next preset.
    pub fn next(self) -> Self {
        match self {
            LayoutPreset::EvenHorizontal => LayoutPreset::EvenVertical,
            LayoutPreset::EvenVertical => LayoutPreset::MainVertical,
            LayoutPreset::MainVertical => LayoutPreset::MainHorizontal,
            LayoutPreset::MainHorizontal => LayoutPreset::Tiled,
            LayoutPreset::Tiled => LayoutPreset::EvenHorizontal,
        }
    }
}

/// Messages sent from server to client.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ServerMsg {
    SessionReady {
        session: SessionId,
        cols: u16,
        rows: u16,
    },
    Render {
        panes: Vec<PaneRender>,
        focused: PaneId,
        borders: Vec<(u16, u16, u16, bool)>,
        status: StyledStatus,
        total_rows: u16,
    },
    Sessions { list: Vec<SessionInfo> },
    /// Result of a scrollback search.
    SearchResult { offset: i32, matches: usize },
    Error { msg: String },
    Detached,
    /// Confirmation that a session was saved.
    SessionSaved,
    /// List of saved session names on disk.
    SavedSessions { list: Vec<String> },
    /// Confirmation that config was reloaded.
    ConfigReloaded,
    /// List of available theme names.
    ThemeList { themes: Vec<String>, active: String },
    /// Confirmation before server exits.
    ServerShutdown,
    /// Confirmation that a session was killed.
    SessionKilled { name: String },
}

/// A single styled segment of the status bar.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatusSegment {
    pub text: String,
    pub fg: (u8, u8, u8),
    pub bg: (u8, u8, u8),
    pub bold: bool,
    /// Optional click action (e.g., "new-window", "next-window").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub click: Option<String>,
}

/// Styled status bar with independently colored left/right segments.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StyledStatus {
    pub left: Vec<StatusSegment>,
    pub right: Vec<StatusSegment>,
    pub bg: (u8, u8, u8),
}

impl StyledStatus {
    /// Flatten all segments into a plain string (for backward-compatible rendering).
    pub fn to_plain_text(&self) -> String {
        let left: String = self.left.iter().map(|s| s.text.as_str()).collect();
        let right: String = self.right.iter().map(|s| s.text.as_str()).collect();
        if right.is_empty() {
            left
        } else {
            format!("{left}  {right}")
        }
    }

    /// Create a simple single-segment status (for search mode, scroll indicators, etc.)
    pub fn simple(text: &str, fg: (u8, u8, u8), bg: (u8, u8, u8)) -> Self {
        StyledStatus {
            left: vec![StatusSegment {
                text: text.to_string(),
                fg,
                bg,
                bold: false,
                click: None,
            }],
            right: vec![],
            bg,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PaneRender {
    pub id: PaneId,
    pub x: u16,
    pub y: u16,
    pub cols: u16,
    pub rows: u16,
    pub content: Vec<Vec<Cell>>,
    pub cursor_x: u16,
    pub cursor_y: u16,
    pub cursor_visible: bool,
    /// Whether this pane is a floating overlay (popup).
    #[serde(default)]
    pub floating: bool,
}

/// Dimensions and position of a floating popup pane.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct PopupRect {
    pub x: u16,
    pub y: u16,
    pub cols: u16,
    pub rows: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionInfo {
    pub id: SessionId,
    pub name: String,
    pub pane_count: usize,
    pub created: u64,
}
