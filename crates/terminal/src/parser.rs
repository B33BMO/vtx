use tracing::trace;
use vte::{Params, Perform};

use crate::grid::Grid;

/// Wraps a `Grid` and processes VT escape sequences via the `vte` crate.
pub struct VtParser {
    pub grid: Grid,
    state: vte::Parser,
}

impl VtParser {
    pub fn new(cols: u16, rows: u16) -> Self {
        VtParser {
            grid: Grid::new(cols, rows),
            state: vte::Parser::new(),
        }
    }

    /// Feed raw bytes from a PTY into the parser.
    pub fn process(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            self.state.advance(&mut self.grid, byte);
        }
    }

    pub fn resize(&mut self, cols: u16, rows: u16) {
        self.grid.resize(cols, rows);
    }
}

/// Implement `vte::Perform` on Grid so the parser can drive it directly.
impl Perform for Grid {
    fn print(&mut self, c: char) {
        self.put_char(c);
    }

    fn execute(&mut self, byte: u8) {
        match byte {
            b'\r' => self.cursor_x = 0,
            b'\n' | 0x0b | 0x0c => self.newline(),
            0x08 => self.cursor_x = self.cursor_x.saturating_sub(1),
            b'\t' => self.advance_to_tab_stop(),
            0x07 => { /* bell */ }
            _ => trace!("unhandled execute byte: {byte:#x}"),
        }
    }

    fn hook(&mut self, _params: &Params, _intermediates: &[u8], _ignore: bool, _action: char) {}
    fn put(&mut self, _byte: u8) {}
    fn unhook(&mut self) {}

    fn osc_dispatch(&mut self, params: &[&[u8]], _bell_terminated: bool) {
        if params.is_empty() {
            return;
        }
        match params[0] {
            b"0" | b"2" => {
                if let Some(title) = params.get(1) {
                    self.title = String::from_utf8_lossy(title).to_string();
                }
            }
            b"1" => { /* icon name */ }
            _ => trace!("unhandled OSC: {:?}", params.first()),
        }
    }

