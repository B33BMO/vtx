use crossterm::{
    cursor, execute, queue,
    style::{self, Attribute, Color as CtColor, SetAttribute, SetBackgroundColor, SetForegroundColor},
    terminal,
};
use std::io::{self, Write};
use vtx_core::cell::{Attr, Cell, Color};
use vtx_core::ipc::{PaneRender, StyledStatus};
use vtx_core::PaneId;

const BORDER_V: char = '│';
const BORDER_H: char = '─';

// Box-drawing characters for popup borders
const BOX_TL: char = '╭';
const BOX_TR: char = '╮';
const BOX_BL: char = '╰';
const BOX_BR: char = '╯';
const BOX_H: char = '─';
const BOX_V: char = '│';

/// Screen-coordinate selection range (line-based).
#[derive(Debug, Clone, Copy)]
pub struct Selection {
    /// Start position (where mouse was pressed).
    pub start_x: u16,
    pub start_y: u16,
    /// End position (where mouse currently is / was released).
    pub end_x: u16,
    pub end_y: u16,
    /// Pane bounds to constrain highlight rendering.
    pub pane_x: u16,
    pub pane_y: u16,
    pub pane_cols: u16,
    pub pane_rows: u16,
}

impl Selection {
    /// Normalize so that (sy, sx) <= (ey, ex) in reading order.
    fn normalized(&self) -> (u16, u16, u16, u16) {
        if self.start_y < self.end_y
            || (self.start_y == self.end_y && self.start_x <= self.end_x)
        {
            (self.start_x, self.start_y, self.end_x, self.end_y)
        } else {
            (self.end_x, self.end_y, self.start_x, self.start_y)
        }
    }

    /// Check if screen position (x, y) is inside the selection,
    /// constrained to the pane bounds.
    fn contains(&self, x: u16, y: u16) -> bool {
        // Clip to pane bounds first
        let px_end = self.pane_x + self.pane_cols.saturating_sub(1);
        let py_end = self.pane_y + self.pane_rows.saturating_sub(1);
        if self.pane_cols > 0 && self.pane_rows > 0 {
            if x < self.pane_x || x > px_end || y < self.pane_y || y > py_end {
                return false;
            }
        }

        let (sx, sy, ex, ey) = self.normalized();
        if y < sy || y > ey {
            return false;
        }
        // Clamp line start/end to pane horizontal bounds
        let line_start = if y == sy { sx } else { self.pane_x };
        let line_end = if y == ey { ex } else { px_end };
        x >= line_start && x <= line_end
    }
}

/// Sentinel cell used to force full redraw (never matches any real cell).
fn sentinel_cell() -> Cell {
    Cell {
        c: '\x00',
        fg: Color::Rgb(255, 0, 255),
        bg: Color::Rgb(255, 0, 255),
        attr: Attr::all(),
    }
}

/// A clickable region on the status bar.
#[derive(Debug, Clone)]
pub struct StatusClickZone {
    pub col_start: u16,
    pub col_end: u16,
    pub action: String,
}

pub struct TtyRenderer {
    stdout: io::Stdout,
    /// What is currently displayed on screen.
    front: Vec<Cell>,
    /// What we want to display next.
    back: Vec<Cell>,
    screen_cols: u16,
    screen_rows: u16,
    /// Status bar colors (configurable via Lua config).
    pub status_fg: Color,
    pub status_bg: Color,
    /// Clickable zones on the status bar from the last render.
    pub click_zones: Vec<StatusClickZone>,
}

impl TtyRenderer {
    pub fn new() -> io::Result<Self> {
        let mut stdout = io::stdout();
        terminal::enable_raw_mode()?;
        execute!(
            stdout,
            terminal::EnterAlternateScreen,
            cursor::Hide,
            terminal::Clear(terminal::ClearType::All),
        )?;

        let (cols, rows) = terminal::size().unwrap_or((80, 24));
        let size = cols as usize * rows as usize;

        Ok(TtyRenderer {
            stdout,
            front: vec![sentinel_cell(); size], // sentinel forces full first draw
            back: vec![Cell::default(); size],
            screen_cols: cols,
            screen_rows: rows,
            status_fg: Color::Rgb(0x7a, 0xa2, 0xf7),  // Tokyo Night blue
            status_bg: Color::Rgb(0x1a, 0x1b, 0x26),  // Tokyo Night bg
            click_zones: vec![],
        })
    }

