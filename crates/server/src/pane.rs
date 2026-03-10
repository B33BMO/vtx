use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};
use std::io::{Read, Write};
use std::sync::mpsc;
use vtx_core::{PaneId, Result, VtxError};
use vtx_terminal::VtParser;

use crate::widget;

/// The backend driving a pane — either a real PTY or a widget.
enum PaneBackend {
    Pty {
        master: Box<dyn MasterPty + Send>,
        writer: Box<dyn Write + Send>,
        pty_rx: mpsc::Receiver<Vec<u8>>,
    },
    Widget {
        widget: Box<dyn widget::Widget>,
        cols: u16,
        rows: u16,
    },
}

/// A single terminal pane backed by a PTY or a widget.
pub struct Pane {
    pub id: PaneId,
    pub parser: VtParser,
    backend: PaneBackend,
    /// Whether the child process has exited (reader thread finished).
    pub dead: bool,
}

impl Pane {
    pub fn spawn(id: PaneId, cols: u16, rows: u16, shell: &str) -> Result<Self> {
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| VtxError::Pty(e.to_string()))?;

        let mut cmd = CommandBuilder::new(shell);
        cmd.env("TERM", "xterm-256color");

        pair.slave
            .spawn_command(cmd)
            .map_err(|e| VtxError::Pty(e.to_string()))?;

        drop(pair.slave);

        let mut reader = pair
            .master
            .try_clone_reader()
            .map_err(|e| VtxError::Pty(e.to_string()))?;
        let writer = pair
            .master
            .take_writer()
            .map_err(|e| VtxError::Pty(e.to_string()))?;

        // Spawn a dedicated thread to read from the PTY.
        // This avoids the non-blocking fd problem entirely — the thread
        // does blocking reads and sends chunks over a channel.
        let (pty_tx, pty_rx) = mpsc::channel();
        std::thread::Builder::new()
            .name(format!("vtx-pty-{}", id.0))
            .spawn(move || {
                let mut buf = [0u8; 8192];
                loop {
                    match reader.read(&mut buf) {
                        Ok(0) => break, // EOF — child exited
                        Ok(n) => {
                            if pty_tx.send(buf[..n].to_vec()).is_err() {
                                break; // Receiver dropped
                            }
                        }
                        Err(_) => break,
                    }
                }
            })
            .map_err(|e| VtxError::Pty(e.to_string()))?;

        Ok(Pane {
            id,
            parser: VtParser::new(cols, rows),
            backend: PaneBackend::Pty {
                master: pair.master,
                writer,
                pty_rx,
            },
            dead: false,
        })
    }

    /// Spawn a new pane running an SSH connection.
    pub fn spawn_ssh(
        id: PaneId,
        cols: u16,
        rows: u16,
        host: &str,
        user: Option<&str>,
        port: Option<u16>,
    ) -> Result<Self> {
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| VtxError::Pty(e.to_string()))?;

        let mut cmd = CommandBuilder::new("ssh");
        if let Some(port) = port {
            cmd.arg("-p");
            cmd.arg(port.to_string());
        }
        let destination = match user {
            Some(u) => format!("{u}@{host}"),
            None => host.to_string(),
        };
        cmd.arg(destination);
        cmd.env("TERM", "xterm-256color");

        pair.slave
            .spawn_command(cmd)
            .map_err(|e| VtxError::Pty(e.to_string()))?;

        drop(pair.slave);

        let mut reader = pair
            .master
            .try_clone_reader()
            .map_err(|e| VtxError::Pty(e.to_string()))?;
        let writer = pair
            .master
            .take_writer()
            .map_err(|e| VtxError::Pty(e.to_string()))?;

        let (pty_tx, pty_rx) = mpsc::channel();
        std::thread::Builder::new()
            .name(format!("vtx-pty-{}", id.0))
            .spawn(move || {
                let mut buf = [0u8; 8192];
                loop {
                    match reader.read(&mut buf) {
                        Ok(0) => break,
                        Ok(n) => {
                            if pty_tx.send(buf[..n].to_vec()).is_err() {
                                break;
                            }
                        }
                        Err(_) => break,
                    }
                }
            })
            .map_err(|e| VtxError::Pty(e.to_string()))?;

        Ok(Pane {
            id,
            parser: VtParser::new(cols, rows),
            backend: PaneBackend::Pty {
                master: pair.master,
                writer,
                pty_rx,
            },
            dead: false,
        })
    }

    /// Spawn a widget pane (no PTY).
    pub fn spawn_widget(id: PaneId, cols: u16, rows: u16, widget_type: &str) -> Result<Self> {
        let w = widget::create_widget(widget_type).ok_or_else(|| {
            VtxError::Other(format!("Unknown widget type: {widget_type}"))
        })?;
        Ok(Pane {
            id,
            parser: VtParser::new(cols, rows),
            backend: PaneBackend::Widget {
                widget: w,
                cols,
                rows,
            },
            dead: false,
        })
    }

    /// Drain all pending PTY output and process it through the VT parser.
    /// For widget panes, calls update + render and writes cells into the grid.
    /// Returns true if any output was processed.
    pub fn drain_output(&mut self) -> bool {
        match &mut self.backend {
            PaneBackend::Pty {
                writer, pty_rx, ..
            } => {
                let mut got_data = false;
                loop {
                    match pty_rx.try_recv() {
                        Ok(data) => {
                            self.parser.process(&data);
                            got_data = true;
                        }
                        Err(mpsc::TryRecvError::Empty) => break,
                        Err(mpsc::TryRecvError::Disconnected) => {
                            self.dead = true;
                            break;
                        }
                    }
                }

                // Drain any pending responses (e.g. cursor position reports) back to the PTY
                for response in self.parser.grid.pending_responses.drain(..) {
                    let _ = writer.write_all(&response);
                }
                let _ = writer.flush();

                got_data
            }
            PaneBackend::Widget {
                widget, cols, rows, ..
            } => {
                widget.update();
                let cells = widget.render(*cols, *rows);
                // Write the rendered cells directly into the parser grid
                let grid = &mut self.parser.grid;
                for (y, row) in cells.iter().enumerate() {
                    if y >= grid.rows as usize {
                        break;
                    }
                    for (x, cell) in row.iter().enumerate() {
                        if x >= grid.cols as usize {
                            break;
                        }
                        *grid.cell_mut(x as u16, y as u16) = cell.clone();
                    }
                }
                true // always "new" data for widgets
            }
        }
    }

    /// Write input to the PTY (keystrokes from user).
    /// Widget panes ignore input.
    pub fn write_input(&mut self, data: &[u8]) -> std::io::Result<()> {
        match &mut self.backend {
            PaneBackend::Pty { writer, .. } => {
                writer.write_all(data)?;
                writer.flush()
            }
            PaneBackend::Widget { .. } => Ok(()), // widgets don't accept input
        }
    }

    /// Resize the pane's PTY and grid.
    pub fn resize(&mut self, cols: u16, rows: u16) -> Result<()> {
        match &mut self.backend {
            PaneBackend::Pty { master, .. } => {
                master
                    .resize(PtySize {
                        rows,
                        cols,
                        pixel_width: 0,
                        pixel_height: 0,
                    })
                    .map_err(|e| VtxError::Pty(e.to_string()))?;
                self.parser.resize(cols, rows);
            }
            PaneBackend::Widget {
                widget: _,
                cols: c,
                rows: r,
            } => {
                *c = cols;
                *r = rows;
                self.parser.resize(cols, rows);
            }
        }
        Ok(())
    }
}
