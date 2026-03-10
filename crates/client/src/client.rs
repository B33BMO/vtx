use crossterm::event::{
    self, Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tracing::{error, info};

use vtx_core::ipc::{ClientMsg, Direction, ServerMsg};
use vtx_renderer_tty::{Selection, TtyRenderer};

/// Client-side input action after processing keys.
enum InputAction {
    Send(ClientMsg),
    Detach,
    ScrollUp,
    ScrollDown,
    None,
}

/// Events from the blocking input reader thread.
enum TermEvent {
    Key(KeyEvent),
    Mouse(MouseEvent),
    Resize(u16, u16),
}

pub struct VtxClient {
    socket_path: String,
}

impl VtxClient {
    pub fn new(socket_path: String) -> Self {
        VtxClient { socket_path }
    }

    pub async fn run_attach(&self, session_name: Option<String>) -> vtx_core::Result<()> {
        let stream = UnixStream::connect(&self.socket_path).await?;
        let (reader, mut writer) = stream.into_split();
        let mut reader = BufReader::new(reader);

        send_msg(&mut writer, &ClientMsg::NewSession { name: session_name }).await?;

        let mut line = String::new();
        reader.read_line(&mut line).await?;
        let response: ServerMsg = serde_json::from_str(line.trim())?;

        match &response {
            ServerMsg::SessionReady { session, cols, rows } => {
                info!("Attached to session {session} ({cols}x{rows})");
            }
            ServerMsg::Error { msg } => {
                return Err(vtx_core::VtxError::Other(msg.clone()));
            }
            _ => {}
        }
        line.clear();

        // Send initial resize
        if let Ok((cols, rows)) = crossterm::terminal::size() {
            send_msg(&mut writer, &ClientMsg::Resize { cols, rows }).await?;
            reader.read_line(&mut line).await?;
            line.clear();
        }

        let mut renderer = TtyRenderer::new()?;
        let mut prefix_active = false;
        let mut scroll_offset: i32 = 0;

        // Mouse selection state
        let mut selection: Option<Selection> = None;
        let mut mouse_dragging = false;

        // Store last render frame for re-rendering with selection
        let mut last_frame: Option<(Vec<vtx_core::ipc::PaneRender>, vtx_core::PaneId, Vec<(u16, u16, u16, bool)>, String, u16)> = None;

        // Channel for all terminal events
        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<TermEvent>(256);

        // Channel for server messages — read_line runs in its own task
        // because it is NOT cancel-safe in tokio::select!
        let (server_msg_tx, mut server_msg_rx) = tokio::sync::mpsc::channel::<ServerMsg>(64);

        // Spawn dedicated server message reader task (never cancelled)
        let server_reader_handle = tokio::spawn(async move {
            let mut line = String::new();
            loop {
                line.clear();
                match reader.read_line(&mut line).await {
                    Ok(0) => break,
                    Ok(_) => {
                        let trimmed = line.trim();
                        if trimmed.is_empty() {
                            continue;
                        }
                        match serde_json::from_str::<ServerMsg>(trimmed) {
                            Ok(msg) => {
                                if server_msg_tx.send(msg).await.is_err() {
                                    break;
                                }
                            }
                            Err(e) => {
                                error!("Bad server msg: {e}");
                            }
                        }
                    }
                    Err(e) => {
                        error!("Read error: {e}");
                        break;
                    }
                }
            }
        });

        // Spawn blocking input reader with mouse capture
        let input_handle = {
            let tx = event_tx.clone();
            tokio::task::spawn_blocking(move || {
                let _ = crossterm::execute!(
                    std::io::stdout(),
                    crossterm::event::EnableMouseCapture
                );

                loop {
                    if event::poll(Duration::from_millis(8)).unwrap_or(false) {
                        match event::read() {
                            Ok(Event::Key(ke)) => {
                                if tx.blocking_send(TermEvent::Key(ke)).is_err() {
                                    break;
                                }
                            }
                            Ok(Event::Mouse(me)) => {
                                if tx.blocking_send(TermEvent::Mouse(me)).is_err() {
                                    break;
                                }
                            }
                            Ok(Event::Resize(c, r)) => {
                                let _ = tx.blocking_send(TermEvent::Resize(c, r));
                            }
                            Ok(_) => {}
                            Err(_) => break,
                        }
                    }
                }

                let _ = crossterm::execute!(
                    std::io::stdout(),
                    crossterm::event::DisableMouseCapture
                );
            })
        };

        // Main loop — only uses cancel-safe channel receives
        loop {
            tokio::select! {
                // Server messages (from dedicated reader task)
                result = server_msg_rx.recv() => {
                    match result {
                        Some(ServerMsg::Render { panes, focused, borders, status, total_rows }) => {
                            let status_display = if scroll_offset > 0 {
                                format!("{status} [SCROLL: +{}]", scroll_offset)
                            } else {
                                status
                            };
                            if let Err(e) = renderer.render_frame(
                                &panes, focused, &borders, &status_display, total_rows, prefix_active,
                                selection.as_ref(),
                            ) {
                                error!("Render error: {e}");
                            }
                            last_frame = Some((panes, focused, borders, status_display, total_rows));
                        }
                        Some(ServerMsg::Detached) => break,
                        Some(ServerMsg::Error { msg }) => error!("Server error: {msg}"),
                        Some(_) => {}
                        None => break, // reader task ended
                    }
                }

                // Terminal events (keyboard, mouse, resize)
                Some(term_event) = event_rx.recv() => {
                    match term_event {
                        TermEvent::Mouse(me) => {
                            match me.kind {
                                MouseEventKind::ScrollUp => {
                                    // Clear selection on scroll
                                    selection = None;
                                    scroll_offset += 3;
                                    send_msg(&mut writer, &ClientMsg::ScrollBack { offset: scroll_offset }).await?;
                                }
                                MouseEventKind::ScrollDown => {
                                    selection = None;
                                    scroll_offset = (scroll_offset - 3).max(0);
                                    send_msg(&mut writer, &ClientMsg::ScrollBack { offset: scroll_offset }).await?;
                                }
                                MouseEventKind::Down(MouseButton::Left) => {
                                    // Start new selection
                                    selection = Some(Selection {
                                        start_x: me.column,
                                        start_y: me.row,
                                        end_x: me.column,
                                        end_y: me.row,
                                    });
                                    mouse_dragging = true;

                                    // Re-render with selection
                                    if let Some((ref panes, focused, ref borders, ref status, total_rows)) = last_frame {
                                        let _ = renderer.render_frame(
                                            panes, focused, borders, status, total_rows, prefix_active,
                                            selection.as_ref(),
                                        );
                                    }
                                }
                                MouseEventKind::Drag(MouseButton::Left) => {
                                    if mouse_dragging {
                                        if let Some(ref mut sel) = selection {
                                            sel.end_x = me.column;
                                            sel.end_y = me.row;
                                        }

                                        // Re-render with updated selection
                                        if let Some((ref panes, focused, ref borders, ref status, total_rows)) = last_frame {
                                            let _ = renderer.render_frame(
                                                panes, focused, borders, status, total_rows, prefix_active,
                                                selection.as_ref(),
                                            );
                                        }
                                    }
                                }
                                MouseEventKind::Up(MouseButton::Left) => {
                                    if mouse_dragging {
                                        mouse_dragging = false;

                                        if let Some(ref mut sel) = selection {
                                            sel.end_x = me.column;
                                            sel.end_y = me.row;

                                            // If start == end (just a click, no drag), focus clicked pane
                                            if sel.start_x == sel.end_x && sel.start_y == sel.end_y {
                                                selection = None;
                                                // Find which pane was clicked and focus it
                                                if let Some((ref panes, _, _, _, _)) = last_frame {
                                                    let click_x = me.column;
                                                    let click_y = me.row;
                                                    for pane in panes {
                                                        if click_x >= pane.x
                                                            && click_x < pane.x + pane.cols
                                                            && click_y >= pane.y
                                                            && click_y < pane.y + pane.rows
                                                        {
                                                            send_msg(&mut writer, &ClientMsg::FocusPane { pane: pane.id }).await?;
                                                            break;
                                                        }
                                                    }
                                                }
                                            } else {
                                                // Copy selected text to clipboard via OSC 52
                                                let text = renderer.extract_selection_text(sel);
                                                if !text.is_empty() {
                                                    let b64 = base64_encode(text.as_bytes());
                                                    let osc = format!("\x1b]52;c;{b64}\x07");
                                                    let mut out = std::io::stdout();
                                                    let _ = std::io::Write::write_all(&mut out, osc.as_bytes());
                                                    let _ = std::io::Write::flush(&mut out);
                                                }
                                            }
                                        }
                                    }
                                }
                                _ => {}
                            }
                        }

                        TermEvent::Key(key_event) => {
                            // Any keypress clears selection
                            if selection.is_some() {
                                selection = None;
                                // Re-render without selection
                                if let Some((ref panes, focused, ref borders, ref status, total_rows)) = last_frame {
                                    let _ = renderer.render_frame(
                                        panes, focused, borders, status, total_rows, prefix_active,
                                        None,
                                    );
                                }
                            }

                            // If scrolled and user types normal input, snap back to bottom
                            if scroll_offset > 0 && is_typing_input(&key_event) {
                                scroll_offset = 0;
                                send_msg(&mut writer, &ClientMsg::ScrollBack { offset: 0 }).await?;
                            }

                            let action = process_key(key_event, &mut prefix_active);
                            match action {
                                InputAction::Send(msg) => {
                                    send_msg(&mut writer, &msg).await?;
                                }
                                InputAction::ScrollUp => {
                                    scroll_offset += 3;
                                    send_msg(&mut writer, &ClientMsg::ScrollBack { offset: scroll_offset }).await?;
                                }
                                InputAction::ScrollDown => {
                                    scroll_offset = (scroll_offset - 3).max(0);
                                    send_msg(&mut writer, &ClientMsg::ScrollBack { offset: scroll_offset }).await?;
                                }
                                InputAction::Detach => {
                                    send_msg(&mut writer, &ClientMsg::Detach).await?;
                                    break;
                                }
                                InputAction::None => {}
                            }
                        }

                        TermEvent::Resize(cols, rows) => {
                            selection = None;
                            send_msg(&mut writer, &ClientMsg::Resize { cols, rows }).await?;
                        }
                    }
                }
            }
        }

        drop(renderer);
        input_handle.abort();
        server_reader_handle.abort();
        Ok(())
    }

    pub async fn send_command(&self, msg: ClientMsg) -> vtx_core::Result<ServerMsg> {
        let stream = UnixStream::connect(&self.socket_path).await?;
        let (reader, mut writer) = stream.into_split();
        let mut reader = BufReader::new(reader);

        send_msg(&mut writer, &msg).await?;

        let mut line = String::new();
        reader.read_line(&mut line).await?;
        let response: ServerMsg = serde_json::from_str(line.trim())?;
        Ok(response)
    }
}

/// Returns true if this key event is "normal typing" that should snap scroll back to bottom.
fn is_typing_input(key: &KeyEvent) -> bool {
    if key.modifiers.contains(KeyModifiers::ALT) {
        return false;
    }
    matches!(
        key.code,
        KeyCode::Char(_) | KeyCode::Enter | KeyCode::Backspace | KeyCode::Tab
    )
}

/// Process a key event, handling prefix mode and direct bindings.
fn process_key(key: KeyEvent, prefix_active: &mut bool) -> InputAction {
    // === Scroll (Shift+PageUp/Down from keyboard) ===
    if key.modifiers.contains(KeyModifiers::SHIFT) {
        match key.code {
            KeyCode::PageUp => return InputAction::ScrollUp,
            KeyCode::PageDown => return InputAction::ScrollDown,
            _ => {}
        }
    }

    // === Direct bindings (no prefix needed) ===
    if key.modifiers.contains(KeyModifiers::ALT) {
        if key.modifiers.contains(KeyModifiers::SHIFT) {
            return match key.code {
                KeyCode::Char('H') => InputAction::Send(ClientMsg::ResizePane {
                    dir: Direction::Left,
                    amount: 5,
                }),
                KeyCode::Char('J') => InputAction::Send(ClientMsg::ResizePane {
                    dir: Direction::Down,
                    amount: 5,
                }),
                KeyCode::Char('K') => InputAction::Send(ClientMsg::ResizePane {
                    dir: Direction::Up,
                    amount: 5,
                }),
                KeyCode::Char('L') => InputAction::Send(ClientMsg::ResizePane {
                    dir: Direction::Right,
                    amount: 5,
                }),
                _ => InputAction::None,
            };
        }

        return match key.code {
            KeyCode::Char('h') => {
                InputAction::Send(ClientMsg::FocusDirection { dir: Direction::Left })
            }
            KeyCode::Char('j') => {
                InputAction::Send(ClientMsg::FocusDirection { dir: Direction::Down })
            }
            KeyCode::Char('k') => {
                InputAction::Send(ClientMsg::FocusDirection { dir: Direction::Up })
            }
            KeyCode::Char('l') => {
                InputAction::Send(ClientMsg::FocusDirection { dir: Direction::Right })
            }
            KeyCode::Left => {
                InputAction::Send(ClientMsg::FocusDirection { dir: Direction::Left })
            }
            KeyCode::Right => {
                InputAction::Send(ClientMsg::FocusDirection { dir: Direction::Right })
            }
            KeyCode::Up => {
                InputAction::Send(ClientMsg::FocusDirection { dir: Direction::Up })
            }
            KeyCode::Down => {
                InputAction::Send(ClientMsg::FocusDirection { dir: Direction::Down })
            }
            KeyCode::Char('x') => InputAction::Send(ClientMsg::KillPane),
            KeyCode::Char('a') => {
                InputAction::Send(ClientMsg::Split { horizontal: true })
            }
            _ => InputAction::None,
        };
    }

    // === Prefix key: Ctrl-A ===
    if !*prefix_active
        && key.code == KeyCode::Char('a')
        && key.modifiers.contains(KeyModifiers::CONTROL)
    {
        *prefix_active = true;
        return InputAction::None;
    }

    if *prefix_active {
        *prefix_active = false;
        return handle_prefix_key(key);
    }

    // === Normal mode — forward input to shell ===
    let data = key_event_to_bytes(&key);
    if data.is_empty() {
        InputAction::None
    } else {
        InputAction::Send(ClientMsg::Input { data })
    }
}

/// Handle the key pressed after the prefix key (Ctrl-A).
fn handle_prefix_key(key: KeyEvent) -> InputAction {
    match key.code {
        KeyCode::Char('|') => InputAction::Send(ClientMsg::Split { horizontal: true }),
        KeyCode::Char('-') => InputAction::Send(ClientMsg::Split { horizontal: false }),
        KeyCode::Char('x') => InputAction::Send(ClientMsg::KillPane),
        KeyCode::Char('D') | KeyCode::Char('d') => InputAction::Detach,
        KeyCode::Char('a') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            InputAction::Send(ClientMsg::Input { data: vec![0x01] })
        }
        KeyCode::Up => InputAction::Send(ClientMsg::FocusDirection { dir: Direction::Up }),
        KeyCode::Down => {
            InputAction::Send(ClientMsg::FocusDirection { dir: Direction::Down })
        }
        KeyCode::Left => {
            InputAction::Send(ClientMsg::FocusDirection { dir: Direction::Left })
        }
        KeyCode::Right => {
            InputAction::Send(ClientMsg::FocusDirection { dir: Direction::Right })
        }
        KeyCode::Char('h') => {
            InputAction::Send(ClientMsg::FocusDirection { dir: Direction::Left })
        }
        KeyCode::Char('j') => {
            InputAction::Send(ClientMsg::FocusDirection { dir: Direction::Down })
        }
        KeyCode::Char('k') => {
            InputAction::Send(ClientMsg::FocusDirection { dir: Direction::Up })
        }
        KeyCode::Char('l') => {
            InputAction::Send(ClientMsg::FocusDirection { dir: Direction::Right })
        }
        _ => InputAction::None,
    }
}

