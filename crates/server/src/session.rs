use std::collections::HashMap;
use vtx_core::ipc::{LayoutPreset, PopupRect};
use vtx_core::{PaneId, SessionId};
use vtx_layout::{LayoutNode, Rect, SplitDir};

use crate::pane::Pane;

/// A single window (tab) within a session. Each window has its own layout tree and panes.
pub struct Window {
    pub name: String,
    pub layout: LayoutNode,
    pub panes: HashMap<PaneId, Pane>,
    pub focused_pane: PaneId,
    /// When Some, the given pane is zoomed to fill the entire pane area.
    pub zoomed_pane: Option<PaneId>,
    /// When Some, a floating popup pane is active overlaying the tiled layout.
    pub popup: Option<(PaneId, PopupRect)>,
    /// The current layout preset applied to this window, if any.
    pub current_preset: Option<LayoutPreset>,
}

pub struct Session {
    pub id: SessionId,
    pub name: String,
    pub windows: Vec<Window>,
    pub active_window: usize,
    next_pane_id: u32,
    pub created: u64,
}

impl Session {
    pub fn new(id: SessionId, name: String, first_pane: Pane) -> Self {
        let pane_id = first_pane.id;
        let mut panes = HashMap::new();
        panes.insert(pane_id, first_pane);

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let window = Window {
            name: "zsh".to_string(),
            layout: LayoutNode::single(pane_id),
            panes,
            focused_pane: pane_id,
            zoomed_pane: None,
            popup: None,
            current_preset: None,
        };

        Session {
            id,
            name,
            windows: vec![window],
            active_window: 0,
            next_pane_id: pane_id.0 + 1,
            created: now,
        }
    }

    /// Get a reference to the active window.
    pub fn active_window(&self) -> &Window {
        &self.windows[self.active_window]
    }

    /// Get a mutable reference to the active window.
    pub fn active_window_mut(&mut self) -> &mut Window {
        &mut self.windows[self.active_window]
    }

    pub fn alloc_pane_id(&mut self) -> PaneId {
        let id = PaneId(self.next_pane_id);
        self.next_pane_id += 1;
        id
    }

    pub fn split_pane(
        &mut self,
        target: PaneId,
        dir: SplitDir,
        shell: &str,
        cols: u16,
        rows: u16,
    ) -> vtx_core::Result<PaneId> {
        let new_id = self.alloc_pane_id();
        let pane = Pane::spawn(new_id, cols, rows, shell)?;
        let win = self.active_window_mut();
        win.panes.insert(new_id, pane);
        win.layout.split(target, new_id, dir);
        win.focused_pane = new_id;
        Ok(new_id)
    }

    pub fn resolve_layout(&self, cols: u16, rows: u16) -> Vec<(PaneId, Rect)> {
        let area = Rect {
            x: 0,
            y: 0,
            cols,
            rows,
        };
        self.active_window().layout.resolve(area)
    }

    /// Total number of panes across all windows.
    pub fn total_pane_count(&self) -> usize {
        self.windows.iter().map(|w| w.panes.len()).sum()
    }
}
