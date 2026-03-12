use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{mpsc, Mutex};
use tracing::{error, info, warn};

use vtx_core::config::Config;
use vtx_core::ipc::{ClientMsg, LayoutPreset, PaneRender, PopupRect, SavedPane, SavedSession, SavedWindow, ServerMsg, SessionInfo, StatusSegment, StyledStatus};
use vtx_core::{PaneId, SessionId};
use vtx_layout::Rect;
use vtx_plugin::PluginManager;
use vtx_plugin::hooks::{HookContext, HookEvent};

use crate::pane::Pane;
use crate::session::Session;

/// Per-client state tracked by the server.
struct ClientState {
    attached_session: Option<SessionId>,
    cols: u16,
    rows: u16,
    scroll_offset: i32,
}

/// Shared server state behind a mutex.
struct ServerState {
    config: Config,
    sessions: HashMap<SessionId, Session>,
    next_session_id: u32,
    plugins: PluginManager,
    /// Name of the currently active theme.
    active_theme: String,
}

pub struct VtxServer {
    state: Arc<Mutex<ServerState>>,
}

impl VtxServer {
    pub fn new(config: Config) -> Self {
        let mut plugins = PluginManager::new();
        load_plugins_from_dir(&mut plugins);

        VtxServer {
            state: Arc::new(Mutex::new(ServerState {
                config,
                sessions: HashMap::new(),
                next_session_id: 0,
                plugins,
                active_theme: "Tokyo Night".to_string(),
            })),
        }
    }

    pub async fn run(self) -> vtx_core::Result<()> {
        let socket_path = {
            let state = self.state.lock().await;
            state.config.socket_path.clone()
        };

        if Path::new(&socket_path).exists() {
            std::fs::remove_file(&socket_path)?;
        }
        if let Some(parent) = Path::new(&socket_path).parent() {
            std::fs::create_dir_all(parent)?;
        }

        let listener = UnixListener::bind(&socket_path)?;
        info!("vtx server listening on {socket_path}");

        // Periodic auto-save: save all sessions every 60 seconds
        let autosave_state = Arc::clone(&self.state);
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(60));
            interval.tick().await; // first tick fires immediately, skip it
            loop {
                interval.tick().await;
                let st = autosave_state.lock().await;
                for session in st.sessions.values() {
                    if let Err(e) = save_session_to_disk(session) {
                        warn!("Auto-save failed for session '{}': {e}", session.name);
                    }
                }
            }
        });

        loop {
            match listener.accept().await {
                Ok((stream, _)) => {
                    info!("Client connected");
                    let state = Arc::clone(&self.state);
                    tokio::spawn(async move {
                        if let Err(e) = handle_client(state, stream).await {
                            error!("Client error: {e}");
                        }
                        info!("Client disconnected");
                    });
                }
                Err(e) => error!("Accept error: {e}"),
            }
        }
    }
}