async fn send_msg(
    writer: &mut tokio::net::unix::OwnedWriteHalf,
    msg: &ClientMsg,
) -> vtx_core::Result<()> {
    let mut json = serde_json::to_string(msg).unwrap();
    json.push('\n');
    writer.write_all(json.as_bytes()).await?;
    Ok(())
}

fn key_event_to_bytes(event: &KeyEvent) -> Vec<u8> {
    if event.modifiers.contains(KeyModifiers::ALT) {
        return vec![];
    }

    match event.code {
        KeyCode::Char(c) => {
            if event.modifiers.contains(KeyModifiers::CONTROL) {
                let byte = (c as u8).to_ascii_lowercase() - b'a' + 1;
                vec![byte]
            } else {
                let mut buf = [0u8; 4];
                let s = c.encode_utf8(&mut buf);
                s.as_bytes().to_vec()
            }
        }
        KeyCode::Enter => vec![b'\r'],
        KeyCode::Backspace => vec![0x7f],
        KeyCode::Tab => vec![b'\t'],
        KeyCode::Esc => vec![0x1b],
        KeyCode::Up => b"\x1b[A".to_vec(),
        KeyCode::Down => b"\x1b[B".to_vec(),
        KeyCode::Right => b"\x1b[C".to_vec(),
        KeyCode::Left => b"\x1b[D".to_vec(),
        KeyCode::Home => b"\x1b[H".to_vec(),
        KeyCode::End => b"\x1b[F".to_vec(),
        KeyCode::Delete => b"\x1b[3~".to_vec(),
        KeyCode::PageUp => b"\x1b[5~".to_vec(),
        KeyCode::PageDown => b"\x1b[6~".to_vec(),
        KeyCode::Insert => b"\x1b[2~".to_vec(),
        KeyCode::F(n) => match n {
            1 => b"\x1bOP".to_vec(),
            2 => b"\x1bOQ".to_vec(),
            3 => b"\x1bOR".to_vec(),
            4 => b"\x1bOS".to_vec(),
            5 => b"\x1b[15~".to_vec(),
            6 => b"\x1b[17~".to_vec(),
            7 => b"\x1b[18~".to_vec(),
            8 => b"\x1b[19~".to_vec(),
            9 => b"\x1b[20~".to_vec(),
            10 => b"\x1b[21~".to_vec(),
            11 => b"\x1b[23~".to_vec(),
            12 => b"\x1b[24~".to_vec(),
            _ => vec![],
        },
        _ => vec![],
    }
}

/// Simple base64 encoder (no external dependency needed).
fn base64_encode(data: &[u8]) -> String {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut result = String::with_capacity((data.len() + 2) / 3 * 4);

    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };
        let triple = (b0 << 16) | (b1 << 8) | b2;

        result.push(CHARS[((triple >> 18) & 0x3F) as usize] as char);
        result.push(CHARS[((triple >> 12) & 0x3F) as usize] as char);
        if chunk.len() > 1 {
            result.push(CHARS[((triple >> 6) & 0x3F) as usize] as char);
        } else {
            result.push('=');
        }
        if chunk.len() > 2 {
            result.push(CHARS[(triple & 0x3F) as usize] as char);
        } else {
            result.push('=');
        }
    }

    result
}