    fn csi_dispatch(&mut self, params: &Params, intermediates: &[u8], _ignore: bool, action: char) {
        let mut params_iter = params.iter();
        let first = params_iter
            .next()
            .and_then(|p| p.first().copied())
            .unwrap_or(0);
        let second = params_iter
            .next()
            .and_then(|p| p.first().copied())
            .unwrap_or(0);

        // DEC private modes: intermediates contains '?'
        let is_private = intermediates.contains(&b'?');

        if is_private {
            match action {
                'h' => {
                    // DEC Private Mode Set
                    for param in params.iter() {
                        match param.first().copied().unwrap_or(0) {
                            1 => self.application_cursor_keys = true,
                            7 => self.auto_wrap = true,
                            25 => self.cursor_visible = true,
                            1049 => self.enter_alt_screen(),
                            1047 => self.enter_alt_screen(),
                            1048 => self.save_cursor(),
                            2004 => self.bracketed_paste = true,
                            _ => {}
                        }
                    }
                    return;
                }
                'l' => {
                    // DEC Private Mode Reset
                    for param in params.iter() {
                        match param.first().copied().unwrap_or(0) {
                            1 => self.application_cursor_keys = false,
                            7 => self.auto_wrap = false,
                            25 => self.cursor_visible = false,
                            1049 => self.leave_alt_screen(),
                            1047 => self.leave_alt_screen(),
                            1048 => self.restore_cursor(),
                            2004 => self.bracketed_paste = false,
                            _ => {}
                        }
                    }
                    return;
                }
                _ => {
                    trace!("unhandled private CSI: ?{first} {action}");
                    return;
                }
            }
        }

        match action {
            // Cursor Up
            'A' => {
                let n = first.max(1) as u16;
                self.cursor_y = self.cursor_y.saturating_sub(n);
            }
            // Cursor Down
            'B' => {
                let n = first.max(1) as u16;
                self.cursor_y = (self.cursor_y + n).min(self.rows - 1);
            }
            // Cursor Forward
            'C' => {
                let n = first.max(1) as u16;
                self.cursor_x = (self.cursor_x + n).min(self.cols - 1);
            }
            // Cursor Back
            'D' => {
                let n = first.max(1) as u16;
                self.cursor_x = self.cursor_x.saturating_sub(n);
            }
            // Cursor Next Line
            'E' => {
                let n = first.max(1) as u16;
                self.cursor_y = (self.cursor_y + n).min(self.rows - 1);
                self.cursor_x = 0;
            }
            // Cursor Previous Line
            'F' => {
                let n = first.max(1) as u16;
                self.cursor_y = self.cursor_y.saturating_sub(n);
                self.cursor_x = 0;
            }
            // Cursor Horizontal Absolute
            'G' => {
                let col = first.max(1) as u16 - 1;
                self.cursor_x = col.min(self.cols - 1);
            }
            // Cursor Position (CUP)
            'H' | 'f' => {
                let row = first.max(1) as u16 - 1;
                let col = second.max(1) as u16 - 1;
                self.cursor_y = row.min(self.rows - 1);
                self.cursor_x = col.min(self.cols - 1);
            }
            // Erase in Display
            'J' => match first {
                0 => self.clear_to_end(),
                1 => self.clear_to_start(),
                2 | 3 => self.clear(),
                _ => {}
            },
            // Erase in Line
            'K' => match first {
                0 => self.clear_to_eol(),
                1 => self.clear_to_start_of_line(),
                2 => self.clear_line(),
                _ => {}
            },
            // Insert Lines
            'L' => {
                let n = first.max(1) as u16;
                self.insert_lines(n);
            }
            // Delete Lines
            'M' => {
                let n = first.max(1) as u16;
                self.delete_lines(n);
            }
            // Delete Characters
            'P' => {
                let n = first.max(1) as u16;
                self.delete_chars(n);
            }
            // Scroll Up
            'S' => {
                let n = first.max(1);
                for _ in 0..n {
                    self.scroll_up_in_region();
                }
            }
            // Scroll Down
            'T' => {
                let n = first.max(1);
                for _ in 0..n {
                    self.scroll_down_in_region();
                }
            }
            // Erase Characters
            'X' => {
                let n = first.max(1) as u16;
                self.erase_chars(n);
            }
            // Insert Characters
            '@' => {
                let n = first.max(1) as u16;
                self.insert_chars(n);
            }
            // Vertical Line Position Absolute
            'd' => {
                let row = first.max(1) as u16 - 1;
                self.cursor_y = row.min(self.rows - 1);
            }
            // Tab clear
            'g' => match first {
                0 => self.clear_tab_stop(),
                3 => self.clear_all_tab_stops(),
                _ => {}
            },
            // Set Mode (non-private)
            'h' => { /* SM — standard modes, rarely used */ }
            // Reset Mode (non-private)
            'l' => { /* RM */ }
            // SGR (Select Graphic Rendition)
            'm' => self.handle_sgr(params),
            // Device Status Report
            'n' => {
                if first == 6 {
                    self.report_cursor_position();
                }
            }
            // Set scroll region (DECSTBM)
            'r' => {
                let top = if first == 0 { 1 } else { first } as u16;
                let bot = if second == 0 { self.rows } else { second } as u16;
                self.set_scroll_region(top - 1, bot - 1);
                self.cursor_x = 0;
                self.cursor_y = 0;
            }
            // Save cursor (ANSI.SYS)
            's' => self.save_cursor(),
            // Restore cursor (ANSI.SYS)
            'u' => self.restore_cursor(),
            _ => trace!("unhandled CSI: {action} params={first},{second}"),
        }
    }

