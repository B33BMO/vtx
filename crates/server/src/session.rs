use std::collections::HashMap;
use vtx_core::{PaneId, SessionId};
use vtx_layout::{LayoutNode, Rect, SplitDir};

use crate::pane::Pane;

pub struct Session {
    pub id: SessionId,
    pub name: String,
    pub layout: LayoutNode,
    pub panes: HashMap<PaneId, Pane>,
    pub focused_pane: PaneId,
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

        Session {
            id,
            name,
            layout: LayoutNode::single(pane_id),
            panes,
            focused_pane: pane_id,
            next_pane_id: pane_id.0 + 1,
            created: now,
        }
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
        self.panes.insert(new_id, pane);
        self.layout.split(target, new_id, dir);
        self.focused_pane = new_id;
        Ok(new_id)
    }

    pub fn resolve_layout(&self, cols: u16, rows: u16) -> Vec<(PaneId, Rect)> {
        let area = Rect {
            x: 0,
            y: 0,
            cols,
            rows,
        };
        self.layout.resolve(area)
    }
}