    /// Handle terminal resize.
    fn ensure_size(&mut self, cols: u16, rows: u16) {
        if cols != self.screen_cols || rows != self.screen_rows {
            let size = cols as usize * rows as usize;
            self.screen_cols = cols;
            self.screen_rows = rows;
            self.front = vec![sentinel_cell(); size]; // force full redraw
            self.back = vec![Cell::default(); size];
        }
    }

    /// Invalidate the front buffer so the next render_frame does a full redraw.
    /// Use after drawing overlays (like context menus) directly to stdout.
    pub fn invalidate(&mut self) {
        self.front.fill(sentinel_cell());
    }

    /// Render panes, borders, and status bar using differential updates.
    pub fn render_frame(
        &mut self,
        panes: &[PaneRender],
        focused: PaneId,
        borders: &[(u16, u16, u16, bool)],
        status: &StyledStatus,
        _total_rows: u16,
        prefix_active: bool,
        selection: Option<&Selection>,
    ) -> io::Result<()> {
        let cols = self.screen_cols;
        let rows = self.screen_rows;
        self.ensure_size(cols, rows);

        // Clear back buffer
        self.back.fill(Cell::default());

        // Stamp borders into back buffer
        for &(x, y, length, horizontal) in borders {
            if horizontal {
                for i in 0..length {
                    self.set_back(x + i, y, Cell {
                        c: BORDER_H,
                        fg: Color::Indexed(8), // dark gray
                        bg: Color::Default,
                        attr: Attr::empty(),
                    });
                }
            } else {
                for i in 0..length {
                    self.set_back(x, y + i, Cell {
                        c: BORDER_V,
                        fg: Color::Indexed(8),
                        bg: Color::Default,
                        attr: Attr::empty(),
                    });
                }
            }
        }

        // Stamp non-floating pane content into back buffer first
        for pane in panes.iter().filter(|p| !p.floating) {
            for (row_idx, row) in pane.content.iter().enumerate() {
                let y = pane.y + row_idx as u16;
                if y >= rows {
                    break;
                }
                for (col_idx, cell) in row.iter().enumerate() {
                    let x = pane.x + col_idx as u16;
                    if x >= cols {
                        break;
                    }
                    self.set_back(x, y, cell.clone());
                }
            }
        }

        // Draw floating (popup) pane borders and content on top
        let border_fg = Color::Rgb(120, 160, 220);
        let border_bg = Color::Rgb(30, 30, 30);
        for pane in panes.iter().filter(|p| p.floating) {
            // The border is drawn 1 cell outside the pane content area
            let bx = pane.x.saturating_sub(1);
            let by = pane.y.saturating_sub(1);
            let bw = pane.cols + 2; // border adds 1 on each side
            let bh = pane.rows + 2;

            let border_cell = |c: char| Cell {
                c,
                fg: border_fg,
                bg: border_bg,
                attr: Attr::empty(),
            };

            // Top border
            if by < rows {
                self.set_back(bx, by, border_cell(BOX_TL));
                for i in 1..bw.saturating_sub(1) {
                    if bx + i < cols {
                        self.set_back(bx + i, by, border_cell(BOX_H));
                    }
                }
                if bx + bw - 1 < cols {
                    self.set_back(bx + bw - 1, by, border_cell(BOX_TR));
                }
            }

            // Bottom border
            let bottom_y = by + bh - 1;
            if bottom_y < rows {
                self.set_back(bx, bottom_y, border_cell(BOX_BL));
                for i in 1..bw.saturating_sub(1) {
                    if bx + i < cols {
                        self.set_back(bx + i, bottom_y, border_cell(BOX_H));
                    }
                }
                if bx + bw - 1 < cols {
                    self.set_back(bx + bw - 1, bottom_y, border_cell(BOX_BR));
                }
            }

            // Left and right borders
            for i in 1..bh.saturating_sub(1) {
                let y = by + i;
                if y < rows {
                    self.set_back(bx, y, border_cell(BOX_V));
                    if bx + bw - 1 < cols {
                        self.set_back(bx + bw - 1, y, border_cell(BOX_V));
                    }
                }
            }

            // Stamp floating pane content (on top of everything)
            for (row_idx, row) in pane.content.iter().enumerate() {
                let y = pane.y + row_idx as u16;
                if y >= rows {
                    break;
                }
                for (col_idx, cell) in row.iter().enumerate() {
                    let x = pane.x + col_idx as u16;
                    if x >= cols {
                        break;
                    }
                    self.set_back(x, y, cell.clone());
                }
            }
        }

        // Apply selection highlight (swap fg/bg for selected cells)
        if let Some(sel) = selection {
            for y in 0..rows.saturating_sub(1) {
                for x in 0..cols {
                    if sel.contains(x, y) {
                        let idx = y as usize * cols as usize + x as usize;
                        if idx < self.back.len() {
                            let cell = &mut self.back[idx];
                            // Swap foreground and background for selection highlight
                            std::mem::swap(&mut cell.fg, &mut cell.bg);
                            // If both are default, use visible colors
                            if cell.fg == Color::Default {
                                cell.fg = Color::Rgb(0, 0, 0);
                            }
                            if cell.bg == Color::Default {
                                cell.bg = Color::Rgb(200, 200, 255);
                            }
                        }
                    }
                }
            }
        }

        // Stamp status bar into back buffer (last row)
        let status_y = rows.saturating_sub(1);
        let gap_bg = Color::Rgb(status.bg.0, status.bg.1, status.bg.2);

        // Fill entire status row with gap bg
        for x in 0..cols {
            self.set_back(x, status_y, Cell {
                c: ' ',
                fg: Color::Default,
                bg: gap_bg,
                attr: Attr::empty(),
            });
        }

        // Collect left segments, optionally appending a PREFIX indicator
        let prefix_seg;
        let left_segs: &[vtx_core::ipc::StatusSegment] = &status.left;
        let mut left_with_prefix: Vec<&vtx_core::ipc::StatusSegment>;
        let left_segs = if prefix_active {
            prefix_seg = vtx_core::ipc::StatusSegment {
                text: " [PREFIX] ".to_string(),
                fg: (0x1a, 0x1b, 0x26),
                bg: (0xe0, 0xaf, 0x68),
                bold: true,
                click: None,
            };
            left_with_prefix = left_segs.iter().collect();
            left_with_prefix.push(&prefix_seg);
            &left_with_prefix[..]
        } else {
            // Borrow as slice of refs
            left_with_prefix = left_segs.iter().collect();
            &left_with_prefix[..]
        };

        // Stamp left segments left-to-right and record click zones
        self.click_zones.clear();
        {
            let mut col: u16 = 0;
            for (i, seg) in left_segs.iter().enumerate() {
                let seg_fg = Color::Rgb(seg.fg.0, seg.fg.1, seg.fg.2);
                let seg_bg = Color::Rgb(seg.bg.0, seg.bg.1, seg.bg.2);
                let seg_attr = if seg.bold { Attr::BOLD } else { Attr::empty() };
                let col_start = col;
                for ch in seg.text.chars() {
                    if col >= cols { break; }
                    self.set_back(col, status_y, Cell { c: ch, fg: seg_fg, bg: seg_bg, attr: seg_attr });
                    col += 1;
                }
                if let Some(ref action) = seg.click {
                    self.click_zones.push(StatusClickZone {
                        col_start,
                        col_end: col,
                        action: action.clone(),
                    });
                }
                // Powerline separator after each segment (except last)
                if i + 1 < left_segs.len() {
                    if col < cols {
                        let next_bg = Color::Rgb(left_segs[i + 1].bg.0, left_segs[i + 1].bg.1, left_segs[i + 1].bg.2);
                        self.set_back(col, status_y, Cell { c: '\u{e0b0}', fg: seg_bg, bg: next_bg, attr: Attr::empty() });
                        col += 1;
                    }
                } else {
                    // Last left segment: separator into gap
                    if col < cols {
                        self.set_back(col, status_y, Cell { c: '\u{e0b0}', fg: seg_bg, bg: gap_bg, attr: Attr::empty() });
                        col += 1;
                    }
                }
            }
        }

        // Stamp right segments right-to-left
        if !status.right.is_empty() {
            let right_segs = &status.right;

            // Calculate total width of right segments (text + separators between them + leading reverse separator)
            let mut total_right_width: u16 = 1; // leading reverse separator
            for (i, seg) in right_segs.iter().enumerate() {
                total_right_width += seg.text.len() as u16;
                if i + 1 < right_segs.len() {
                    total_right_width += 1; // separator between segments
                }
            }

            let start_col = cols.saturating_sub(total_right_width);
            let mut col = start_col;

            // Leading reverse separator: fg = first right segment's bg, bg = gap
            if col < cols {
                let first_bg = Color::Rgb(right_segs[0].bg.0, right_segs[0].bg.1, right_segs[0].bg.2);
                self.set_back(col, status_y, Cell { c: '\u{e0b2}', fg: first_bg, bg: gap_bg, attr: Attr::empty() });
                col += 1;
            }

            for (i, seg) in right_segs.iter().enumerate() {
                let seg_fg = Color::Rgb(seg.fg.0, seg.fg.1, seg.fg.2);
                let seg_bg = Color::Rgb(seg.bg.0, seg.bg.1, seg.bg.2);
                let seg_attr = if seg.bold { Attr::BOLD } else { Attr::empty() };
                let col_start = col;
                for ch in seg.text.chars() {
                    if col >= cols { break; }
                    self.set_back(col, status_y, Cell { c: ch, fg: seg_fg, bg: seg_bg, attr: seg_attr });
                    col += 1;
                }
                if let Some(ref action) = seg.click {
                    self.click_zones.push(StatusClickZone {
                        col_start,
                        col_end: col,
                        action: action.clone(),
                    });
                }
                // Separator between right segments
                if i + 1 < right_segs.len() {
                    if col < cols {
                        let next_bg = Color::Rgb(right_segs[i + 1].bg.0, right_segs[i + 1].bg.1, right_segs[i + 1].bg.2);
                        self.set_back(col, status_y, Cell { c: '\u{e0b2}', fg: next_bg, bg: seg_bg, attr: Attr::empty() });
                        col += 1;
                    }
                }
            }
        }

        // Diff front vs back and emit only changed cells
        self.emit_diff()?;

        // Swap buffers
        std::mem::swap(&mut self.front, &mut self.back);

        // Position cursor at the focused pane's cursor location
        let focused_pane = panes.iter().find(|p| p.id == focused);
        if let Some(pane) = focused_pane {
            if pane.cursor_visible && selection.is_none() {
                let cx = pane.x + pane.cursor_x.min(pane.cols.saturating_sub(1));
                let cy = pane.y + pane.cursor_y.min(pane.rows.saturating_sub(1));
                queue!(self.stdout, cursor::MoveTo(cx, cy), cursor::Show)?;
            } else {
                queue!(self.stdout, cursor::Hide)?;
            }
        } else {
            queue!(self.stdout, cursor::Hide)?;
        }

        self.stdout.flush()
    }

