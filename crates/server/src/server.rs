use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{mpsc, Mutex};
use tracing::{error, info};

use vtx_core::config::Config;
use vtx_core::ipc::{ClientMsg, PaneRender, ServerMsg, SessionInfo};
use vtx_core::{PaneId, SessionId};
use vtx_layout::Rect;

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
}

pub struct VtxServer {
    state: Arc<Mutex<ServerState>>,
}

impl VtxServer {
    pub fn new(config: Config) -> Self {
        VtxServer {
            state: Arc::new(Mutex::new(ServerState {
                config,
                sessions: HashMap::new(),
                next_session_id: 0,
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
                for pane in session.panes.values_mut() {
                    if pane.drain_output() {
                        any_output = true;
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
                        let msg = build_render_msg_scrolled(session, cs.cols, cs.rows, cs.scroll_offset);
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

                        let mut json = serde_json::to_string(&response).unwrap();
                        json.push('\n');
                        let mut w = writer.lock().await;
                        if w.write_all(json.as_bytes()).await.is_err() {
                            break;
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
            let sid = SessionId(st.next_session_id);
            st.next_session_id += 1;

            let pane_id = PaneId(0);
            // Use client's terminal size minus 1 row for status bar
            let pane_rows = cs.rows.saturating_sub(1);

            match Pane::spawn(pane_id, cs.cols, pane_rows, &st.config.default_shell) {
                Ok(pane) => {
                    let session_name = name.unwrap_or_else(|| format!("vtx-{}", sid.0));
                    let session = Session::new(sid, session_name, pane);
                    st.sessions.insert(sid, session);
                    cs.attached_session = Some(sid);
                    info!("Created session {sid}");
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
                    pane_count: s.panes.len(),
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
                    let focused = session.focused_pane;
                    if let Some(pane) = session.panes.get_mut(&focused) {
                        let _ = pane.write_input(&data);
                    }
                    return build_render_msg(session, cs.cols, cs.rows);
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
                    for (pid, rect) in &rects {
                        if let Some(pane) = session.panes.get_mut(pid) {
                            let _ = pane.resize(rect.cols, rect.rows);
                        }
                    }
                    return build_render_msg(session, cols, rows);
                }
            }
            ServerMsg::Error { msg: "No session attached".into() }
        }
        ClientMsg::Split { horizontal } => {
            if let Some(sid) = cs.attached_session {
                if let Some(session) = st.sessions.get_mut(&sid) {
                    let target = session.focused_pane;
                    let dir = if horizontal {
                        vtx_layout::SplitDir::Horizontal
                    } else {
                        vtx_layout::SplitDir::Vertical
                    };

                    // Calculate what size the new pane should be
                    let pane_rows = cs.rows.saturating_sub(1);
                    match session.split_pane(target, dir, &st.config.default_shell, cs.cols, pane_rows) {
                        Ok(_) => {
                            // Now resize all panes to their layout rects
                            let rects = session.resolve_layout(cs.cols, pane_rows);
                            for (pid, rect) in &rects {
                                if let Some(pane) = session.panes.get_mut(pid) {
                                    let _ = pane.resize(rect.cols, rect.rows);
                                }
                            }
                            return build_render_msg(session, cs.cols, cs.rows);
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
                    if let Some(next) = session.layout.find_neighbor(area, session.focused_pane, dir) {
                        session.focused_pane = next;
                    }
                    return build_render_msg(session, cs.cols, cs.rows);
                }
            }
            ServerMsg::Error { msg: "No session attached".into() }
        }
        ClientMsg::FocusPane { pane } => {
            if let Some(sid) = cs.attached_session {
                if let Some(session) = st.sessions.get_mut(&sid) {
                    if session.panes.contains_key(&pane) {
                        session.focused_pane = pane;
                        return build_render_msg(session, cs.cols, cs.rows);
                    }
                }
            }
            ServerMsg::Error { msg: "Pane not found".into() }
        }
        ClientMsg::KillPane => {
            if let Some(sid) = cs.attached_session {
                if let Some(session) = st.sessions.get_mut(&sid) {
                    let target = session.focused_pane;
                    session.panes.remove(&target);

                    if session.panes.is_empty() {
                        // Last pane — remove session
                        st.sessions.remove(&sid);
                        cs.attached_session = None;
                        return ServerMsg::Detached;
                    }

                    // Remove from layout tree (collapses the split node)
                    session.layout.remove(target);

                    // Focus the first remaining pane
                    let first = *session.panes.keys().next().unwrap();
                    session.focused_pane = first;

                    // Resize remaining panes to fill the space
                    let pane_rows = cs.rows.saturating_sub(1);
                    let rects = session.resolve_layout(cs.cols, pane_rows);
                    for (pid, rect) in &rects {
                        if let Some(pane) = session.panes.get_mut(pid) {
                            let _ = pane.resize(rect.cols, rect.rows);
                        }
                    }
                    return build_render_msg(session, cs.cols, cs.rows);
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
                    session.layout.resize_pane(session.focused_pane, dir, delta);

                    // Resize all PTYs to match new layout
                    let rects = session.resolve_layout(cs.cols, pane_rows);
                    for (pid, rect) in &rects {
                        if let Some(pane) = session.panes.get_mut(pid) {
                            let _ = pane.resize(rect.cols, rect.rows);
                        }
                    }
                    return build_render_msg(session, cs.cols, cs.rows);
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
                            let target = session.focused_pane;
                            session.panes.insert(new_id, pane);
                            session.layout.split(target, new_id, vtx_layout::SplitDir::Vertical);
                            session.focused_pane = new_id;

                            // Resize all panes to their layout rects
                            let rects = session.resolve_layout(cs.cols, pane_rows);
                            for (pid, rect) in &rects {
                                if let Some(p) = session.panes.get_mut(pid) {
                                    let _ = p.resize(rect.cols, rect.rows);
                                }
                            }
                            return build_render_msg(session, cs.cols, cs.rows);
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
                            let target = session.focused_pane;
                            session.panes.insert(new_id, pane);
                            session
                                .layout
                                .split(target, new_id, vtx_layout::SplitDir::Vertical);
                            session.focused_pane = new_id;

                            // Resize all panes to their layout rects
                            let rects = session.resolve_layout(cs.cols, pane_rows);
                            for (pid, rect) in &rects {
                                if let Some(p) = session.panes.get_mut(pid) {
                                    let _ = p.resize(rect.cols, rect.rows);
                                }
                            }
                            return build_render_msg(session, cs.cols, cs.rows);
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
                    return build_render_msg_scrolled(session, cs.cols, cs.rows, offset);
                }
            }
            ServerMsg::Error { msg: "No session attached".into() }
        }
        ClientMsg::Detach => ServerMsg::Detached,
    }
}

fn build_render_msg(session: &Session, cols: u16, total_rows: u16) -> ServerMsg {
    build_render_msg_scrolled(session, cols, total_rows, 0)
}

fn build_render_msg_scrolled(session: &Session, cols: u16, total_rows: u16, scroll_offset: i32) -> ServerMsg {
    let pane_area_rows = total_rows.saturating_sub(1);
    let area = Rect { x: 0, y: 0, cols, rows: pane_area_rows };

    let rects = session.layout.resolve(area);
    let borders = session.layout.borders(area);

    let offset = scroll_offset.max(0) as usize;
    let focused_id = session.focused_pane;

    let panes: Vec<PaneRender> = rects
        .iter()
        .filter_map(|(pid, rect)| {
            session.panes.get(pid).map(|pane| {
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
                }
            })
        })
        .collect();

    let pane_count = session.panes.len();
    let focused_idx = rects.iter().position(|(id, _)| *id == session.focused_pane).unwrap_or(0);
    let status = format!(
        "vtx | {} | pane {}/{} [{}]",
        session.name,
        focused_idx + 1,
        pane_count,
        session.focused_pane,
    );

    let border_data: Vec<(u16, u16, u16, bool)> = borders
        .iter()
        .map(|b| (b.x, b.y, b.length, b.horizontal))
        .collect();

    ServerMsg::Render {
        panes,
        focused: session.focused_pane,
        borders: border_data,
        status,
        total_rows,
    }
}
