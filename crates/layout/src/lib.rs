use serde::{Deserialize, Serialize};
use vtx_core::PaneId;
pub use vtx_core::ipc::LayoutPreset;

/// Direction of a split.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SplitDir {
    Horizontal,
    Vertical,
}

/// Build a layout tree from a list of pane IDs using a preset.
pub fn build_preset(preset: &LayoutPreset, panes: &[PaneId]) -> LayoutNode {
    if panes.is_empty() {
        // Should not happen, but return a dummy
        return LayoutNode::Pane(PaneId(0));
    }
    if panes.len() == 1 {
        return LayoutNode::Pane(panes[0]);
    }

    match preset {
        LayoutPreset::EvenHorizontal => build_even_chain(panes, SplitDir::Horizontal),
        LayoutPreset::EvenVertical => build_even_chain(panes, SplitDir::Vertical),
        LayoutPreset::MainVertical => build_main_vertical(panes),
        LayoutPreset::MainHorizontal => build_main_horizontal(panes),
        LayoutPreset::Tiled => build_tiled(panes),
    }
}

/// Build a chain of even splits in the given direction.
/// For N panes, the first split gives 1/N to the first pane, then
/// the second child recursively splits the remaining N-1 panes.
fn build_even_chain(panes: &[PaneId], dir: SplitDir) -> LayoutNode {
    if panes.len() == 1 {
        return LayoutNode::Pane(panes[0]);
    }
    let n = panes.len() as f32;
    let ratio = 1.0 / n;
    LayoutNode::Split {
        dir,
        ratio,
        first: Box::new(LayoutNode::Pane(panes[0])),
        second: Box::new(build_even_chain(&panes[1..], dir)),
    }
}

/// Main-vertical: first pane ~65% on left, remaining stacked vertically on right.
fn build_main_vertical(panes: &[PaneId]) -> LayoutNode {
    if panes.len() == 2 {
        return LayoutNode::Split {
            dir: SplitDir::Horizontal,
            ratio: 0.65,
            first: Box::new(LayoutNode::Pane(panes[0])),
            second: Box::new(LayoutNode::Pane(panes[1])),
        };
    }
    LayoutNode::Split {
        dir: SplitDir::Horizontal,
        ratio: 0.65,
        first: Box::new(LayoutNode::Pane(panes[0])),
        second: Box::new(build_even_chain(&panes[1..], SplitDir::Vertical)),
    }
}

/// Main-horizontal: first pane ~65% on top, remaining side by side on bottom.
fn build_main_horizontal(panes: &[PaneId]) -> LayoutNode {
    if panes.len() == 2 {
        return LayoutNode::Split {
            dir: SplitDir::Vertical,
            ratio: 0.65,
            first: Box::new(LayoutNode::Pane(panes[0])),
            second: Box::new(LayoutNode::Pane(panes[1])),
        };
    }
    LayoutNode::Split {
        dir: SplitDir::Vertical,
        ratio: 0.65,
        first: Box::new(LayoutNode::Pane(panes[0])),
        second: Box::new(build_even_chain(&panes[1..], SplitDir::Horizontal)),
    }
}

/// Tiled (dwindle/spiral): each new pane splits the previous one,
/// alternating horizontal and vertical — like Hyprland's dwindle layout.
///
/// 1: [A]
/// 2: [A ][B ]           ← horizontal
/// 3: [A ][B ]           ← B splits vertical
///         [C ]
/// 4: [A ][B ]           ← C splits horizontal
///         [C][D]
/// 5: [A ][B ]           ← D splits vertical
///         [C][D]
///              [E]
fn build_tiled(panes: &[PaneId]) -> LayoutNode {
    if panes.len() == 1 {
        return LayoutNode::Pane(panes[0]);
    }

    // Start with first pane, then each subsequent pane splits the "last" region,
    // alternating direction starting with horizontal.
    let mut root = LayoutNode::Pane(panes[0]);
    let mut last_pane = panes[0];

    for (i, &new_pane) in panes.iter().enumerate().skip(1) {
        // Alternate: even index (1st split) = horizontal, odd = vertical, etc.
        let dir = if i % 2 == 1 {
            SplitDir::Horizontal
        } else {
            SplitDir::Vertical
        };
        root.split(last_pane, new_pane, dir);
        last_pane = new_pane;
    }

    root
}