    /// Draw a context menu overlay at the given screen position.
    /// `items` is the list of menu labels, `selected` is the highlighted index.
    /// This draws directly on top of the current screen (no buffer swap).
    pub fn render_context_menu(
        &mut self,
        menu_x: u16,
        menu_y: u16,
        items: &[&str],
        selected: usize,
    ) -> io::Result<()> {
        let cols = self.screen_cols;
        let rows = self.screen_rows;

        // Compute menu dimensions
        let width = items.iter().map(|s| s.len()).max().unwrap_or(0) + 4; // 2 padding + 2 border
        let height = items.len() as u16 + 2; // +2 for top/bottom border

        // Clamp position so menu stays on screen
        let mx = if menu_x + width as u16 > cols { cols.saturating_sub(width as u16) } else { menu_x };
        let my = if menu_y + height > rows { rows.saturating_sub(height) } else { menu_y };

        let border_fg = Color::Rgb(120, 160, 220);
        let border_bg = Color::Rgb(30, 30, 30);
        let item_fg = Color::Rgb(200, 200, 200);
        let item_bg = Color::Rgb(30, 30, 30);
        let sel_fg = Color::Rgb(255, 255, 255);
        let sel_bg = Color::Rgb(60, 90, 140);

        // Top border
        queue!(self.stdout, cursor::MoveTo(mx, my))?;
        queue!(self.stdout, SetForegroundColor(to_ct_color(&border_fg)), SetBackgroundColor(to_ct_color(&border_bg)))?;
        queue!(self.stdout, style::Print(BOX_TL))?;
        for _ in 0..width - 2 {
            queue!(self.stdout, style::Print(BOX_H))?;
        }
        queue!(self.stdout, style::Print(BOX_TR))?;

        // Menu items
        for (i, item) in items.iter().enumerate() {
            let y = my + 1 + i as u16;
            queue!(self.stdout, cursor::MoveTo(mx, y))?;
            queue!(self.stdout, SetForegroundColor(to_ct_color(&border_fg)), SetBackgroundColor(to_ct_color(&border_bg)))?;
            queue!(self.stdout, style::Print(BOX_V))?;

            if i == selected {
                queue!(self.stdout, SetForegroundColor(to_ct_color(&sel_fg)), SetBackgroundColor(to_ct_color(&sel_bg)))?;
            } else {
                queue!(self.stdout, SetForegroundColor(to_ct_color(&item_fg)), SetBackgroundColor(to_ct_color(&item_bg)))?;
            }
            let padded = format!(" {:<w$}", item, w = width - 3);
            queue!(self.stdout, style::Print(&padded[..width - 2]))?;

            queue!(self.stdout, SetForegroundColor(to_ct_color(&border_fg)), SetBackgroundColor(to_ct_color(&border_bg)))?;
            queue!(self.stdout, style::Print(BOX_V))?;
        }

        // Bottom border
        let bottom_y = my + 1 + items.len() as u16;
        queue!(self.stdout, cursor::MoveTo(mx, bottom_y))?;
        queue!(self.stdout, SetForegroundColor(to_ct_color(&border_fg)), SetBackgroundColor(to_ct_color(&border_bg)))?;
        queue!(self.stdout, style::Print(BOX_BL))?;
        for _ in 0..width - 2 {
            queue!(self.stdout, style::Print(BOX_H))?;
        }
        queue!(self.stdout, style::Print(BOX_BR))?;

        // Reset and flush
        queue!(self.stdout, SetForegroundColor(CtColor::Reset), SetBackgroundColor(CtColor::Reset), cursor::Hide)?;
        self.stdout.flush()
    }

