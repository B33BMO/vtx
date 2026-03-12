use vtx_core::cell::{Attr, Cell, Color};

/// A fixed-size grid of terminal cells with a cursor.
#[derive(Debug, Clone)]
pub struct Grid {
    pub cols: u16,
    pub rows: u16,
    cells: Vec<Cell>,
    pub cursor_x: u16,
    pub cursor_y: u16,
    /// Current drawing attributes
    pub attr: Attr,
    pub fg: Color,
    pub bg: Color,
    /// Track if any cell changed since last `clear_dirty()`
    dirty: bool,

    // Cursor save/restore (DECSC/DECRC)
    saved_cursor_x: u16,
    saved_cursor_y: u16,
    saved_fg: Color,
    saved_bg: Color,
    saved_attr: Attr,

    // Scroll region (DECSTBM)
    pub scroll_top: u16,
    pub scroll_bottom: u16,

    // Mode flags
    pub cursor_visible: bool,
    pub auto_wrap: bool,
    pub bracketed_paste: bool,
    pub origin_mode: bool,
    pub application_cursor_keys: bool,

    // Alternate screen buffer
    alt_cells: Vec<Cell>,
    alt_cursor_x: u16,
    alt_cursor_y: u16,
    pub using_alt_screen: bool,

    // Tab stops
    tab_stops: Vec<bool>,

    // Window title (set via OSC)
    pub title: String,

    // Pending responses to write back to PTY (e.g. cursor position report)
    pub pending_responses: Vec<Vec<u8>>,

    // Scrollback buffer — stores lines that scrolled off the top
    scrollback: Vec<Vec<Cell>>,
    pub scrollback_limit: usize,
}

impl Grid {
    pub fn new(cols: u16, rows: u16) -> Self {
        let size = cols as usize * rows as usize;
        Grid {
            cols,
            rows,
            cells: vec![Cell::default(); size],
            cursor_x: 0,
            cursor_y: 0,
            attr: Attr::empty(),
            fg: Color::Default,
            bg: Color::Default,
            dirty: true,
            saved_cursor_x: 0,
            saved_cursor_y: 0,
            saved_fg: Color::Default,
            saved_bg: Color::Default,
            saved_attr: Attr::empty(),
            scroll_top: 0,
            scroll_bottom: rows.saturating_sub(1),
            cursor_visible: true,
            auto_wrap: true,
            bracketed_paste: false,
            origin_mode: false,
            application_cursor_keys: false,
            alt_cells: vec![Cell::default(); size],
            alt_cursor_x: 0,
            alt_cursor_y: 0,
            using_alt_screen: false,
            tab_stops: Self::default_tab_stops(cols),
            title: String::new(),
            pending_responses: Vec::new(),
            scrollback: Vec::new(),
            scrollback_limit: 100_000,
        }
    }

    fn default_tab_stops(cols: u16) -> Vec<bool> {
        (0..cols).map(|i| i % 8 == 0).collect()
    }

    pub fn resize(&mut self, cols: u16, rows: u16) {
        let mut new_cells = vec![Cell::default(); cols as usize * rows as usize];
        let copy_cols = self.cols.min(cols) as usize;
        let copy_rows = self.rows.min(rows) as usize;

        for y in 0..copy_rows {
            let src_start = y * self.cols as usize;
            let dst_start = y * cols as usize;
            new_cells[dst_start..dst_start + copy_cols]
                .clone_from_slice(&self.cells[src_start..src_start + copy_cols]);
        }

        self.cols = cols;
        self.rows = rows;
        self.cells = new_cells;
        self.alt_cells = vec![Cell::default(); cols as usize * rows as usize];
        self.cursor_x = self.cursor_x.min(cols.saturating_sub(1));
        self.cursor_y = self.cursor_y.min(rows.saturating_sub(1));
        self.scroll_top = 0;
        self.scroll_bottom = rows.saturating_sub(1);
        self.tab_stops = Self::default_tab_stops(cols);
        self.dirty = true;
    }

    #[inline]
    fn idx(&self, x: u16, y: u16) -> usize {
        y as usize * self.cols as usize + x as usize
    }

    pub fn cell(&self, x: u16, y: u16) -> &Cell {
        &self.cells[self.idx(x, y)]
    }

    pub fn cell_mut(&mut self, x: u16, y: u16) -> &mut Cell {
        self.dirty = true;
        let idx = self.idx(x, y);
        &mut self.cells[idx]
    }