    fn esc_dispatch(&mut self, intermediates: &[u8], _ignore: bool, byte: u8) {
        match (byte, intermediates) {
            // DECSC — Save Cursor
            (b'7', []) => self.save_cursor(),
            // DECRC — Restore Cursor
            (b'8', []) => self.restore_cursor(),
            // RI — Reverse Index
            (b'M', []) => self.reverse_index(),
            // IND — Index (newline)
            (b'D', []) => self.newline(),
            // NEL — Next Line
            (b'E', []) => {
                self.cursor_x = 0;
                self.newline();
            }
            // HTS — Horizontal Tab Set
            (b'H', []) => self.set_tab_stop(),
            // DECKPAM — Application Keypad
            (b'=', []) => { /* keypad mode */ }
            // DECKPNM — Normal Keypad
            (b'>', []) => { /* keypad mode */ }
            // RIS — Full Reset
            (b'c', []) => {
                let cols = self.cols;
                let rows = self.rows;
                *self = Grid::new(cols, rows);
            }
            _ => trace!("unhandled ESC: intermediates={intermediates:?} byte={byte:#x}"),
        }
    }
}

impl Grid {
    fn handle_sgr(&mut self, params: &Params) {
        use vtx_core::cell::{Attr, Color};

        let mut iter = params.iter();

        if params.len() == 0 {
            self.attr = Attr::empty();
            self.fg = Color::Default;
            self.bg = Color::Default;
            return;
        }

        while let Some(param) = iter.next() {
            let code = param.first().copied().unwrap_or(0);
            match code {
                0 => {
                    self.attr = Attr::empty();
                    self.fg = Color::Default;
                    self.bg = Color::Default;
                }
                1 => self.attr |= Attr::BOLD,
                2 => self.attr |= Attr::DIM,
                3 => self.attr |= Attr::ITALIC,
                4 => self.attr |= Attr::UNDERLINE,
                7 => self.attr |= Attr::REVERSE,
                9 => self.attr |= Attr::STRIKE,
                22 => self.attr -= Attr::BOLD | Attr::DIM,
                23 => self.attr -= Attr::ITALIC,
                24 => self.attr -= Attr::UNDERLINE,
                27 => self.attr -= Attr::REVERSE,
                29 => self.attr -= Attr::STRIKE,
                30..=37 => self.fg = Color::Indexed((code - 30) as u8),
                39 => self.fg = Color::Default,
                40..=47 => self.bg = Color::Indexed((code - 40) as u8),
                49 => self.bg = Color::Default,
                38 => {
                    if let Some(next) = iter.next() {
                        match next.first().copied().unwrap_or(0) {
                            5 => {
                                if let Some(idx) = iter.next() {
                                    self.fg =
                                        Color::Indexed(idx.first().copied().unwrap_or(0) as u8);
                                }
                            }
                            2 => {
                                let r =
                                    iter.next().and_then(|p| p.first().copied()).unwrap_or(0);
                                let g =
                                    iter.next().and_then(|p| p.first().copied()).unwrap_or(0);
                                let b =
                                    iter.next().and_then(|p| p.first().copied()).unwrap_or(0);
                                self.fg = Color::Rgb(r as u8, g as u8, b as u8);
                            }
                            _ => {}
                        }
                    }
                }
                48 => {
                    if let Some(next) = iter.next() {
                        match next.first().copied().unwrap_or(0) {
                            5 => {
                                if let Some(idx) = iter.next() {
                                    self.bg =
                                        Color::Indexed(idx.first().copied().unwrap_or(0) as u8);
                                }
                            }
                            2 => {
                                let r =
                                    iter.next().and_then(|p| p.first().copied()).unwrap_or(0);
                                let g =
                                    iter.next().and_then(|p| p.first().copied()).unwrap_or(0);
                                let b =
                                    iter.next().and_then(|p| p.first().copied()).unwrap_or(0);
                                self.bg = Color::Rgb(r as u8, g as u8, b as u8);
                            }
                            _ => {}
                        }
                    }
                }
                90..=97 => self.fg = Color::Indexed((code - 90 + 8) as u8),
                100..=107 => self.bg = Color::Indexed((code - 100 + 8) as u8),
                _ => {}
            }
        }
    }
}