    /// Draw a centered settings/theme menu overlay.
    /// `items` is the list of theme names, `selected` is the highlighted index,
    /// `active_theme` is the currently applied theme name.
    pub fn render_settings_menu(
        &mut self,
        items: &[String],
        selected: usize,
        active_theme: &str,
    ) -> io::Result<()> {
        let cols = self.screen_cols;
        let rows = self.screen_rows;

        let title = " Settings ";
        let section = " Theme ";
        // Compute widths
        let item_max = items.iter().map(|s| s.len() + 4).max().unwrap_or(20); // " name  ✓ "
        let width = item_max.max(title.len()).max(section.len()) + 4; // border + padding
        let height = items.len() as u16 + 5; // top border + title + section header + items + bottom border

        // Center on screen
        let mx = cols.saturating_sub(width as u16) / 2;
        let my = rows.saturating_sub(height) / 2;

        let border_fg = Color::Rgb(120, 160, 220);
        let border_bg = Color::Rgb(22, 22, 30);
        let title_fg = Color::Rgb(255, 255, 255);
        let title_bg = Color::Rgb(60, 90, 140);
        let section_fg = Color::Rgb(180, 180, 200);
        let section_bg = Color::Rgb(22, 22, 30);
        let item_fg = Color::Rgb(200, 200, 200);
        let item_bg = Color::Rgb(30, 30, 40);
        let sel_fg = Color::Rgb(255, 255, 255);
        let sel_bg = Color::Rgb(60, 90, 140);
        let active_fg = Color::Rgb(120, 220, 120);

        // Top border
        queue!(self.stdout, cursor::MoveTo(mx, my))?;
        queue!(self.stdout, SetForegroundColor(to_ct_color(&border_fg)), SetBackgroundColor(to_ct_color(&border_bg)))?;
        queue!(self.stdout, style::Print(BOX_TL))?;
        for _ in 0..width - 2 { queue!(self.stdout, style::Print(BOX_H))?; }
        queue!(self.stdout, style::Print(BOX_TR))?;

        // Title row
        let title_row = my + 1;
        queue!(self.stdout, cursor::MoveTo(mx, title_row))?;
        queue!(self.stdout, SetForegroundColor(to_ct_color(&border_fg)), SetBackgroundColor(to_ct_color(&border_bg)))?;
        queue!(self.stdout, style::Print(BOX_V))?;
        queue!(self.stdout, SetForegroundColor(to_ct_color(&title_fg)), SetBackgroundColor(to_ct_color(&title_bg)))?;
        queue!(self.stdout, SetAttribute(Attribute::Bold))?;
        let padded_title = format!("{:^w$}", title.trim(), w = width - 2);
        queue!(self.stdout, style::Print(&padded_title))?;
        queue!(self.stdout, SetAttribute(Attribute::Reset))?;
        queue!(self.stdout, SetForegroundColor(to_ct_color(&border_fg)), SetBackgroundColor(to_ct_color(&border_bg)))?;
        queue!(self.stdout, style::Print(BOX_V))?;

        // Section header row
        let section_row = my + 2;
        queue!(self.stdout, cursor::MoveTo(mx, section_row))?;
        queue!(self.stdout, SetForegroundColor(to_ct_color(&border_fg)), SetBackgroundColor(to_ct_color(&border_bg)))?;
        queue!(self.stdout, style::Print(BOX_V))?;
        queue!(self.stdout, SetForegroundColor(to_ct_color(&section_fg)), SetBackgroundColor(to_ct_color(&section_bg)))?;
        let padded_section = format!(" {:<w$}", "Theme", w = width - 3);
        queue!(self.stdout, style::Print(&padded_section[..width - 2]))?;
        queue!(self.stdout, SetForegroundColor(to_ct_color(&border_fg)), SetBackgroundColor(to_ct_color(&border_bg)))?;
        queue!(self.stdout, style::Print(BOX_V))?;

        // Separator
        let sep_row = my + 3;
        queue!(self.stdout, cursor::MoveTo(mx, sep_row))?;
        queue!(self.stdout, SetForegroundColor(to_ct_color(&border_fg)), SetBackgroundColor(to_ct_color(&border_bg)))?;
        queue!(self.stdout, style::Print("├"))?;
        for _ in 0..width - 2 { queue!(self.stdout, style::Print("─"))?; }
        queue!(self.stdout, style::Print("┤"))?;

        // Theme items
        for (i, name) in items.iter().enumerate() {
            let y = my + 4 + i as u16;
            queue!(self.stdout, cursor::MoveTo(mx, y))?;
            queue!(self.stdout, SetForegroundColor(to_ct_color(&border_fg)), SetBackgroundColor(to_ct_color(&border_bg)))?;
            queue!(self.stdout, style::Print(BOX_V))?;

            let is_active = name.eq_ignore_ascii_case(active_theme);
            let marker = if is_active { " ●" } else { "  " };

            if i == selected {
                queue!(self.stdout, SetForegroundColor(to_ct_color(&sel_fg)), SetBackgroundColor(to_ct_color(&sel_bg)))?;
            } else {
                queue!(self.stdout, SetForegroundColor(to_ct_color(&item_fg)), SetBackgroundColor(to_ct_color(&item_bg)))?;
            }
            let label = format!(" {}{}", name, marker);
            let padded = format!("{:<w$}", label, w = width - 2);
            queue!(self.stdout, style::Print(&padded[..width - 2]))?;

            if is_active && i != selected {
                // Show active marker in green
                let marker_col = mx + 1 + name.len() as u16 + 1;
                if marker_col + 2 < mx + width as u16 - 1 {
                    queue!(self.stdout, cursor::MoveTo(marker_col + 1, y))?;
                    queue!(self.stdout, SetForegroundColor(to_ct_color(&active_fg)), SetBackgroundColor(to_ct_color(&item_bg)))?;
                    queue!(self.stdout, style::Print("●"))?;
                }
            }

            queue!(self.stdout, cursor::MoveTo(mx + width as u16 - 1, y))?;
            queue!(self.stdout, SetForegroundColor(to_ct_color(&border_fg)), SetBackgroundColor(to_ct_color(&border_bg)))?;
            queue!(self.stdout, style::Print(BOX_V))?;
        }

        // Bottom border
        let bottom_y = my + 4 + items.len() as u16;
        queue!(self.stdout, cursor::MoveTo(mx, bottom_y))?;
        queue!(self.stdout, SetForegroundColor(to_ct_color(&border_fg)), SetBackgroundColor(to_ct_color(&border_bg)))?;
        queue!(self.stdout, style::Print(BOX_BL))?;
        for _ in 0..width - 2 { queue!(self.stdout, style::Print(BOX_H))?; }
        queue!(self.stdout, style::Print(BOX_BR))?;

        // Reset and flush
        queue!(self.stdout, SetForegroundColor(CtColor::Reset), SetBackgroundColor(CtColor::Reset), cursor::Hide)?;
        self.stdout.flush()
    }

