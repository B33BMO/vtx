use crossterm::{
    cursor, execute, queue,
    style::{self, Attribute, Color as CtColor, SetAttribute, SetBackgroundColor, SetForegroundColor},
    terminal,
};
use std::io::{self, Write};
use vtx_core::cell::{Attr, Cell, Color};
use vtx_core::ipc::PaneRender;
use vtx_core::PaneId;

const BORDER_V: char = '│';
const BORDER_H: char = '─';

/// Screen-coordinate selection range (line-based).
#[derive(Debug, Clone, Copy)]
pub struct Selection {
    /// Start position (where mouse was pressed).
    pub start_x: u16,
    pub start_y: u16,
    /// End position (where mouse currently is / was released).
    pub end_x: u16,
    pub end_y: u16,
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

    /// Check if screen position (x, y) is inside the selection.
    fn contains(&self, x: u16, y: u16) -> bool {
        let (sx, sy, ex, ey) = self.normalized();
        if y < sy || y > ey {
            return false;
        }
        if sy == ey {
            // Single line
            return x >= sx && x <= ex;
        }
        if y == sy {
            return x >= sx;
        }
        if y == ey {
            return x <= ex;
        }
        // Middle lines — fully selected
        true
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

pub struct TtyRenderer {
    stdout: io::Stdout,
    /// What is currently displayed on screen.
    front: Vec<Cell>,
    /// What we want to display next.
    back: Vec<Cell>,
    screen_cols: u16,
    screen_rows: u16,
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

    /// Render panes, borders, and status bar using differential updates.
    pub fn render_frame(
        &mut self,
        panes: &[PaneRender],
        focused: PaneId,
        borders: &[(u16, u16, u16, bool)],
        status: &str,
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

        // Stamp pane content into back buffer
        for pane in panes {
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
        let prefix_indicator = if prefix_active { " [PREFIX]" } else { "" };
        let time = utc_time();
        let left = format!(" {status}{prefix_indicator}");
        let right = format!(" {time} ");
        let total_len = left.len() + right.len();
        let padding = if (cols as usize) > total_len {
            cols as usize - total_len
        } else {
            0
        };
        let full_status = format!("{left}{:padding$}{right}", "", padding = padding);

        let status_fg = Color::Rgb(180, 210, 255);
        let status_bg = Color::Rgb(40, 40, 40);
        for (i, ch) in full_status.chars().enumerate() {
            if i >= cols as usize {
                break;
            }
            self.set_back(i as u16, status_y, Cell {
                c: ch,
                fg: status_fg,
                bg: status_bg,
                attr: Attr::empty(),
            });
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

    /// Extract text from the back buffer for a given selection range.
    pub fn extract_selection_text(&self, sel: &Selection) -> String {
        let cols = self.screen_cols;
        let (sx, sy, ex, ey) = sel.normalized();
        let mut result = String::new();

        for y in sy..=ey {
            let line_start = if y == sy { sx } else { 0 };
            let line_end = if y == ey { ex } else { cols.saturating_sub(1) };

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

fn utc_time() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let s = secs % 60;
    let m = (secs / 60) % 60;
    let h = (secs / 3600) % 24;
    format!("{h:02}:{m:02}:{s:02}")
}
