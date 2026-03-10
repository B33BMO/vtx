use serde::{Deserialize, Serialize};

use crate::cell::Cell;
use crate::types::{PaneId, SessionId};

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
    Detach,
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
        status: String,
        total_rows: u16,
    },
    Sessions { list: Vec<SessionInfo> },
    Error { msg: String },
    Detached,
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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionInfo {
    pub id: SessionId,
    pub name: String,
    pub pane_count: usize,
    pub created: u64,
}