    /// Get the screen dimensions.
    pub fn screen_size(&self) -> (u16, u16) {
        (self.screen_cols, self.screen_rows)
    }

    /// Extract text from the back buffer for a given selection range.
    pub fn extract_selection_text(&self, sel: &Selection) -> String {
        let cols = self.screen_cols;
        let (sx, sy, ex, ey) = sel.normalized();
        let px_end = sel.pane_x + sel.pane_cols.saturating_sub(1);
        let mut result = String::new();

        for y in sy..=ey {
            // Clamp line extents to pane bounds
            let line_start = if y == sy { sx } else { sel.pane_x };
            let line_end = if y == ey { ex } else { px_end };

            for x in line_start..=line_end {
                let idx = y as usize * cols as usize + x as usize;
                if idx < self.front.len() {
                    let c = self.front[idx].c;
                    result.push(if c == '\x00' { ' ' } else { c });
                }
            }

            // Trim trailing spaces from each line
            if y < ey {
                let trimmed = result.trim_end_matches(' ');
                let trimmed_len = trimmed.len();
                result.truncate(trimmed_len);
                result.push('\n');
            }
        }

        // Trim trailing spaces from last line
        let trimmed = result.trim_end_matches(' ');
        trimmed.to_string()
    }

    #[inline]
    fn set_back(&mut self, x: u16, y: u16, cell: Cell) {
        let idx = y as usize * self.screen_cols as usize + x as usize;
        if idx < self.back.len() {
            self.back[idx] = cell;
        }
    }