async fn handle_client(
    state: Arc<Mutex<ServerState>>,
    stream: UnixStream,
) -> vtx_core::Result<()> {
    let (reader, writer) = stream.into_split();
    let mut reader = BufReader::new(reader);
    let writer = Arc::new(Mutex::new(writer));

    let (pty_tx, mut pty_rx) = mpsc::channel::<()>(64);
    // Channel for client messages — read_line runs in its own task so it's never cancelled
    let (msg_tx, mut msg_rx) = mpsc::channel::<ClientMsg>(64);

    let mut cs = ClientState {
        attached_session: None,
        cols: 80,
        rows: 24,
        scroll_offset: 0,
    };

    // PTY polling task — drains channel-based readers, no blocking
    let poll_state = Arc::clone(&state);
    let poll_handle = tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_millis(8)).await;

            let mut st = poll_state.lock().await;
            let mut any_output = false;

            for session in st.sessions.values_mut() {
                for window in session.windows.iter_mut() {
                    for pane in window.panes.values_mut() {
                        if pane.drain_output() {
                            any_output = true;
                        }
                    }
                }
            }

            if any_output {
                let _ = pty_tx.try_send(());
            }
        }
    });

    // Dedicated reader task — read_line is NOT cancel-safe in select!,
    // so we give it its own task where it's never cancelled.
    let reader_handle = tokio::spawn(async move {
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line).await {
                Ok(0) => break,
                Ok(_) => {
                    match serde_json::from_str::<ClientMsg>(&line) {
                        Ok(msg) => {
                            if msg_tx.send(msg).await.is_err() {
                                break;
                            }
                        }
                        Err(e) => {
                            error!("Bad client message: {e}");
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

    // Main message loop — only uses cancel-safe channel receives
    loop {
        tokio::select! {
            _ = pty_rx.recv() => {
                if let Some(sid) = cs.attached_session {
                    let st = state.lock().await;
                    if let Some(session) = st.sessions.get(&sid) {
                        let msg = build_render_msg_scrolled(session, cs.cols, cs.rows, cs.scroll_offset, &st.config.status_bar);
                        let mut json = serde_json::to_string(&msg).unwrap();
                        json.push('\n');
                        let mut w = writer.lock().await;
                        if w.write_all(json.as_bytes()).await.is_err() {
                            break;
                        }
                    }
                }
            }

            result = msg_rx.recv() => {
                match result {
                    Some(msg) => {
                        let response = {
                            let mut st = state.lock().await;
                            handle_message(&mut st, &mut cs, msg)
                        };

                        let is_shutdown = matches!(response, ServerMsg::ServerShutdown);

                        let mut json = serde_json::to_string(&response).unwrap();
                        json.push('\n');
                        let mut w = writer.lock().await;
                        if w.write_all(json.as_bytes()).await.is_err() {
                            break;
                        }
                        drop(w);

                        if is_shutdown {
                            // Give the response time to flush, then exit
                            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                            std::process::exit(0);
                        }
                    }
                    None => break, // reader task ended
                }
            }
        }
    }

    poll_handle.abort();
    reader_handle.abort();
    Ok(())
}

fn handle_message(
    st: &mut ServerState,
    cs: &mut ClientState,
    msg: ClientMsg,
) -> ServerMsg {
    match msg {
        ClientMsg::NewSession { name } => {
            let session_name = name.clone().unwrap_or_else(|| format!("vtx-{}", st.next_session_id));

            // Try to auto-restore a saved session with this name
            let saved_path = sessions_dir().join(format!("{}.json", session_name));
            if saved_path.exists() {
                info!("Found saved session '{}', restoring...", session_name);
                match restore_session_from_disk(st, cs, &session_name) {
                    Ok(msg) => return msg,
                    Err(e) => {
                        warn!("Failed to restore saved session '{}': {e}, creating fresh", session_name);
                        // Fall through to create a new session
                    }
                }
            }

            let sid = SessionId(st.next_session_id);
            st.next_session_id += 1;

            let pane_id = PaneId(0);
            // Use client's terminal size minus 1 row for status bar
            let pane_rows = cs.rows.saturating_sub(1);

            match Pane::spawn(pane_id, cs.cols, pane_rows, &st.config.default_shell) {
                Ok(pane) => {
                    let session = Session::new(sid, session_name, pane);
                    st.sessions.insert(sid, session);
                    cs.attached_session = Some(sid);
                    info!("Created session {sid}");

                    // Dispatch plugin hooks and process returned actions
                    let actions = st.plugins.dispatch_hook(HookEvent::SessionCreate, &HookContext {
                        session_id: Some(sid.0),
                        pane_id: Some(pane_id.0),
                        ..Default::default()
                    });
                    process_plugin_actions(st, cs, actions);

                    ServerMsg::SessionReady {
                        session: sid,
                        cols: cs.cols,
                        rows: cs.rows,
                    }
                }
                Err(e) => ServerMsg::Error {
                    msg: e.to_string(),
                },
            }
        }
        ClientMsg::ListSessions => {
            let list = st
                .sessions
                .values()
                .map(|s| SessionInfo {
                    id: s.id,
                    name: s.name.clone(),
                    pane_count: s.total_pane_count(),
                    created: s.created,
                })
                .collect();
            ServerMsg::Sessions { list }
        }
        ClientMsg::Attach { session } => {
            if st.sessions.contains_key(&session) {
                cs.attached_session = Some(session);
                ServerMsg::SessionReady {
                    session,
                    cols: cs.cols,
                    rows: cs.rows,
                }
            } else {
                ServerMsg::Error {
                    msg: format!("Session {session} not found"),
                }
            }
        }
        ClientMsg::Input { data } => {
            if let Some(sid) = cs.attached_session {
                if let Some(session) = st.sessions.get_mut(&sid) {
                    let win = session.active_window_mut();
                    // When a popup is active, route input to the popup pane
                    let target = if let Some((popup_id, _)) = win.popup {
                        popup_id
                    } else {
                        win.focused_pane
                    };
                    if let Some(pane) = win.panes.get_mut(&target) {
                        let _ = pane.write_input(&data);
                    }
                    return build_render_msg(session, cs.cols, cs.rows, &st.config.status_bar);
                }
            }
            ServerMsg::Error { msg: "No session attached".into() }
        }
        ClientMsg::Resize { cols, rows } => {
            cs.cols = cols;
            cs.rows = rows;

            if let Some(sid) = cs.attached_session {
                if let Some(session) = st.sessions.get_mut(&sid) {
                    // Recalculate layout and resize each pane to its rect
                    let pane_area_rows = rows.saturating_sub(1); // status bar
                    let rects = session.resolve_layout(cols, pane_area_rows);
                    let win = session.active_window_mut();
                    for (pid, rect) in &rects {
                        if let Some(pane) = win.panes.get_mut(pid) {
                            let _ = pane.resize(rect.cols, rect.rows);
                        }
                    }
                    // Also resize popup pane if active
                    if let Some((popup_id, ref mut prect)) = win.popup {
                        let popup_cols = (cols as f32 * 0.8) as u16;
                        let popup_rows = (pane_area_rows as f32 * 0.8) as u16;
                        let popup_x = (cols.saturating_sub(popup_cols)) / 2;
                        let popup_y = (pane_area_rows.saturating_sub(popup_rows)) / 2;
                        prect.x = popup_x;
                        prect.y = popup_y;
                        prect.cols = popup_cols;
                        prect.rows = popup_rows;
                        if let Some(pane) = win.panes.get_mut(&popup_id) {
                            let _ = pane.resize(popup_cols, popup_rows);
                        }
                    }
                    return build_render_msg(session, cols, rows, &st.config.status_bar);
                }
            }
            ServerMsg::Error { msg: "No session attached".into() }
        }
        ClientMsg::Split { horizontal: _ } => {
            if let Some(sid) = cs.attached_session {
                if let Some(session) = st.sessions.get_mut(&sid) {
                    let target = session.active_window().focused_pane;
                    // Dwindle-style: alternate split direction with each new pane.
                    // Odd pane count → horizontal (side by side), even → vertical (top/bottom).
                    // This gives the classic spiral: A|B, then B splits to B/C, then C splits to C|D, etc.
                    let pane_count = session.active_window().panes.len();
                    let dir = if pane_count % 2 == 1 {
                        vtx_layout::SplitDir::Horizontal
                    } else {
                        vtx_layout::SplitDir::Vertical
                    };

                    // Calculate what size the new pane should be
                    let pane_rows = cs.rows.saturating_sub(1);
                    match session.split_pane(target, dir, &st.config.default_shell, cs.cols, pane_rows) {
                        Ok(new_id) => {
                            // Now resize all panes to their layout rects
                            let rects = session.resolve_layout(cs.cols, pane_rows);
                            let win = session.active_window_mut();
                            for (pid, rect) in &rects {
                                if let Some(pane) = win.panes.get_mut(pid) {
                                    let _ = pane.resize(rect.cols, rect.rows);
                                }
                            }
                            let actions = st.plugins.dispatch_hook(HookEvent::PaneCreate, &HookContext {
                                pane_id: Some(new_id.0),
                                session_id: cs.attached_session.map(|s| s.0),
                                ..Default::default()
                            });
                            process_plugin_actions(st, cs, actions);
                            if let Some(session) = st.sessions.get(&sid) {
                                return build_render_msg(session, cs.cols, cs.rows, &st.config.status_bar);
                            }
                            return ServerMsg::Error { msg: "Session lost".into() };
                        }
                        Err(e) => return ServerMsg::Error { msg: e.to_string() },
                    }
                }
            }
            ServerMsg::Error { msg: "No session attached".into() }
        }
        ClientMsg::FocusDirection { dir } => {
            if let Some(sid) = cs.attached_session {
                if let Some(session) = st.sessions.get_mut(&sid) {
                    let pane_rows = cs.rows.saturating_sub(1);
                    let area = Rect { x: 0, y: 0, cols: cs.cols, rows: pane_rows };
                    let win = session.active_window_mut();
                    if let Some(next) = win.layout.find_neighbor(area, win.focused_pane, dir) {
                        win.focused_pane = next;
                    }
                    return build_render_msg(session, cs.cols, cs.rows, &st.config.status_bar);
                }
            }
            ServerMsg::Error { msg: "No session attached".into() }
        }
        ClientMsg::FocusPane { pane } => {
            if let Some(sid) = cs.attached_session {
                if let Some(session) = st.sessions.get_mut(&sid) {
                    let win = session.active_window_mut();
                    if win.panes.contains_key(&pane) {
                        win.focused_pane = pane;
                        return build_render_msg(session, cs.cols, cs.rows, &st.config.status_bar);
                    }
                }
            }
            ServerMsg::Error { msg: "Pane not found".into() }
        }
        ClientMsg::KillPane => {
            if let Some(sid) = cs.attached_session {
                if let Some(session) = st.sessions.get_mut(&sid) {
                    let target = session.active_window().focused_pane;
                    let win = session.active_window_mut();
                    win.panes.remove(&target);

                    let actions = st.plugins.dispatch_hook(HookEvent::PaneClose, &HookContext {
                        pane_id: Some(target.0),
                        session_id: Some(sid.0),
                        ..Default::default()
                    });
                    process_plugin_actions(st, cs, actions);

                    // Re-fetch session after plugin actions
                    let session = match st.sessions.get_mut(&sid) {
                        Some(s) => s,
                        None => return ServerMsg::Detached,
                    };
                    let win = session.active_window_mut();
                    if win.panes.is_empty() {
                        // Last pane in window — remove the window
                        let win_idx = session.active_window;
                        session.windows.remove(win_idx);

                        if session.windows.is_empty() {
                            // Last window — remove session
                            st.sessions.remove(&sid);
                            cs.attached_session = None;
                            return ServerMsg::Detached;
                        }

                        // Adjust active_window index
                        if session.active_window >= session.windows.len() {
                            session.active_window = session.windows.len() - 1;
                        }
                        return build_render_msg(session, cs.cols, cs.rows, &st.config.status_bar);
                    }

                    // Remove from layout tree (collapses the split node)
                    win.layout.remove(target);

                    // Focus the first remaining pane
                    let first = *win.panes.keys().next().unwrap();
                    win.focused_pane = first;

                    // Resize remaining panes to fill the space
                    let pane_rows = cs.rows.saturating_sub(1);
                    let rects = session.resolve_layout(cs.cols, pane_rows);
                    let win = session.active_window_mut();
                    for (pid, rect) in &rects {
                        if let Some(pane) = win.panes.get_mut(pid) {
                            let _ = pane.resize(rect.cols, rect.rows);
                        }
                    }
                    return build_render_msg(session, cs.cols, cs.rows, &st.config.status_bar);
                }
            }
            ServerMsg::Error { msg: "No session attached".into() }
        }
        ClientMsg::ResizePane { dir, amount } => {
            if let Some(sid) = cs.attached_session {
                if let Some(session) = st.sessions.get_mut(&sid) {
                    let pane_rows = cs.rows.saturating_sub(1);
                    // Convert pixel amount to ratio delta based on total size
                    let total = match dir {
                        vtx_core::ipc::Direction::Left | vtx_core::ipc::Direction::Right => cs.cols as f32,
                        vtx_core::ipc::Direction::Up | vtx_core::ipc::Direction::Down => pane_rows as f32,
                    };
                    let delta = amount as f32 / total;
                    let win = session.active_window_mut();
                    win.layout.resize_pane(win.focused_pane, dir, delta);

                    // Resize all PTYs to match new layout
                    let rects = session.resolve_layout(cs.cols, pane_rows);
                    let win = session.active_window_mut();
                    for (pid, rect) in &rects {
                        if let Some(pane) = win.panes.get_mut(pid) {
                            let _ = pane.resize(rect.cols, rect.rows);
                        }
                    }
                    return build_render_msg(session, cs.cols, cs.rows, &st.config.status_bar);
                }
            }
            ServerMsg::Error { msg: "No session attached".into() }
        }
        ClientMsg::SshPane { host, user, port } => {
            if let Some(sid) = cs.attached_session {
                if let Some(session) = st.sessions.get_mut(&sid) {
                    let new_id = session.alloc_pane_id();
                    let pane_rows = cs.rows.saturating_sub(1);
                    match Pane::spawn_ssh(
                        new_id,
                        cs.cols,
                        pane_rows,
                        &host,
                        user.as_deref(),
                        port,
                    ) {
                        Ok(pane) => {
                            let win = session.active_window_mut();
                            let target = win.focused_pane;
                            win.panes.insert(new_id, pane);
                            win.layout.split(target, new_id, vtx_layout::SplitDir::Vertical);
                            win.focused_pane = new_id;

                            // Resize all panes to their layout rects
                            let rects = session.resolve_layout(cs.cols, pane_rows);
                            let win = session.active_window_mut();
                            for (pid, rect) in &rects {
                                if let Some(p) = win.panes.get_mut(pid) {
                                    let _ = p.resize(rect.cols, rect.rows);
                                }
                            }
                            return build_render_msg(session, cs.cols, cs.rows, &st.config.status_bar);
                        }
                        Err(e) => return ServerMsg::Error { msg: e.to_string() },
                    }
                }
            }
            ServerMsg::Error { msg: "No session attached".into() }
        }
        ClientMsg::Widget { kind } => {
            if let Some(sid) = cs.attached_session {
                if let Some(session) = st.sessions.get_mut(&sid) {
                    let new_id = session.alloc_pane_id();
                    let pane_rows = cs.rows.saturating_sub(1);
                    match Pane::spawn_widget(new_id, cs.cols, pane_rows, &kind) {
                        Ok(pane) => {
                            let win = session.active_window_mut();
                            let target = win.focused_pane;
                            win.panes.insert(new_id, pane);
                            win.layout.split(target, new_id, vtx_layout::SplitDir::Vertical);
                            win.focused_pane = new_id;

                            // Resize all panes to their layout rects
                            let rects = session.resolve_layout(cs.cols, pane_rows);
                            let win = session.active_window_mut();
                            for (pid, rect) in &rects {
                                if let Some(p) = win.panes.get_mut(pid) {
                                    let _ = p.resize(rect.cols, rect.rows);
                                }
                            }
                            return build_render_msg(session, cs.cols, cs.rows, &st.config.status_bar);
                        }
                        Err(e) => return ServerMsg::Error { msg: e.to_string() },
                    }
                }
            }
            ServerMsg::Error {
                msg: "No session attached".into(),
            }
        }
        ClientMsg::ScrollBack { offset } => {
            cs.scroll_offset = offset;
            if let Some(sid) = cs.attached_session {
                if let Some(session) = st.sessions.get(&sid) {
                    return build_render_msg_scrolled(session, cs.cols, cs.rows, offset, &st.config.status_bar);
                }
            }
            ServerMsg::Error { msg: "No session attached".into() }
        }
        ClientMsg::ZoomPane => {
            if let Some(sid) = cs.attached_session {
                if let Some(session) = st.sessions.get_mut(&sid) {
                    let win = session.active_window_mut();
                    let focused = win.focused_pane;
                    if win.zoomed_pane == Some(focused) {
                        // Unzoom — restore normal layout sizes
                        win.zoomed_pane = None;
                        let pane_rows = cs.rows.saturating_sub(1);
                        let rects = session.resolve_layout(cs.cols, pane_rows);
                        let win = session.active_window_mut();
                        for (pid, rect) in &rects {
                            if let Some(pane) = win.panes.get_mut(pid) {
                                let _ = pane.resize(rect.cols, rect.rows);
                            }
                        }
                    } else {
                        // Zoom the focused pane to full size
                        win.zoomed_pane = Some(focused);
                        let pane_rows = cs.rows.saturating_sub(1);
                        if let Some(pane) = win.panes.get_mut(&focused) {
                            let _ = pane.resize(cs.cols, pane_rows);
                        }
                    }
                    return build_render_msg(session, cs.cols, cs.rows, &st.config.status_bar);
                }
            }
            ServerMsg::Error { msg: "No session attached".into() }
        }
        ClientMsg::SearchScrollback { query } => {
            if let Some(sid) = cs.attached_session {
                if let Some(session) = st.sessions.get(&sid) {
                    let win = session.active_window();
                    let focused = win.focused_pane;
                    if let Some(pane) = win.panes.get(&focused) {
                        let matches = pane.parser.grid.search(&query);
                        let total_matches = matches.len();
                        if let Some(&line_idx) = matches.first() {
                            let sb_len = pane.parser.grid.scrollback_len();
                            let screen_rows = pane.parser.grid.rows as usize;
                            let total = sb_len + screen_rows;
                            let half_view = screen_rows / 2;
                            let lines_from_bottom = total.saturating_sub(line_idx).saturating_sub(1);
                            let offset = lines_from_bottom.saturating_sub(half_view);
                            let offset = offset.min(sb_len) as i32;
                            cs.scroll_offset = offset;
                            return ServerMsg::SearchResult {
                                offset,
                                matches: total_matches,
                            };
                        } else {
                            return ServerMsg::SearchResult {
                                offset: cs.scroll_offset,
                                matches: 0,
                            };
                        }
                    }
                }
            }
            ServerMsg::Error { msg: "No session attached".into() }
        }
        ClientMsg::PopupPane { command } => {
            if let Some(sid) = cs.attached_session {
                if let Some(session) = st.sessions.get_mut(&sid) {
                    let win = session.active_window_mut();
                    // Close existing popup if any
                    if let Some((old_id, _)) = win.popup.take() {
                        win.panes.remove(&old_id);
                    }

                    let pane_area_rows = cs.rows.saturating_sub(1);
                    // 80% of the terminal, centered
                    let popup_cols = (cs.cols as f32 * 0.8) as u16;
                    let popup_rows = (pane_area_rows as f32 * 0.8) as u16;
                    let popup_x = (cs.cols.saturating_sub(popup_cols)) / 2;
                    let popup_y = (pane_area_rows.saturating_sub(popup_rows)) / 2;

                    let new_id = session.alloc_pane_id();
                    let shell = command.unwrap_or_else(|| st.config.default_shell.clone());
                    match Pane::spawn(new_id, popup_cols, popup_rows, &shell) {
                        Ok(pane) => {
                            let win = session.active_window_mut();
                            win.panes.insert(new_id, pane);
                            win.popup = Some((new_id, PopupRect {
                                x: popup_x,
                                y: popup_y,
                                cols: popup_cols,
                                rows: popup_rows,
                            }));
                            return build_render_msg(session, cs.cols, cs.rows, &st.config.status_bar);
                        }
                        Err(e) => return ServerMsg::Error { msg: e.to_string() },
                    }
                }
            }
            ServerMsg::Error { msg: "No session attached".into() }
        }
        ClientMsg::ClosePopup => {
            if let Some(sid) = cs.attached_session {
                if let Some(session) = st.sessions.get_mut(&sid) {
                    let win = session.active_window_mut();
                    if let Some((popup_id, _)) = win.popup.take() {
                        win.panes.remove(&popup_id);
                    }
                    return build_render_msg(session, cs.cols, cs.rows, &st.config.status_bar);
                }
            }
            ServerMsg::Error { msg: "No session attached".into() }
        }
        ClientMsg::NewWindow { name } => {
            if let Some(sid) = cs.attached_session {
                if let Some(session) = st.sessions.get_mut(&sid) {
                    let new_id = session.alloc_pane_id();
                    let pane_rows = cs.rows.saturating_sub(1);
                    match Pane::spawn(new_id, cs.cols, pane_rows, &st.config.default_shell) {
                        Ok(pane) => {
                            let window_name = name.unwrap_or_else(|| "zsh".to_string());
                            let mut panes = HashMap::new();
                            panes.insert(new_id, pane);
                            let window = crate::session::Window {
                                name: window_name,
                                layout: vtx_layout::LayoutNode::single(new_id),
                                panes,
                                focused_pane: new_id,
                                zoomed_pane: None,
                                popup: None,
                                current_preset: None,
                            };
                            session.windows.push(window);
                            session.active_window = session.windows.len() - 1;
                            // Dispatch window create hook
                            let actions = st.plugins.dispatch_hook(HookEvent::WindowCreate, &HookContext {
                                session_id: Some(sid.0),
                                pane_id: Some(new_id.0),
                                ..Default::default()
                            });
                            process_plugin_actions(st, cs, actions);
                            if let Some(session) = st.sessions.get(&sid) {
                                return build_render_msg(session, cs.cols, cs.rows, &st.config.status_bar);
                            }
                            return ServerMsg::Error { msg: "Session lost".into() };
                        }
                        Err(e) => return ServerMsg::Error { msg: e.to_string() },
                    }
                }
            }
            ServerMsg::Error { msg: "No session attached".into() }
        }
        ClientMsg::NextWindow => {
            if let Some(sid) = cs.attached_session {
                if let Some(session) = st.sessions.get_mut(&sid) {
                    if session.windows.len() > 1 {
                        session.active_window = (session.active_window + 1) % session.windows.len();
                    }
                    return build_render_msg(session, cs.cols, cs.rows, &st.config.status_bar);
                }
            }
            ServerMsg::Error { msg: "No session attached".into() }
        }
        ClientMsg::PrevWindow => {
            if let Some(sid) = cs.attached_session {
                if let Some(session) = st.sessions.get_mut(&sid) {
                    if session.windows.len() > 1 {
                        if session.active_window == 0 {
                            session.active_window = session.windows.len() - 1;
                        } else {
                            session.active_window -= 1;
                        }
                    }
                    return build_render_msg(session, cs.cols, cs.rows, &st.config.status_bar);
                }
            }
            ServerMsg::Error { msg: "No session attached".into() }
        }
        ClientMsg::SelectWindow { index } => {
            if let Some(sid) = cs.attached_session {
                if let Some(session) = st.sessions.get_mut(&sid) {
                    if index < session.windows.len() {
                        session.active_window = index;
                    }
                    return build_render_msg(session, cs.cols, cs.rows, &st.config.status_bar);
                }
            }
            ServerMsg::Error { msg: "No session attached".into() }
        }
        ClientMsg::RenameWindow { name } => {
            if let Some(sid) = cs.attached_session {
                if let Some(session) = st.sessions.get_mut(&sid) {
                    session.active_window_mut().name = name;
                    return build_render_msg(session, cs.cols, cs.rows, &st.config.status_bar);
                }
            }
            ServerMsg::Error { msg: "No session attached".into() }
        }
        ClientMsg::LayoutCycle => {
            if let Some(sid) = cs.attached_session {
                if let Some(session) = st.sessions.get_mut(&sid) {
                    let win = session.active_window_mut();
                    let next_preset = win.current_preset
                        .unwrap_or(LayoutPreset::EvenHorizontal)
                        .next();
                    win.current_preset = Some(next_preset);

                    // Collect pane IDs from current layout (preserves order)
                    let pane_ids = win.layout.pane_ids();
                    win.layout = vtx_layout::build_preset(&next_preset, &pane_ids);

                    // Resize all PTYs to match the new layout
                    let pane_rows = cs.rows.saturating_sub(1);
                    let rects = session.resolve_layout(cs.cols, pane_rows);
                    let win = session.active_window_mut();
                    for (pid, rect) in &rects {
                        if let Some(pane) = win.panes.get_mut(pid) {
                            let _ = pane.resize(rect.cols, rect.rows);
                        }
                    }
                    return build_render_msg(session, cs.cols, cs.rows, &st.config.status_bar);
                }
            }
            ServerMsg::Error { msg: "No session attached".into() }
        }
        ClientMsg::SelectLayout { preset } => {
            if let Some(sid) = cs.attached_session {
                if let Some(session) = st.sessions.get_mut(&sid) {
                    let win = session.active_window_mut();
                    win.current_preset = Some(preset);

                    let pane_ids = win.layout.pane_ids();
                    win.layout = vtx_layout::build_preset(&preset, &pane_ids);

                    let pane_rows = cs.rows.saturating_sub(1);
                    let rects = session.resolve_layout(cs.cols, pane_rows);
                    let win = session.active_window_mut();
                    for (pid, rect) in &rects {
                        if let Some(pane) = win.panes.get_mut(pid) {
                            let _ = pane.resize(rect.cols, rect.rows);
                        }
                    }
                    return build_render_msg(session, cs.cols, cs.rows, &st.config.status_bar);
                }
            }
            ServerMsg::Error { msg: "No session attached".into() }
        }
        ClientMsg::DragBorder { border_x, border_y, horizontal, delta } => {
            if let Some(sid) = cs.attached_session {
                if let Some(session) = st.sessions.get_mut(&sid) {
                    let pane_rows = cs.rows.saturating_sub(1);
                    let area = Rect { x: 0, y: 0, cols: cs.cols, rows: pane_rows };
                    let win = session.active_window_mut();
                    win.layout.adjust_border_at(area, border_x, border_y, horizontal, delta);

                    // Resize all PTYs to match new layout
                    let rects = session.resolve_layout(cs.cols, pane_rows);
                    let win = session.active_window_mut();
                    for (pid, rect) in &rects {
                        if let Some(pane) = win.panes.get_mut(pid) {
                            let _ = pane.resize(rect.cols, rect.rows);
                        }
                    }
                    return build_render_msg(session, cs.cols, cs.rows, &st.config.status_bar);
                }
            }
            ServerMsg::Error { msg: "No session attached".into() }
        }
        ClientMsg::SwapPane { dir } => {
            if let Some(sid) = cs.attached_session {
                if let Some(session) = st.sessions.get_mut(&sid) {
                    let pane_rows = cs.rows.saturating_sub(1);
                    let area = Rect { x: 0, y: 0, cols: cs.cols, rows: pane_rows };
                    let win = session.active_window_mut();
                    let focused = win.focused_pane;
                    if let Some(neighbor) = win.layout.find_neighbor(area, focused, dir) {
                        win.layout.swap_panes(focused, neighbor);
                        // Resize all PTYs to match swapped positions
                        let rects = session.resolve_layout(cs.cols, pane_rows);
                        let win = session.active_window_mut();
                        for (pid, rect) in &rects {
                            if let Some(pane) = win.panes.get_mut(pid) {
                                let _ = pane.resize(rect.cols, rect.rows);
                            }
                        }
                    }
                    return build_render_msg(session, cs.cols, cs.rows, &st.config.status_bar);
                }
            }
            ServerMsg::Error { msg: "No session attached".into() }
        }
        ClientMsg::RespawnPane => {
            if let Some(sid) = cs.attached_session {
                if let Some(session) = st.sessions.get_mut(&sid) {
                    let pane_rows = cs.rows.saturating_sub(1);
                    let rects = session.resolve_layout(cs.cols, pane_rows);
                    let win = session.active_window_mut();
                    let focused = win.focused_pane;

                    // Find the rect for the focused pane
                    let rect = rects.iter().find(|(pid, _)| *pid == focused).map(|(_, r)| *r);
                    let (pcols, prows) = rect.map(|r| (r.cols, r.rows)).unwrap_or((cs.cols, pane_rows));

                    // Remove old pane and spawn a fresh one with the same ID
                    win.panes.remove(&focused);
                    match Pane::spawn(focused, pcols, prows, &st.config.default_shell) {
                        Ok(pane) => {
                            win.panes.insert(focused, pane);
                        }
                        Err(e) => return ServerMsg::Error { msg: e.to_string() },
                    }
                    return build_render_msg(session, cs.cols, cs.rows, &st.config.status_bar);
                }
            }
            ServerMsg::Error { msg: "No session attached".into() }
        }
        ClientMsg::Detach => {
            // Auto-save session on detach
            if let Some(sid) = cs.attached_session {
                if let Some(session) = st.sessions.get(&sid) {
                    if let Err(e) = save_session_to_disk(session) {
                        warn!("Failed to auto-save session on detach: {e}");
                    } else {
                        info!("Auto-saved session '{}' on detach", session.name);
                    }
                }
                // Dispatch session detach hook
                let actions = st.plugins.dispatch_hook(HookEvent::SessionDetach, &HookContext {
                    session_id: Some(sid.0),
                    ..Default::default()
                });
                process_plugin_actions(st, cs, actions);
            }
            ServerMsg::Detached
        }
        ClientMsg::SaveSession => {
            if let Some(sid) = cs.attached_session {
                if let Some(session) = st.sessions.get(&sid) {
                    match save_session_to_disk(session) {
                        Ok(()) => {
                            info!("Saved session '{}'", session.name);
                            ServerMsg::SessionSaved
                        }
                        Err(e) => ServerMsg::Error { msg: format!("Failed to save session: {e}") },
                    }
                } else {
                    ServerMsg::Error { msg: "Session not found".into() }
                }
            } else {
                ServerMsg::Error { msg: "No session attached".into() }
            }
        }
        ClientMsg::RestoreSession { name } => {
            match restore_session_from_disk(st, cs, &name) {
                Ok(msg) => msg,
                Err(e) => ServerMsg::Error { msg: format!("Failed to restore session: {e}") },
            }
        }
        ClientMsg::ListSavedSessions => {
            match list_saved_sessions() {
                Ok(list) => ServerMsg::SavedSessions { list },
                Err(e) => ServerMsg::Error { msg: format!("Failed to list saved sessions: {e}") },
            }
        }
        ClientMsg::SourceConfig { path } => {
            let result = if let Some(ref p) = path {
                vtx_core::lua_config::load_from_path(std::path::Path::new(p))
            } else {
                // Reload from default config path
                let lua_cfg = vtx_core::lua_config::load();
                Ok(lua_cfg)
            };
            match result {
                Ok(lua_cfg) => {
                    st.config.reload_from_lua(lua_cfg);
                    info!("Config reloaded{}", path.map(|p| format!(" from {p}")).unwrap_or_default());
                    ServerMsg::ConfigReloaded
                }
                Err(e) => ServerMsg::Error { msg: format!("Config reload failed: {e}") },
            }
        }
        ClientMsg::KillSession { name } => {
            let found = st.sessions.iter()
                .find(|(_, s)| s.name == name)
                .map(|(id, _)| *id);
            if let Some(sid) = found {
                st.sessions.remove(&sid);
                if cs.attached_session == Some(sid) {
                    cs.attached_session = None;
                }
                info!("Killed session '{name}'");
                ServerMsg::SessionKilled { name }
            } else {
                ServerMsg::Error { msg: format!("Session '{name}' not found") }
            }
        }
        ClientMsg::SwitchTheme { name } => {
            if let Some(theme) = vtx_core::lua_config::find_theme(&name) {
                st.config.status_bg = theme.status_bg.as_tuple();
                st.config.status_fg = theme.status_fg.as_tuple();
                st.config.status_bar = theme.bar;
                st.active_theme = theme.name.to_string();
                info!("Switched theme to '{}'", theme.name);
                ServerMsg::ConfigReloaded
            } else {
                ServerMsg::Error { msg: format!("Unknown theme: {name}") }
            }
        }
        ClientMsg::ListThemes => {
            let themes = vtx_core::lua_config::builtin_themes()
                .iter()
                .map(|t| t.name.to_string())
                .collect();
            ServerMsg::ThemeList { themes, active: st.active_theme.clone() }
        }
        ClientMsg::KillServer => {
            info!("Server shutdown requested by client");
            // Clean up socket and exit
            let socket_path = st.config.socket_path.clone();
            let _ = std::fs::remove_file(&socket_path);
            // Send response then exit
            // We use std::process::exit after sending the response
            // The caller will handle sending ServerShutdown and then the process exits
            ServerMsg::ServerShutdown
        }
    }
}

/// Process plugin actions returned by hook dispatch.
fn process_plugin_actions(
    st: &mut ServerState,
    cs: &mut ClientState,
    actions: Vec<vtx_plugin::lua_plugin::PluginAction>,
) {
    use vtx_plugin::lua_plugin::PluginAction;

    for action in actions {
        match action {
            PluginAction::Notify { message } => {
                info!("[plugin] {message}");
            }
            PluginAction::Split { horizontal } => {
                if let Some(sid) = cs.attached_session {
                    if let Some(session) = st.sessions.get_mut(&sid) {
                        let target = session.active_window().focused_pane;
                        let dir = if horizontal {
                            vtx_layout::SplitDir::Horizontal
                        } else {
                            vtx_layout::SplitDir::Vertical
                        };
                        let pane_rows = cs.rows.saturating_sub(1);
                        let _ = session.split_pane(target, dir, &st.config.default_shell, cs.cols, pane_rows);
                    }
                }
            }
            PluginAction::SendKeys { pane_id, data } => {
                if let Some(sid) = cs.attached_session {
                    if let Some(session) = st.sessions.get_mut(&sid) {
                        let pid = PaneId(pane_id);
                        let win = session.active_window_mut();
                        if let Some(pane) = win.panes.get_mut(&pid) {
                            let _ = pane.write_input(&data);
                        }
                    }
                }
            }
            PluginAction::NewWindow { name } => {
                if let Some(sid) = cs.attached_session {
                    if let Some(session) = st.sessions.get_mut(&sid) {
                        let new_id = session.alloc_pane_id();
                        let pane_rows = cs.rows.saturating_sub(1);
                        if let Ok(pane) = Pane::spawn(new_id, cs.cols, pane_rows, &st.config.default_shell) {
                            let mut panes = HashMap::new();
                            panes.insert(new_id, pane);
                            let window = crate::session::Window {
                                name: name.unwrap_or_else(|| "zsh".to_string()),
                                layout: vtx_layout::LayoutNode::single(new_id),
                                panes,
                                focused_pane: new_id,
                                zoomed_pane: None,
                                popup: None,
                                current_preset: None,
                            };
                            session.windows.push(window);
                            session.active_window = session.windows.len() - 1;
                        }
                    }
                }
            }
            PluginAction::RunCommand { command } => {
                if let Some(sid) = cs.attached_session {
                    if let Some(session) = st.sessions.get_mut(&sid) {
                        let target = session.active_window().focused_pane;
                        let new_id = session.alloc_pane_id();
                        let pane_rows = cs.rows.saturating_sub(1);
                        let shell_cmd = format!("sh -c '{}'", command.replace('\'', "'\\''"));
                        if let Ok(pane) = Pane::spawn(new_id, cs.cols, pane_rows, &shell_cmd) {
                            let win = session.active_window_mut();
                            win.panes.insert(new_id, pane);
                            win.layout.split(target, new_id, vtx_layout::SplitDir::Vertical);
                            win.focused_pane = new_id;
                        }
                    }
                }
            }
            PluginAction::SetLayout { preset } => {
                if let Some(sid) = cs.attached_session {
                    if let Some(session) = st.sessions.get_mut(&sid) {
                        let lp = match preset.as_str() {
                            "even-horizontal" | "even-h" => Some(LayoutPreset::EvenHorizontal),
                            "even-vertical" | "even-v" => Some(LayoutPreset::EvenVertical),
                            "main-vertical" | "main-v" => Some(LayoutPreset::MainVertical),
                            "main-horizontal" | "main-h" => Some(LayoutPreset::MainHorizontal),
                            "tiled" => Some(LayoutPreset::Tiled),
                            _ => None,
                        };
                        if let Some(lp) = lp {
                            let win = session.active_window_mut();
                            win.current_preset = Some(lp);
                            let pane_ids = win.layout.pane_ids();
                            win.layout = vtx_layout::build_preset(&lp, &pane_ids);
                        }
                    }
                }
            }
            PluginAction::RenameWindow { name } => {
                if let Some(sid) = cs.attached_session {
                    if let Some(session) = st.sessions.get_mut(&sid) {
                        session.active_window_mut().name = name;
                    }
                }
            }
            PluginAction::ZoomPane => {
                if let Some(sid) = cs.attached_session {
                    if let Some(session) = st.sessions.get_mut(&sid) {
                        let win = session.active_window_mut();
                        let focused = win.focused_pane;
                        if win.zoomed_pane == Some(focused) {
                            win.zoomed_pane = None;
                        } else {
                            win.zoomed_pane = Some(focused);
                        }
                    }
                }
            }
            PluginAction::SelectWindow { index } => {
                if let Some(sid) = cs.attached_session {
                    if let Some(session) = st.sessions.get_mut(&sid) {
                        if index < session.windows.len() {
                            session.active_window = index;
                        }
                    }
                }
            }
            PluginAction::KillPane => {
                if let Some(sid) = cs.attached_session {
                    if let Some(session) = st.sessions.get_mut(&sid) {
                        let target = session.active_window().focused_pane;
                        let win = session.active_window_mut();
                        win.panes.remove(&target);
                        if !win.panes.is_empty() {
                            win.layout.remove(target);
                            let first = *win.panes.keys().next().unwrap();
                            win.focused_pane = first;
                        }
                    }
                }
            }
            PluginAction::Popup { command } => {
                if let Some(sid) = cs.attached_session {
                    if let Some(session) = st.sessions.get_mut(&sid) {
                        let pane_area_rows = cs.rows.saturating_sub(1);
                        let popup_cols = (cs.cols as f32 * 0.8) as u16;
                        let popup_rows = (pane_area_rows as f32 * 0.8) as u16;
                        let popup_x = (cs.cols.saturating_sub(popup_cols)) / 2;
                        let popup_y = (pane_area_rows.saturating_sub(popup_rows)) / 2;
                        let new_id = session.alloc_pane_id();
                        let shell = command.unwrap_or_else(|| st.config.default_shell.clone());
                        if let Ok(pane) = Pane::spawn(new_id, popup_cols, popup_rows, &shell) {
                            let win = session.active_window_mut();
                            if let Some((old_id, _)) = win.popup.take() {
                                win.panes.remove(&old_id);
                            }
                            win.panes.insert(new_id, pane);
                            win.popup = Some((new_id, vtx_core::ipc::PopupRect {
                                x: popup_x, y: popup_y, cols: popup_cols, rows: popup_rows,
                            }));
                        }
                    }
                }
            }
        }
    }
}

/// Directory where saved sessions are stored.
fn sessions_dir() -> PathBuf {
    let config_dir = std::env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
            PathBuf::from(home).join(".config")
        });
    config_dir.join("vtx").join("sessions")
}

/// Save a session's state to disk as JSON.
fn save_session_to_disk(session: &Session) -> std::result::Result<(), String> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let mut saved_windows = Vec::new();
    for win in &session.windows {
        let mut saved_panes = Vec::new();
        for (pid, pane) in &win.panes {
            saved_panes.push(SavedPane {
                id: pid.0,
                cwd: pane.read_cwd(),
                command: None, // Could be extended to read /proc/<pid>/cmdline
            });
        }
        let layout_json = serde_json::to_string(&win.layout)
            .map_err(|e| format!("layout serialize: {e}"))?;
        saved_windows.push(SavedWindow {
            name: win.name.clone(),
            panes: saved_panes,
            layout: layout_json,
            focused_pane: win.focused_pane.0,
        });
    }

    let saved = SavedSession {
        name: session.name.clone(),
        windows: saved_windows,
        active_window: session.active_window,
        saved_at: now,
    };

    let dir = sessions_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("create dir: {e}"))?;
    let path = dir.join(format!("{}.json", session.name));
    let json = serde_json::to_string_pretty(&saved)
        .map_err(|e| format!("serialize: {e}"))?;
    std::fs::write(&path, json).map_err(|e| format!("write: {e}"))?;
    Ok(())
}