    /// Write a character at the cursor position and advance.
    pub fn put_char(&mut self, c: char) {
        if self.cursor_x >= self.cols {
            if self.auto_wrap {
                self.cursor_x = 0;
                self.newline();
            } else {
                self.cursor_x = self.cols - 1;
            }
        }

        let fg = self.fg;
        let bg = self.bg;
        let attr = self.attr;
        let cell = self.cell_mut(self.cursor_x, self.cursor_y);
        cell.c = c;
        cell.fg = fg;
        cell.bg = bg;
        cell.attr = attr;

        self.cursor_x += 1;
    }

    /// Move cursor to the next line, scrolling if at the bottom of the scroll region.
    pub fn newline(&mut self) {
        if self.cursor_y == self.scroll_bottom {
            self.scroll_up_in_region();
        } else if self.cursor_y < self.rows - 1 {
            self.cursor_y += 1;
        }
    }

    /// Reverse index — move cursor up, scrolling down if at top of scroll region.
    pub fn reverse_index(&mut self) {
        if self.cursor_y == self.scroll_top {
            self.scroll_down_in_region();
        } else if self.cursor_y > 0 {
            self.cursor_y -= 1;
        }
    }

    /// Scroll the scroll region up by one line.
    pub fn scroll_up_in_region(&mut self) {
        let cols = self.cols as usize;
        let top = self.scroll_top as usize;
        let bottom = self.scroll_bottom as usize;

        // If scrolling the full screen (top == 0), save the top line to scrollback
        if top == 0 && !self.using_alt_screen {
            let top_row: Vec<Cell> = (0..cols)
                .map(|x| self.cells[x].clone())
                .collect();
            self.scrollback.push(top_row);
            if self.scrollback.len() > self.scrollback_limit {
                self.scrollback.remove(0);
            }
        }

        // Shift rows up within the region
        for y in top..bottom {
            let src_start = (y + 1) * cols;
            let dst_start = y * cols;
            for x in 0..cols {
                self.cells[dst_start + x] = self.cells[src_start + x].clone();
            }
        }
        // Clear the bottom row
        let bot_start = bottom * cols;
        for x in 0..cols {
            self.cells[bot_start + x] = Cell::default();
        }
        self.dirty = true;
    }

    /// Scroll the scroll region down by one line.
    pub fn scroll_down_in_region(&mut self) {
        let cols = self.cols as usize;
        let top = self.scroll_top as usize;
        let bottom = self.scroll_bottom as usize;

        // Shift rows down within the region
        for y in (top + 1..=bottom).rev() {
            let src_start = (y - 1) * cols;
            let dst_start = y * cols;
            for x in 0..cols {
                self.cells[dst_start + x] = self.cells[src_start + x].clone();
            }
        }
        // Clear the top row
        let top_start = top * cols;
        for x in 0..cols {
            self.cells[top_start + x] = Cell::default();
        }
        self.dirty = true;
    }

    /// Insert n blank lines at cursor row, pushing lines down within scroll region.
    pub fn insert_lines(&mut self, n: u16) {
        let cols = self.cols as usize;
        let y = self.cursor_y as usize;
        let bottom = self.scroll_bottom as usize;

        for _ in 0..n {
            if y > bottom {
                break;
            }
            // Shift rows down
            for row in (y + 1..=bottom).rev() {
                let src = (row - 1) * cols;
                let dst = row * cols;
                for x in 0..cols {
                    self.cells[dst + x] = self.cells[src + x].clone();
                }
            }
            // Clear the inserted row
            let start = y * cols;
            for x in 0..cols {
                self.cells[start + x] = Cell::default();
            }
        }
        self.dirty = true;
    }

    /// Delete n lines at cursor row, shifting lines up within scroll region.
    pub fn delete_lines(&mut self, n: u16) {
        let cols = self.cols as usize;
        let y = self.cursor_y as usize;
        let bottom = self.scroll_bottom as usize;

        for _ in 0..n {
            if y > bottom {
                break;
            }
            // Shift rows up
            for row in y..bottom {
                let src = (row + 1) * cols;
                let dst = row * cols;
                for x in 0..cols {
                    self.cells[dst + x] = self.cells[src + x].clone();
                }
            }
            // Clear the bottom row
            let start = bottom * cols;
            for x in 0..cols {
                self.cells[start + x] = Cell::default();
            }
        }
        self.dirty = true;
    }

    /// Insert n blank characters at cursor, shifting chars right.
    pub fn insert_chars(&mut self, n: u16) {
        let y = self.cursor_y;
        let cols = self.cols;
        let x = self.cursor_x;

        for _ in 0..n {
            // Shift right
            for col in (x + 1..cols).rev() {
                let prev = self.cell(col - 1, y).clone();
                *self.cell_mut(col, y) = prev;
            }
            *self.cell_mut(x, y) = Cell::default();
        }
    }