    fn emit_diff(&mut self) -> io::Result<()> {
        let cols = self.screen_cols as usize;
        let rows = self.screen_rows as usize;

        let mut last_fg = Color::Default;
        let mut last_bg = Color::Default;
        let mut last_attr = Attr::empty();
        let mut need_move;

        // Reset colors at start
        queue!(
            self.stdout,
            SetForegroundColor(CtColor::Reset),
            SetBackgroundColor(CtColor::Reset),
            SetAttribute(Attribute::Reset),
        )?;

        for y in 0..rows {
            need_move = true;
            for x in 0..cols {
                let idx = y * cols + x;
                if idx >= self.back.len() || idx >= self.front.len() {
                    break;
                }

                if self.back[idx] != self.front[idx] {
                    if need_move {
                        queue!(self.stdout, cursor::MoveTo(x as u16, y as u16))?;
                        need_move = false;
                    }

                    let cell = &self.back[idx];

                    // Attribute changes first — Reset clears colors too,
                    // so we must re-emit fg/bg after an attr reset.
                    if cell.attr != last_attr {
                        emit_attr_diff(&mut self.stdout, last_attr, cell.attr)?;
                        last_attr = cell.attr;
                        // Attribute::Reset clears fg/bg, so force re-emit
                        last_fg = Color::Default;
                        last_bg = Color::Default;
                    }
                    if cell.fg != last_fg {
                        queue!(self.stdout, SetForegroundColor(to_ct_color(&cell.fg)))?;
                        last_fg = cell.fg;
                    }
                    if cell.bg != last_bg {
                        queue!(self.stdout, SetBackgroundColor(to_ct_color(&cell.bg)))?;
                        last_bg = cell.bg;
                    }

                    queue!(self.stdout, style::Print(cell.c))?;
                } else {
                    need_move = true;
                }
            }
        }

        // Reset after drawing
        queue!(
            self.stdout,
            SetForegroundColor(CtColor::Reset),
            SetBackgroundColor(CtColor::Reset),
            SetAttribute(Attribute::Reset),
        )?;

        Ok(())
    }