/// Restore a session from a saved JSON file on disk.
fn restore_session_from_disk(
    st: &mut ServerState,
    cs: &mut ClientState,
    name: &str,
) -> std::result::Result<ServerMsg, String> {
    let path = sessions_dir().join(format!("{name}.json"));
    let json = std::fs::read_to_string(&path)
        .map_err(|e| format!("read {}: {e}", path.display()))?;
    let saved: SavedSession = serde_json::from_str(&json)
        .map_err(|e| format!("parse: {e}"))?;

    let sid = SessionId(st.next_session_id);
    st.next_session_id += 1;

    let pane_rows = cs.rows.saturating_sub(1);
    let shell = st.config.default_shell.clone();

    // Build the first window's first pane to create the session
    let first_saved_win = saved.windows.first()
        .ok_or_else(|| "Saved session has no windows".to_string())?;
    let first_saved_pane = first_saved_win.panes.first()
        .ok_or_else(|| "Saved window has no panes".to_string())?;

    let first_pane_id = PaneId(first_saved_pane.id);
    let first_pane = if let Some(ref cwd) = first_saved_pane.cwd {
        Pane::spawn_in_cwd(first_pane_id, cs.cols, pane_rows, &shell, cwd)
    } else {
        Pane::spawn(first_pane_id, cs.cols, pane_rows, &shell)
    }.map_err(|e| format!("spawn pane: {e}"))?;

    let mut session = Session::new(sid, saved.name.clone(), first_pane);

    // Restore the first window's remaining panes and layout
    {
        let win = &mut session.windows[0];
        win.name = first_saved_win.name.clone();

        // Spawn remaining panes for the first window
        for sp in first_saved_win.panes.iter().skip(1) {
            let pane_id = PaneId(sp.id);
            let pane = if let Some(ref cwd) = sp.cwd {
                Pane::spawn_in_cwd(pane_id, cs.cols, pane_rows, &shell, cwd)
            } else {
                Pane::spawn(pane_id, cs.cols, pane_rows, &shell)
            }.map_err(|e| format!("spawn pane: {e}"))?;
            win.panes.insert(pane_id, pane);
        }

        // Restore layout
        if let Ok(layout) = serde_json::from_str(&first_saved_win.layout) {
            win.layout = layout;
        }
        win.focused_pane = PaneId(first_saved_win.focused_pane);
    }

    // Restore additional windows
    for saved_win in saved.windows.iter().skip(1) {
        if saved_win.panes.is_empty() {
            continue;
        }
        let first_sp = &saved_win.panes[0];
        let first_id = PaneId(first_sp.id);
        let first_pane = if let Some(ref cwd) = first_sp.cwd {
            Pane::spawn_in_cwd(first_id, cs.cols, pane_rows, &shell, cwd)
        } else {
            Pane::spawn(first_id, cs.cols, pane_rows, &shell)
        }.map_err(|e| format!("spawn pane: {e}"))?;

        let mut panes = HashMap::new();
        panes.insert(first_id, first_pane);

        for sp in saved_win.panes.iter().skip(1) {
            let pane_id = PaneId(sp.id);
            let pane = if let Some(ref cwd) = sp.cwd {
                Pane::spawn_in_cwd(pane_id, cs.cols, pane_rows, &shell, cwd)
            } else {
                Pane::spawn(pane_id, cs.cols, pane_rows, &shell)
            }.map_err(|e| format!("spawn pane: {e}"))?;
            panes.insert(pane_id, pane);
        }

        let layout = serde_json::from_str(&saved_win.layout)
            .unwrap_or_else(|_| vtx_layout::LayoutNode::single(first_id));

        let win = crate::session::Window {
            name: saved_win.name.clone(),
            layout,
            panes,
            focused_pane: PaneId(saved_win.focused_pane),
            zoomed_pane: None,
            popup: None,
            current_preset: None,
        };
        session.windows.push(win);
    }

    // Restore active window index
    if saved.active_window < session.windows.len() {
        session.active_window = saved.active_window;
    }

    st.sessions.insert(sid, session);
    cs.attached_session = Some(sid);
    info!("Restored session '{}' as {sid}", saved.name);

    Ok(ServerMsg::SessionReady {
        session: sid,
        cols: cs.cols,
        rows: cs.rows,
    })
}