    /// Delete n characters at cursor, shifting chars left.
    pub fn delete_chars(&mut self, n: u16) {
        let y = self.cursor_y;
        let cols = self.cols;
        let x = self.cursor_x;

        for _ in 0..n {
            for col in x..cols.saturating_sub(1) {
                let next = self.cell(col + 1, y).clone();
                *self.cell_mut(col, y) = next;
            }
            *self.cell_mut(cols - 1, y) = Cell::default();
        }
    }

    /// Erase n characters starting at cursor (replace with blanks, don't shift).
    pub fn erase_chars(&mut self, n: u16) {
        for i in 0..n {
            let x = self.cursor_x + i;
            if x >= self.cols {
                break;
            }
            *self.cell_mut(x, self.cursor_y) = Cell::default();
        }
    }

    /// Clear from cursor to end of line.
    pub fn clear_to_eol(&mut self) {
        for x in self.cursor_x..self.cols {
            *self.cell_mut(x, self.cursor_y) = Cell::default();
        }
    }

    /// Clear from start of line to cursor (inclusive).
    pub fn clear_to_start_of_line(&mut self) {
        for x in 0..=self.cursor_x.min(self.cols - 1) {
            *self.cell_mut(x, self.cursor_y) = Cell::default();
        }
    }

    /// Clear entire line.
    pub fn clear_line(&mut self) {
        for x in 0..self.cols {
            *self.cell_mut(x, self.cursor_y) = Cell::default();
        }
    }

    /// Clear the entire screen.
    pub fn clear(&mut self) {
        self.cells.fill(Cell::default());
        self.cursor_x = 0;
        self.cursor_y = 0;
        self.dirty = true;
    }

    /// Erase from cursor to end of display.
    pub fn clear_to_end(&mut self) {
        self.clear_to_eol();
        for y in (self.cursor_y + 1)..self.rows {
            for x in 0..self.cols {
                *self.cell_mut(x, y) = Cell::default();
            }
        }
    }

    /// Erase from start of display to cursor (inclusive).
    pub fn clear_to_start(&mut self) {
        for y in 0..self.cursor_y {
            for x in 0..self.cols {
                *self.cell_mut(x, y) = Cell::default();
            }
        }
        self.clear_to_start_of_line();
    }

    // --- Cursor save/restore ---

    pub fn save_cursor(&mut self) {
        self.saved_cursor_x = self.cursor_x;
        self.saved_cursor_y = self.cursor_y;
        self.saved_fg = self.fg;
        self.saved_bg = self.bg;
        self.saved_attr = self.attr;
    }

    pub fn restore_cursor(&mut self) {
        self.cursor_x = self.saved_cursor_x.min(self.cols.saturating_sub(1));
        self.cursor_y = self.saved_cursor_y.min(self.rows.saturating_sub(1));
        self.fg = self.saved_fg;
        self.bg = self.saved_bg;
        self.attr = self.saved_attr;
    }

    // --- Scroll region ---

    pub fn set_scroll_region(&mut self, top: u16, bottom: u16) {
        let top = top.min(self.rows.saturating_sub(1));
        let bottom = bottom.min(self.rows.saturating_sub(1));
        if top < bottom {
            self.scroll_top = top;
            self.scroll_bottom = bottom;
        }
    }

    // --- Alternate screen buffer ---

    pub fn enter_alt_screen(&mut self) {
        if self.using_alt_screen {
            return;
        }
        self.save_cursor();
        // Swap to alt buffer
        std::mem::swap(&mut self.cells, &mut self.alt_cells);
        self.alt_cursor_x = self.cursor_x;
        self.alt_cursor_y = self.cursor_y;
        // Clear the alt screen
        self.cells.fill(Cell::default());
        self.cursor_x = 0;
        self.cursor_y = 0;
        self.using_alt_screen = true;
        self.dirty = true;
    }

    pub fn leave_alt_screen(&mut self) {
        if !self.using_alt_screen {
            return;
        }
        // Swap back to primary buffer
        std::mem::swap(&mut self.cells, &mut self.alt_cells);
        self.cursor_x = self.alt_cursor_x;
        self.cursor_y = self.alt_cursor_y;
        self.restore_cursor();
        self.using_alt_screen = false;
        self.dirty = true;
    }

    // --- Tab stops ---

    pub fn advance_to_tab_stop(&mut self) {
        let mut x = self.cursor_x + 1;
        while (x as usize) < self.tab_stops.len() && !self.tab_stops[x as usize] {
            x += 1;
        }
        self.cursor_x = x.min(self.cols - 1);
    }

