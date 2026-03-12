//! GPU-accelerated attach loop using winit + vtx-renderer-gpu.
//!
//! winit requires owning the main thread, so this module provides a
//! synchronous entry-point that blocks on the winit event loop.

use std::sync::Arc;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tracing::{error, info};
use winit::application::ApplicationHandler;
use winit::event::{ElementState, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::WindowId;

use vtx_core::ipc::{ClientMsg, Direction, PaneRender, ServerMsg, StyledStatus};
use vtx_core::PaneId;
use vtx_renderer_gpu::GpuRenderer;

// ── Helpers ─────────────────────────────────────────────────────────────

async fn send_msg(
    writer: &mut tokio::net::unix::OwnedWriteHalf,
    msg: &ClientMsg,
) -> vtx_core::Result<()> {
    let mut json = serde_json::to_string(msg).unwrap();
    json.push('\n');
    writer.write_all(json.as_bytes()).await?;
    Ok(())
}

/// Messages sent from the async Tokio tasks to the winit event loop via
/// an `EventLoopProxy`.
#[derive(Debug)]
enum UserEvent {
    ServerMsg(ServerMsg),
    /// The async bootstrap (connect + handshake) completed.
    Connected {
        writer: tokio::net::unix::OwnedWriteHalf,
    },
    /// A fatal error occurred in the async tasks.
    Error(String),
}

// ── Application state ───────────────────────────────────────────────────

struct GpuApp {
    socket_path: String,
    session_name: Option<String>,

    /// Tokio runtime — we keep a handle so we can spawn tasks.
    rt: Arc<tokio::runtime::Runtime>,

    renderer: Option<GpuRenderer>,
    writer: Option<tokio::net::unix::OwnedWriteHalf>,
    proxy: winit::event_loop::EventLoopProxy<UserEvent>,

    // Client state
    prefix_active: bool,
    scroll_offset: i32,
    last_frame: Option<(Vec<PaneRender>, PaneId, Vec<(u16, u16, u16, bool)>, StyledStatus, u16)>,
    modifiers: ModifiersState,
    done: bool,
}

impl GpuApp {
    fn send(&mut self, msg: ClientMsg) {
        if let Some(ref mut writer) = self.writer {
            let mut json = serde_json::to_string(&msg).unwrap();
            json.push('\n');
            let bytes = json.into_bytes();
            // We need to write from the tokio runtime
            let writer_ptr = writer as *mut tokio::net::unix::OwnedWriteHalf;
            // SAFETY: we only access writer from the main thread which is
            // also the only thread calling send().  The raw-pointer dance
            // avoids a borrow-checker fight with &mut self.
            let writer_ref = unsafe { &mut *writer_ptr };
            let rt = self.rt.clone();
            rt.block_on(async {
                let _ = writer_ref.write_all(&bytes).await;
            });
        }
    }

    fn request_redraw(&self) {
        if let Some(ref r) = self.renderer {
            r.window().request_redraw();
        }
    }

    fn render(&mut self) {
        if let (Some(renderer), Some((panes, focused, borders, status, total_rows))) =
            (&mut self.renderer, &self.last_frame)
        {
            if let Err(e) = renderer.render_frame(
                panes,
                *focused,
                borders,
                status,
                *total_rows,
                self.prefix_active,
                None,
            ) {
                error!("GPU render error: {e}");
            }
        }
    }
}

impl ApplicationHandler<UserEvent> for GpuApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        // Create the renderer (and window) on first resume.
        if self.renderer.is_some() {
            return;
        }

        match GpuRenderer::new(event_loop) {
            Ok(r) => {
                info!("GPU renderer initialised");
                self.renderer = Some(r);
            }
            Err(e) => {
                error!("Failed to create GPU renderer: {e}");
                event_loop.exit();
                return;
            }
        }

        // Kick off the async connection from the tokio runtime.
        let socket_path = self.socket_path.clone();
        let session_name = self.session_name.clone();
        let proxy = self.proxy.clone();

        self.rt.spawn(async move {
            let stream = match UnixStream::connect(&socket_path).await {
                Ok(s) => s,
                Err(e) => {
                    let _ = proxy.send_event(UserEvent::Error(format!("connect: {e}")));
                    return;
                }
            };
            let (reader, mut writer) = stream.into_split();
            let mut reader = BufReader::new(reader);

            // Handshake: NewSession
            if let Err(e) = send_msg(&mut writer, &ClientMsg::NewSession { name: session_name }).await {
                let _ = proxy.send_event(UserEvent::Error(format!("handshake send: {e}")));
                return;
            }

            let mut line = String::new();
            if let Err(e) = reader.read_line(&mut line).await {
                let _ = proxy.send_event(UserEvent::Error(format!("handshake read: {e}")));
                return;
            }

            match serde_json::from_str::<ServerMsg>(line.trim()) {
                Ok(ServerMsg::SessionReady { session, cols, rows }) => {
                    info!("GPU client attached to session {session} ({cols}x{rows})");
                }
                Ok(ServerMsg::Error { msg }) => {
                    let _ = proxy.send_event(UserEvent::Error(msg));
                    return;
                }
                Ok(_) => {}
                Err(e) => {
                    let _ = proxy.send_event(UserEvent::Error(format!("handshake parse: {e}")));
                    return;
                }
            }

            // Tell the event loop that we're connected — hand over writer.
            let _ = proxy.send_event(UserEvent::Connected { writer });

            // Dedicated server message reader — forward to event loop.
            let proxy2 = proxy.clone();
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
                                if proxy2.send_event(UserEvent::ServerMsg(msg)).is_err() {
                                    break;
                                }
                            }
                            Err(e) => error!("Bad server msg: {e}"),
                        }
                    }
                    Err(e) => {
                        error!("Server read error: {e}");
                        break;
                    }
                }
            }
        });
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UserEvent) {
        match event {
            UserEvent::Connected { writer } => {
                self.writer = Some(writer);
                // Send initial resize based on the GPU window's grid size.
                if let Some(ref renderer) = self.renderer {
                    let (cols, rows) = renderer.size();
                    self.send(ClientMsg::Resize { cols, rows });
                }
            }
            UserEvent::ServerMsg(msg) => match msg {
                ServerMsg::Render { panes, focused, borders, status, total_rows } => {
                    let mut status_display = status;
                    if self.scroll_offset > 0 {
                        status_display.left.push(vtx_core::ipc::StatusSegment {
                            text: format!(" [SCROLL: +{}] ", self.scroll_offset),
                            fg: (0x1a, 0x1b, 0x26),
                            bg: (0x7a, 0xa2, 0xf7),
                            bold: true,
                            click: None,
                        });
                    }
                    self.last_frame = Some((panes, focused, borders, status_display, total_rows));
                    self.request_redraw();
                }
                ServerMsg::Detached => {
                    self.done = true;
                    event_loop.exit();
                }
                ServerMsg::Error { msg } => {
                    error!("Server error: {msg}");
                }
                _ => {}
            },
            UserEvent::Error(msg) => {
                error!("Fatal: {msg}");
                self.done = true;
                event_loop.exit();
            }
        }
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _window_id: WindowId,
        event: WindowEvent,
    ) {
        match event {
            WindowEvent::CloseRequested => {
                self.send(ClientMsg::Detach);
                self.done = true;
                event_loop.exit();
            }

            WindowEvent::Resized(size) => {
                if let Some(ref mut renderer) = self.renderer {
                    renderer.handle_resize(size);
                    let (cols, rows) = renderer.size();
                    self.send(ClientMsg::Resize { cols, rows });
                }
            }

            WindowEvent::RedrawRequested => {
                self.render();
            }

            WindowEvent::ModifiersChanged(mods) => {
                self.modifiers = mods.state();
            }

            WindowEvent::KeyboardInput { event, .. } => {
                if event.state != ElementState::Pressed {
                    return;
                }

                let ctrl = self.modifiers.control_key();
                let alt = self.modifiers.alt_key();
                let shift = self.modifiers.shift_key();

                // Prefix key: Ctrl-A
                if !self.prefix_active && ctrl && event.logical_key == Key::Character("a".into()) {
                    self.prefix_active = true;
                    return;
                }

                if self.prefix_active {
                    self.prefix_active = false;
                    match &event.logical_key {
                        Key::Character(c) if c.as_str() == "|" => {
                            self.send(ClientMsg::Split { horizontal: true });
                        }
                        Key::Character(c) if c.as_str() == "-" => {
                            self.send(ClientMsg::Split { horizontal: false });
                        }
                        Key::Character(c) if c.as_str() == "x" => {
                            self.send(ClientMsg::KillPane);
                        }
                        Key::Character(c) if c.as_str() == "d" || c.as_str() == "D" => {
                            self.send(ClientMsg::Detach);
                            self.done = true;
                            event_loop.exit();
                        }
                        Key::Named(NamedKey::ArrowUp) => {
                            self.send(ClientMsg::FocusDirection { dir: Direction::Up });
                        }
                        Key::Named(NamedKey::ArrowDown) => {
                            self.send(ClientMsg::FocusDirection { dir: Direction::Down });
                        }
                        Key::Named(NamedKey::ArrowLeft) => {
                            self.send(ClientMsg::FocusDirection { dir: Direction::Left });
                        }
                        Key::Named(NamedKey::ArrowRight) => {
                            self.send(ClientMsg::FocusDirection { dir: Direction::Right });
                        }
                        _ => {}
                    }
                    return;
                }

                // Scroll: Shift+PageUp/Down
                if shift {
                    if event.logical_key == Key::Named(NamedKey::PageUp) {
                        self.scroll_offset += 3;
                        self.send(ClientMsg::ScrollBack { offset: self.scroll_offset });
                        return;
                    }
                    if event.logical_key == Key::Named(NamedKey::PageDown) {
                        self.scroll_offset = (self.scroll_offset - 3).max(0);
                        self.send(ClientMsg::ScrollBack { offset: self.scroll_offset });
                        return;
                    }
                }

                // Alt bindings (focus / resize / split)
                if alt {
                    if shift {
                        match &event.logical_key {
                            Key::Character(c) if c.as_str() == "H" => {
                                self.send(ClientMsg::ResizePane { dir: Direction::Left, amount: 5 });
                                return;
                            }
                            Key::Character(c) if c.as_str() == "J" => {
                                self.send(ClientMsg::ResizePane { dir: Direction::Down, amount: 5 });
                                return;
                            }
                            Key::Character(c) if c.as_str() == "K" => {
                                self.send(ClientMsg::ResizePane { dir: Direction::Up, amount: 5 });
                                return;
                            }
                            Key::Character(c) if c.as_str() == "L" => {
                                self.send(ClientMsg::ResizePane { dir: Direction::Right, amount: 5 });
                                return;
                            }
                            _ => {}
                        }
                    }
                    match &event.logical_key {
                        Key::Character(c) if c.as_str() == "h" => {
                            self.send(ClientMsg::FocusDirection { dir: Direction::Left });
                            return;
                        }
                        Key::Character(c) if c.as_str() == "j" => {
                            self.send(ClientMsg::FocusDirection { dir: Direction::Down });
                            return;
                        }
                        Key::Character(c) if c.as_str() == "k" => {
                            self.send(ClientMsg::FocusDirection { dir: Direction::Up });
                            return;
                        }
                        Key::Character(c) if c.as_str() == "l" => {
                            self.send(ClientMsg::FocusDirection { dir: Direction::Right });
                            return;
                        }
                        Key::Character(c) if c.as_str() == "x" => {
                            self.send(ClientMsg::KillPane);
                            return;
                        }
                        Key::Character(c) if c.as_str() == "a" => {
                            self.send(ClientMsg::Split { horizontal: true });
                            return;
                        }
                        _ => {}
                    }
                    return;
                }

                // Snap scrollback on normal typing
                if self.scroll_offset > 0 {
                    let is_typing = matches!(
                        &event.logical_key,
                        Key::Character(_)
                            | Key::Named(NamedKey::Enter)
                            | Key::Named(NamedKey::Backspace)
                            | Key::Named(NamedKey::Tab)
                    );
                    if is_typing && !alt {
                        self.scroll_offset = 0;
                        self.send(ClientMsg::ScrollBack { offset: 0 });
                    }
                }

                // Normal input — convert to bytes and send
                let data = logical_key_to_bytes(&event.logical_key, ctrl);
                if !data.is_empty() {
                    self.send(ClientMsg::Input { data });
                }
            }

            _ => {}
        }
    }
}