/// A binary tree of splits, with panes at the leaves.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum LayoutNode {
    Pane(PaneId),
    Split {
        dir: SplitDir,
        /// Ratio of first child (0.0 - 1.0)
        ratio: f32,
        first: Box<LayoutNode>,
        second: Box<LayoutNode>,
    },
}

/// A resolved rectangle for a pane.
#[derive(Debug, Clone, Copy)]
pub struct Rect {
    pub x: u16,
    pub y: u16,
    pub cols: u16,
    pub rows: u16,
}

impl Rect {
    /// Center point of the rect.
    pub fn center(&self) -> (i32, i32) {
        (
            self.x as i32 + self.cols as i32 / 2,
            self.y as i32 + self.rows as i32 / 2,
        )
    }
}

/// Info about a border segment to draw.
#[derive(Debug, Clone, Copy)]
pub struct Border {
    pub x: u16,
    pub y: u16,
    pub length: u16,
    pub horizontal: bool,
}

impl LayoutNode {
    /// Create a layout with a single pane.
    pub fn single(pane: PaneId) -> Self {
        LayoutNode::Pane(pane)
    }

    /// Split a pane, replacing it with two panes.
    pub fn split(&mut self, target: PaneId, new_pane: PaneId, dir: SplitDir) -> bool {
        match self {
            LayoutNode::Pane(id) if *id == target => {
                *self = LayoutNode::Split {
                    dir,
                    ratio: 0.5,
                    first: Box::new(LayoutNode::Pane(target)),
                    second: Box::new(LayoutNode::Pane(new_pane)),
                };
                true
            }
            LayoutNode::Split { first, second, .. } => {
                first.split(target, new_pane, dir) || second.split(target, new_pane, dir)
            }
            _ => false,
        }
    }

    /// Resolve the layout tree into absolute rectangles for each pane.
    pub fn resolve(&self, area: Rect) -> Vec<(PaneId, Rect)> {
        let mut result = Vec::new();
        self.resolve_inner(area, &mut result);
        result
    }

    /// Collect border segments from the layout tree.
    pub fn borders(&self, area: Rect) -> Vec<Border> {
        let mut result = Vec::new();
        self.borders_inner(area, &mut result);
        result
    }

    fn resolve_inner(&self, area: Rect, out: &mut Vec<(PaneId, Rect)>) {
        match self {
            LayoutNode::Pane(id) => out.push((*id, area)),
            LayoutNode::Split {
                dir,
                ratio,
                first,
                second,
            } => {
                let (a, b) = split_area(area, *dir, *ratio);
                first.resolve_inner(a, out);
                second.resolve_inner(b, out);
            }
        }
    }

    fn borders_inner(&self, area: Rect, out: &mut Vec<Border>) {
        if let LayoutNode::Split {
            dir,
            ratio,
            first,
            second,
        } = self
        {
            // The border sits between the two children
            match dir {
                SplitDir::Horizontal => {
                    let left_cols = ((area.cols as f32) * ratio) as u16;
                    let border_x = area.x + left_cols;
                    out.push(Border {
                        x: border_x,
                        y: area.y,
                        length: area.rows,
                        horizontal: false,
                    });
                }
                SplitDir::Vertical => {
                    let top_rows = ((area.rows as f32) * ratio) as u16;
                    let border_y = area.y + top_rows;
                    out.push(Border {
                        x: area.x,
                        y: border_y,
                        length: area.cols,
                        horizontal: true,
                    });
                }
            }

            let (a, b) = split_area(area, *dir, *ratio);
            first.borders_inner(a, out);
            second.borders_inner(b, out);
        }
    }