    pub fn set_tab_stop(&mut self) {
        if (self.cursor_x as usize) < self.tab_stops.len() {
            self.tab_stops[self.cursor_x as usize] = true;
        }
    }

    pub fn clear_tab_stop(&mut self) {
        if (self.cursor_x as usize) < self.tab_stops.len() {
            self.tab_stops[self.cursor_x as usize] = false;
        }
    }

    pub fn clear_all_tab_stops(&mut self) {
        self.tab_stops.fill(false);
    }

    // --- Cursor position report ---

    pub fn report_cursor_position(&mut self) {
        // ESC [ row ; col R (1-based)
        let response = format!("\x1b[{};{}R", self.cursor_y + 1, self.cursor_x + 1);
        self.pending_responses.push(response.into_bytes());
    }

    // --- Query helpers ---

    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    pub fn clear_dirty(&mut self) {
        self.dirty = false;
    }

    /// Extract content as rows of chars (for simple rendering).
    pub fn content_chars(&self) -> Vec<Vec<char>> {
        (0..self.rows)
            .map(|y| (0..self.cols).map(|x| self.cell(x, y).c).collect())
            .collect()
    }

    /// Extract content as rows of full Cells (for rich rendering with colors/attrs).
    pub fn content_cells(&self) -> Vec<Vec<Cell>> {
        (0..self.rows)
            .map(|y| {
                (0..self.cols)
                    .map(|x| self.cell(x, y).clone())
                    .collect()
            })
            .collect()
    }

    /// Get content with scrollback offset. offset=0 is current view,
    /// positive values scroll back into history.
    ///
    /// Think of it as a virtual buffer:
    ///   [ scrollback line 0 ]     <- oldest
    ///   [ scrollback line 1 ]
    ///   [ ...                ]
    ///   [ scrollback line N  ]    <- most recent scrollback
    ///   [ screen row 0       ]    <- current visible screen
    ///   [ screen row 1       ]
    ///   [ ...                ]
    ///   [ screen row M       ]    <- bottom of screen
    ///
    /// offset=0 means the viewport is at the bottom (showing the screen).
    /// offset=5 shifts the viewport up by 5 lines.
    pub fn content_cells_scrolled(&self, offset: usize) -> Vec<Vec<Cell>> {
        if offset == 0 {
            return self.content_cells();
        }

        let view_rows = self.rows as usize;
        let cols = self.cols as usize;
        let sb_len = self.scrollback.len();

        // Total virtual lines = scrollback + screen
        let total = sb_len + view_rows;
        // The viewport bottom is at (total - offset), clamped
        let offset = offset.min(sb_len); // can't scroll past all scrollback
        let viewport_end = total - offset;
        let viewport_start = viewport_end.saturating_sub(view_rows);

        let mut result = Vec::with_capacity(view_rows);

        for i in viewport_start..viewport_end {
            if i < sb_len {
                // This line comes from scrollback
                let mut row = self.scrollback[i].clone();
                row.resize(cols, Cell::default());
                result.push(row);
            } else {
                // This line comes from the current screen
                let screen_row = (i - sb_len) as u16;
                if screen_row < self.rows {
                    result.push(
                        (0..self.cols)
                            .map(|x| self.cell(x, screen_row).clone())
                            .collect(),
                    );
                } else {
                    result.push(vec![Cell::default(); cols]);
                }
            }
        }

        result
    }

    /// How many scrollback lines are available.
    pub fn scrollback_len(&self) -> usize {
        self.scrollback.len()
    }

    /// Search the scrollback + screen buffer for `query`, returning line indices
    /// (in the virtual buffer coordinate space) where matches occur.
    /// Results are ordered from bottom (most recent) to top (oldest).
    pub fn search(&self, query: &str) -> Vec<usize> {
        if query.is_empty() {
            return Vec::new();
        }

        let query_lower = query.to_lowercase();
        let sb_len = self.scrollback.len();
        let total = sb_len + self.rows as usize;
        let mut results = Vec::new();

        // Search backwards from the bottom (most recent first)
        for i in (0..total).rev() {
            let line_text: String = if i < sb_len {
                // Line from scrollback
                self.scrollback[i].iter().map(|c| c.c).collect()
            } else {
                // Line from current screen
                let screen_row = (i - sb_len) as u16;
                (0..self.cols).map(|x| self.cell(x, screen_row).c).collect()
            };

            if line_text.to_lowercase().contains(&query_lower) {
                results.push(i);
            }
        }

        results
    }
}