/// List saved session names from disk.
fn list_saved_sessions() -> std::result::Result<Vec<String>, String> {
    let dir = sessions_dir();
    if !dir.exists() {
        return Ok(vec![]);
    }
    let mut names = Vec::new();
    let entries = std::fs::read_dir(&dir)
        .map_err(|e| format!("read dir: {e}"))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("read entry: {e}"))?;
        let path = entry.path();
        if path.extension().map(|e| e == "json").unwrap_or(false) {
            if let Some(stem) = path.file_stem() {
                names.push(stem.to_string_lossy().into_owned());
            }
        }
    }
    names.sort();
    Ok(names)
}

fn build_render_msg(session: &Session, cols: u16, total_rows: u16, status_cfg: &vtx_core::lua_config::StatusBarConfig) -> ServerMsg {
    build_render_msg_scrolled(session, cols, total_rows, 0, status_cfg)
}

fn build_render_msg_scrolled(session: &Session, cols: u16, total_rows: u16, scroll_offset: i32, status_cfg: &vtx_core::lua_config::StatusBarConfig) -> ServerMsg {
    let pane_area_rows = total_rows.saturating_sub(1);
    let offset = scroll_offset.max(0) as usize;
    let win = session.active_window();
    let focused_id = win.focused_pane;

    // If a pane is zoomed, render only that pane at full size with no borders.
    if let Some(zoomed_id) = win.zoomed_pane {
        let panes: Vec<PaneRender> = win
            .panes
            .get(&zoomed_id)
            .map(|pane| {
                let content = if zoomed_id == focused_id && offset > 0 {
                    pane.parser.grid.content_cells_scrolled(offset)
                } else {
                    pane.parser.grid.content_cells()
                };
                let cursor_visible = if zoomed_id == focused_id && offset > 0 {
                    false
                } else {
                    pane.parser.grid.cursor_visible
                };
                vec![PaneRender {
                    id: zoomed_id,
                    x: 0,
                    y: 0,
                    cols,
                    rows: pane_area_rows,
                    content,
                    cursor_x: pane.parser.grid.cursor_x,
                    cursor_y: pane.parser.grid.cursor_y,
                    cursor_visible,
                    floating: false,
                }]
            })
            .unwrap_or_default();

        let status = build_status_bar(session, status_cfg, Some("[Z]"));

        return ServerMsg::Render {
            panes,
            focused: focused_id,
            borders: vec![],
            status,
            total_rows,
        };
    }

    let area = Rect { x: 0, y: 0, cols, rows: pane_area_rows };

    let rects = win.layout.resolve(area);
    let borders = win.layout.borders(area);

    let mut panes: Vec<PaneRender> = rects
        .iter()
        .filter_map(|(pid, rect)| {
            win.panes.get(pid).map(|pane| {
                // Only apply scroll offset to the focused pane
                let content = if *pid == focused_id && offset > 0 {
                    pane.parser.grid.content_cells_scrolled(offset)
                } else {
                    pane.parser.grid.content_cells()
                };
                let cursor_visible = if *pid == focused_id && offset > 0 {
                    false // hide cursor when scrolling
                } else {
                    pane.parser.grid.cursor_visible
                };
                PaneRender {
                    id: *pid,
                    x: rect.x,
                    y: rect.y,
                    cols: rect.cols,
                    rows: rect.rows,
                    content,
                    cursor_x: pane.parser.grid.cursor_x,
                    cursor_y: pane.parser.grid.cursor_y,
                    cursor_visible,
                    floating: false,
                }
            })
        })
        .collect();

    // If there's a popup, add it as a floating pane
    if let Some((popup_id, popup_rect)) = &win.popup {
        if let Some(pane) = win.panes.get(popup_id) {
            panes.push(PaneRender {
                id: *popup_id,
                x: popup_rect.x,
                y: popup_rect.y,
                cols: popup_rect.cols,
                rows: popup_rect.rows,
                content: pane.parser.grid.content_cells(),
                cursor_x: pane.parser.grid.cursor_x,
                cursor_y: pane.parser.grid.cursor_y,
                cursor_visible: pane.parser.grid.cursor_visible,
                floating: true,
            });
        }
    }

    let popup_indicator = if win.popup.is_some() { Some("[POPUP]") } else { None };
    let status = build_status_bar(session, status_cfg, popup_indicator);

    let border_data: Vec<(u16, u16, u16, bool)> = borders
        .iter()
        .map(|b| (b.x, b.y, b.length, b.horizontal))
        .collect();

    // When popup is active, focus the popup pane for cursor positioning
    let effective_focused = if let Some((popup_id, _)) = win.popup {
        popup_id
    } else {
        win.focused_pane
    };

    ServerMsg::Render {
        panes,
        focused: effective_focused,
        borders: border_data,
        status,
        total_rows,
    }
}