    /// Remove a pane from the layout tree.
    /// When a pane is removed from a Split, the sibling takes over the parent's position.
    /// Returns true if the pane was found and removed.
    pub fn remove(&mut self, target: PaneId) -> bool {
        match self {
            LayoutNode::Pane(id) => {
                // Can't remove the root pane from itself — caller handles that
                *id == target
            }
            LayoutNode::Split { first, second, .. } => {
                // Check if either child is the target pane
                if matches!(first.as_ref(), LayoutNode::Pane(id) if *id == target) {
                    // Replace self with the second child
                    *self = *second.clone();
                    return true;
                }
                if matches!(second.as_ref(), LayoutNode::Pane(id) if *id == target) {
                    // Replace self with the first child
                    *self = *first.clone();
                    return true;
                }
                // Recurse into children
                first.remove(target) || second.remove(target)
            }
        }
    }

    /// Resize a pane by adjusting the ratio of its nearest compatible ancestor split.
    /// `delta` is in the range -1.0..1.0, computed by caller as amount/total_size.
    /// Returns true if a resize happened.
    pub fn resize_pane(&mut self, target: PaneId, dir: vtx_core::ipc::Direction, delta: f32) -> bool {
        let compatible_split = match dir {
            vtx_core::ipc::Direction::Left | vtx_core::ipc::Direction::Right => SplitDir::Horizontal,
            vtx_core::ipc::Direction::Up | vtx_core::ipc::Direction::Down => SplitDir::Vertical,
        };
        // Positive delta = grow the focused pane in that direction
        let grow_first = matches!(dir, vtx_core::ipc::Direction::Right | vtx_core::ipc::Direction::Down);
        self.resize_pane_inner(target, compatible_split, delta, grow_first)
    }

    fn resize_pane_inner(&mut self, target: PaneId, compat: SplitDir, delta: f32, grow_first: bool) -> bool {
        match self {
            LayoutNode::Pane(_) => false,
            LayoutNode::Split { dir, ratio, first, second } => {
                let in_first = first.contains_pane(target);
                let in_second = second.contains_pane(target);

                if !in_first && !in_second {
                    return false;
                }

                // If this split's direction matches, and the target is in one of the children
                if *dir == compat {
                    if in_first {
                        // Target is in first child — growing "right/down" means increasing ratio
                        let adjustment = if grow_first { delta } else { -delta };
                        *ratio = (*ratio + adjustment).clamp(0.1, 0.9);
                        return true;
                    } else {
                        // Target is in second child — growing "right/down" means decreasing ratio
                        let adjustment = if grow_first { -delta } else { delta };
                        *ratio = (*ratio + adjustment).clamp(0.1, 0.9);
                        return true;
                    }
                }

                // Wrong split direction — recurse into the child that contains the target
                if in_first {
                    first.resize_pane_inner(target, compat, delta, grow_first)
                } else {
                    second.resize_pane_inner(target, compat, delta, grow_first)
                }
            }
        }
    }

    /// Check if this subtree contains a given pane.
    fn contains_pane(&self, target: PaneId) -> bool {
        match self {
            LayoutNode::Pane(id) => *id == target,
            LayoutNode::Split { first, second, .. } => {
                first.contains_pane(target) || second.contains_pane(target)
            }
        }
    }

    /// Adjust the split ratio of the split node whose border is at the given position.
    /// `border_x`/`border_y` identify the border, `horizontal` its orientation,
    /// and `delta` is the number of cells to shift (positive = right/down).
    /// Returns true if a matching border was found and adjusted.
    pub fn adjust_border_at(
        &mut self,
        area: Rect,
        border_x: u16,
        border_y: u16,
        horizontal: bool,
        delta: i16,
    ) -> bool {
        match self {
            LayoutNode::Pane(_) => false,
            LayoutNode::Split { dir, ratio, first, second } => {
                match dir {
                    SplitDir::Horizontal if !horizontal => {
                        // Vertical border line at x = area.x + left_cols
                        let left_cols = ((area.cols as f32) * *ratio) as u16;
                        let bx = area.x + left_cols;
                        if bx == border_x && border_y >= area.y && border_y < area.y + area.rows {
                            // This is the border — adjust ratio
                            let new_ratio = *ratio + (delta as f32 / area.cols as f32);
                            *ratio = new_ratio.clamp(0.1, 0.9);
                            return true;
                        }
                    }
                    SplitDir::Vertical if horizontal => {
                        // Horizontal border line at y = area.y + top_rows
                        let top_rows = ((area.rows as f32) * *ratio) as u16;
                        let by = area.y + top_rows;
                        if by == border_y && border_x >= area.x && border_x < area.x + area.cols {
                            let new_ratio = *ratio + (delta as f32 / area.rows as f32);
                            *ratio = new_ratio.clamp(0.1, 0.9);
                            return true;
                        }
                    }
                    _ => {}
                }

                // Recurse into children
                let (a, b) = split_area(area, *dir, *ratio);
                if first.adjust_border_at(a, border_x, border_y, horizontal, delta) {
                    return true;
                }
                second.adjust_border_at(b, border_x, border_y, horizontal, delta)
            }
        }
    }