/// Convert a winit logical key to terminal byte sequence.
fn logical_key_to_bytes(key: &Key, ctrl: bool) -> Vec<u8> {
    match key {
        Key::Character(c) => {
            let s = c.as_str();
            if ctrl && s.len() == 1 {
                let byte = s.as_bytes()[0].to_ascii_lowercase() - b'a' + 1;
                vec![byte]
            } else {
                s.as_bytes().to_vec()
            }
        }
        Key::Named(named) => match named {
            NamedKey::Enter => vec![b'\r'],
            NamedKey::Backspace => vec![0x7f],
            NamedKey::Tab => vec![b'\t'],
            NamedKey::Escape => vec![0x1b],
            NamedKey::ArrowUp => b"\x1b[A".to_vec(),
            NamedKey::ArrowDown => b"\x1b[B".to_vec(),
            NamedKey::ArrowRight => b"\x1b[C".to_vec(),
            NamedKey::ArrowLeft => b"\x1b[D".to_vec(),
            NamedKey::Home => b"\x1b[H".to_vec(),
            NamedKey::End => b"\x1b[F".to_vec(),
            NamedKey::Delete => b"\x1b[3~".to_vec(),
            NamedKey::PageUp => b"\x1b[5~".to_vec(),
            NamedKey::PageDown => b"\x1b[6~".to_vec(),
            NamedKey::Insert => b"\x1b[2~".to_vec(),
            NamedKey::F1 => b"\x1bOP".to_vec(),
            NamedKey::F2 => b"\x1bOQ".to_vec(),
            NamedKey::F3 => b"\x1bOR".to_vec(),
            NamedKey::F4 => b"\x1bOS".to_vec(),
            NamedKey::F5 => b"\x1b[15~".to_vec(),
            NamedKey::F6 => b"\x1b[17~".to_vec(),
            NamedKey::F7 => b"\x1b[18~".to_vec(),
            NamedKey::F8 => b"\x1b[19~".to_vec(),
            NamedKey::F9 => b"\x1b[20~".to_vec(),
            NamedKey::F10 => b"\x1b[21~".to_vec(),
            NamedKey::F11 => b"\x1b[23~".to_vec(),
            NamedKey::F12 => b"\x1b[24~".to_vec(),
            _ => vec![],
        },
        _ => vec![],
    }
}

// ── Public entry point ──────────────────────────────────────────────────

/// Run the GPU-accelerated attach loop.  This blocks the calling thread
/// because winit's event loop takes ownership of the main thread.
pub fn run_gpu_attach(socket_path: String, session_name: Option<String>) -> vtx_core::Result<()> {
    let event_loop = EventLoop::<UserEvent>::with_user_event()
        .build()
        .map_err(|e| vtx_core::VtxError::Other(format!("Failed to create event loop: {e}")))?;

    event_loop.set_control_flow(ControlFlow::Wait);

    let proxy = event_loop.create_proxy();

    let rt = Arc::new(
        tokio::runtime::Runtime::new()
            .map_err(|e| vtx_core::VtxError::Other(format!("Failed to create runtime: {e}")))?,
    );

    let mut app = GpuApp {
        socket_path,
        session_name,
        rt,
        renderer: None,
        writer: None,
        proxy,
        prefix_active: false,
        scroll_offset: 0,
        last_frame: None,
        modifiers: ModifiersState::empty(),
        done: false,
    };

    event_loop
        .run_app(&mut app)
        .map_err(|e| vtx_core::VtxError::Other(format!("Event loop error: {e}")))?;

    Ok(())
}