/// Build the styled status bar from the user's StatusBarConfig segments.
fn build_status_bar(session: &Session, status_cfg: &vtx_core::lua_config::StatusBarConfig, extra: Option<&str>) -> StyledStatus {
    use crate::status;

    let win = session.active_window();

    // Helpers: resolve template variables in a segment's text
    let resolve_var = |var: &str| -> Option<String> {
        match var {
            "session" => Some(session.name.clone()),
            "windows" => {
                let tabs: String = session
                    .windows
                    .iter()
                    .enumerate()
                    .map(|(i, w)| {
                        if i == session.active_window {
                            format!("{}:{}*", i, w.name)
                        } else {
                            format!("{}:{}", i, w.name)
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(" \u{2502} ");
                Some(tabs)
            }
            "git" => {
                let g = win.panes.get(&win.focused_pane)
                    .and_then(|p| p.read_cwd())
                    .and_then(|cwd| status::git_info(std::path::Path::new(&cwd)))?;
                let mut s = format!("\u{e0a0} {}", g.branch);
                if g.dirty { s.push('*'); }
                if g.ahead > 0 { s.push_str(&format!("\u{2191}{}", g.ahead)); }
                if g.behind > 0 { s.push_str(&format!("\u{2193}{}", g.behind)); }
                Some(s)
            }
            "cpu" => {
                let sys = status::sys_info();
                Some(format!("cpu {:.0}%", sys.cpu_pct))
            }
            "mem" => {
                let sys = status::sys_info();
                Some(format!("{}/{}", status::format_mem(sys.mem_used_mb), status::format_mem(sys.mem_total_mb)))
            }
            "time" => Some(status::local_time()),
            "pane" => Some(format!("{}", win.focused_pane)),
            "cwd" => {
                let cwd = win.panes.get(&win.focused_pane)
                    .and_then(|p| p.read_cwd())?;
                let home = std::env::var("HOME").unwrap_or_default();
                if !home.is_empty() && cwd.starts_with(&home) {
                    Some(format!("~{}", &cwd[home.len()..]))
                } else {
                    Some(cwd)
                }
            }
            _ => None,
        }
    };

    // Resolve all template #{...} placeholders in a text string.
    // Returns None if the resolved text ends up empty (e.g., #{git} when not in repo).
    let resolve_text = |text: &str| -> Option<String> {
        let mut result = String::new();
        let mut rest = text;
        let mut had_var = false;
        let mut any_empty_var = false;
        while let Some(start) = rest.find("#{") {
            result.push_str(&rest[..start]);
            let after = &rest[start + 2..];
            if let Some(end) = after.find('}') {
                let var_name = &after[..end];
                had_var = true;
                match resolve_var(var_name) {
                    Some(val) if !val.is_empty() => result.push_str(&val),
                    _ => { any_empty_var = true; }
                }
                rest = &after[end + 1..];
            } else {
                result.push_str(&rest[..start + 2]);
                rest = after;
            }
        }
        result.push_str(rest);
        // If the segment was purely a template variable and it resolved empty, skip it
        if had_var && any_empty_var && result.trim().is_empty() {
            return None;
        }
        Some(result)
    };

    // Build individual window tab segments (clickable).
    let build_window_tabs = |base_fg: (u8, u8, u8), base_bg: (u8, u8, u8), bold: bool| -> Vec<StatusSegment> {
        session.windows.iter().enumerate().map(|(i, w)| {
            let is_active = i == session.active_window;
            let label = if is_active {
                format!(" {}:{}* ", i, w.name)
            } else {
                format!(" {}:{} ", i, w.name)
            };
            // Active tab gets the configured colors; inactive gets dimmed
            let (fg, bg) = if is_active {
                (base_fg, base_bg)
            } else {
                // Dim: blend toward the status bar background
                let dim_fg = (
                    ((base_fg.0 as u16 + status_cfg.bg.as_tuple().0 as u16) / 2) as u8,
                    ((base_fg.1 as u16 + status_cfg.bg.as_tuple().1 as u16) / 2) as u8,
                    ((base_fg.2 as u16 + status_cfg.bg.as_tuple().2 as u16) / 2) as u8,
                );
                let dim_bg = (
                    ((base_bg.0 as u16 + status_cfg.bg.as_tuple().0 as u16) / 2) as u8,
                    ((base_bg.1 as u16 + status_cfg.bg.as_tuple().1 as u16) / 2) as u8,
                    ((base_bg.2 as u16 + status_cfg.bg.as_tuple().2 as u16) / 2) as u8,
                );
                (dim_fg, dim_bg)
            };
            StatusSegment {
                text: label,
                fg,
                bg,
                bold: bold && is_active,
                click: Some(format!("select-window-{}", i)),
            }
        }).collect()
    };

    let build_segments = |defs: &[vtx_core::lua_config::SegmentDef]| -> Vec<StatusSegment> {
        let mut out = Vec::new();
        for seg in defs {
            // Special case: if the segment text is purely #{windows}, expand to per-tab segments
            let trimmed = seg.text.trim();
            if trimmed == "#{windows}" {
                let tabs = build_window_tabs(seg.fg.as_tuple(), seg.bg.as_tuple(), seg.bold);
                out.extend(tabs);
                continue;
            }
            if let Some(text) = resolve_text(&seg.text) {
                if text.is_empty() {
                    continue;
                }
                out.push(StatusSegment {
                    text,
                    fg: seg.fg.as_tuple(),
                    bg: seg.bg.as_tuple(),
                    bold: seg.bold,
                    click: None,
                });
            }
        }
        out
    };

    let mut left = build_segments(&status_cfg.left);
    let right = build_segments(&status_cfg.right);

    // Add [+] new tab button after the left segments
    left.push(StatusSegment {
        text: " + ".to_string(),
        fg: status_cfg.bg.as_tuple(),
        bg: (0x56, 0x5f, 0x89), // muted accent
        bold: true,
        click: Some("new-window".to_string()),
    });

    // Append extra indicator (e.g., "[PREFIX]") as a bright left segment
    if let Some(e) = extra {
        left.push(StatusSegment {
            text: format!(" {} ", e),
            fg: (0x1a, 0x1b, 0x26),
            bg: (0xe0, 0xaf, 0x68),
            bold: true,
            click: None,
        });
    }

    StyledStatus {
        left,
        right,
        bg: status_cfg.bg.as_tuple(),
    }
}

/// Discover and load plugins from `~/.config/vtx/plugins/`.
fn load_plugins_from_dir(mgr: &mut PluginManager) {
    let plugin_dir = match dirs_path("plugins") {
        Some(p) => p,
        None => return,
    };

    if !plugin_dir.is_dir() {
        return;
    }

    let entries = match std::fs::read_dir(&plugin_dir) {
        Ok(e) => e,
        Err(e) => {
            warn!("Failed to read plugin dir {}: {e}", plugin_dir.display());
            return;
        }
    };

    for entry in entries.flatten() {
        let path = entry.path();
        match path.extension().and_then(|e| e.to_str()) {
            Some("lua") => {
                match mgr.load_lua_plugin(&path) {
                    Ok(name) => info!("Loaded Lua plugin: {name}"),
                    Err(e) => warn!("Failed to load {}: {e}", path.display()),
                }
            }
            Some("wasm") => {
                match mgr.load_wasm_plugin(&path) {
                    Ok(name) => info!("Loaded WASM plugin: {name}"),
                    Err(e) => warn!("Failed to load {}: {e}", path.display()),
                }
            }
            _ => {}
        }
    }
}

/// Get a path under `~/.config/vtx/<subdir>`.
fn dirs_path(subdir: &str) -> Option<PathBuf> {
    let home = std::env::var("HOME").ok()?;
    Some(PathBuf::from(home).join(".config").join("vtx").join(subdir))
}