    pub fn cleanup(&mut self) -> io::Result<()> {
        execute!(
            self.stdout,
            crossterm::event::DisableMouseCapture,
            cursor::Show,
            terminal::LeaveAlternateScreen,
        )?;
        terminal::disable_raw_mode()
    }
}

impl Drop for TtyRenderer {
    fn drop(&mut self) {
        let _ = self.cleanup();
    }
}

fn to_ct_color(color: &Color) -> CtColor {
    match color {
        Color::Default => CtColor::Reset,
        Color::Indexed(i) => CtColor::AnsiValue(*i),
        Color::Rgb(r, g, b) => CtColor::Rgb { r: *r, g: *g, b: *b },
    }
}

fn emit_attr_diff(stdout: &mut io::Stdout, _old: Attr, new: Attr) -> io::Result<()> {
    // Reset then set what's needed — simpler and more reliable than
    // tracking individual attribute transitions.
    queue!(stdout, SetAttribute(Attribute::Reset))?;

    if new.contains(Attr::BOLD) {
        queue!(stdout, SetAttribute(Attribute::Bold))?;
    }
    if new.contains(Attr::DIM) {
        queue!(stdout, SetAttribute(Attribute::Dim))?;
    }
    if new.contains(Attr::ITALIC) {
        queue!(stdout, SetAttribute(Attribute::Italic))?;
    }
    if new.contains(Attr::UNDERLINE) {
        queue!(stdout, SetAttribute(Attribute::Underlined))?;
    }
    if new.contains(Attr::REVERSE) {
        queue!(stdout, SetAttribute(Attribute::Reverse))?;
    }
    if new.contains(Attr::STRIKE) {
        queue!(stdout, SetAttribute(Attribute::CrossedOut))?;
    }

    Ok(())
}