    /// Swap two pane IDs in the layout tree.
    pub fn swap_panes(&mut self, a: PaneId, b: PaneId) {
        // First pass: rename a -> sentinel, second pass: b -> a, third: sentinel -> b
        let sentinel = PaneId(u32::MAX);
        self.rename_pane(a, sentinel);
        self.rename_pane(b, a);
        self.rename_pane(sentinel, b);
    }

    fn rename_pane(&mut self, from: PaneId, to: PaneId) {
        match self {
            LayoutNode::Pane(id) if *id == from => *id = to,
            LayoutNode::Split { first, second, .. } => {
                first.rename_pane(from, to);
                second.rename_pane(from, to);
            }
            _ => {}
        }
    }

    /// List all pane IDs in the layout.
    pub fn pane_ids(&self) -> Vec<PaneId> {
        match self {
            LayoutNode::Pane(id) => vec![*id],
            LayoutNode::Split { first, second, .. } => {
                let mut ids = first.pane_ids();
                ids.extend(second.pane_ids());
                ids
            }
        }
    }

    /// Find the next pane in a given direction from the focused pane.
    pub fn find_neighbor(
        &self,
        area: Rect,
        focused: PaneId,
        dir: vtx_core::ipc::Direction,
    ) -> Option<PaneId> {
        let resolved = self.resolve(area);
        let focused_rect = resolved.iter().find(|(id, _)| *id == focused)?.1;
        let (fx, fy) = focused_rect.center();

        let mut best: Option<(PaneId, i32)> = None;

        for (id, rect) in &resolved {
            if *id == focused {
                continue;
            }
            let (cx, cy) = rect.center();

            let valid = match dir {
                vtx_core::ipc::Direction::Up => cy < fy,
                vtx_core::ipc::Direction::Down => cy > fy,
                vtx_core::ipc::Direction::Left => cx < fx,
                vtx_core::ipc::Direction::Right => cx > fx,
            };

            if !valid {
                continue;
            }

            let dist = (cx - fx).abs() + (cy - fy).abs();
            if best.is_none() || dist < best.unwrap().1 {
                best = Some((*id, dist));
            }
        }

        best.map(|(id, _)| id)
    }
}

fn split_area(area: Rect, dir: SplitDir, ratio: f32) -> (Rect, Rect) {
    match dir {
        SplitDir::Horizontal => {
            let left_cols = ((area.cols as f32) * ratio) as u16;
            let right_cols = area.cols.saturating_sub(left_cols + 1);
            let a = Rect {
                x: area.x,
                y: area.y,
                cols: left_cols,
                rows: area.rows,
            };
            let b = Rect {
                x: area.x + left_cols + 1,
                y: area.y,
                cols: right_cols,
                rows: area.rows,
            };
            (a, b)
        }
        SplitDir::Vertical => {
            let top_rows = ((area.rows as f32) * ratio) as u16;
            let bottom_rows = area.rows.saturating_sub(top_rows + 1);
            let a = Rect {
                x: area.x,
                y: area.y,
                cols: area.cols,
                rows: top_rows,
            };
            let b = Rect {
                x: area.x,
                y: area.y + top_rows + 1,
                cols: area.cols,
                rows: bottom_rows,
            };
            (a, b)
        }
    }
}
