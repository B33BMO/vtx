use crossterm::event::{
    self, Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tracing::{error, info};

use vtx_core::config::Config;
use vtx_core::ipc::{ClientMsg, Direction, LayoutPreset, ServerMsg, StyledStatus};
use vtx_core::lua_config::KeyBinding;
use vtx_renderer_tty::{Selection, TtyRenderer};

/// Client-side input action after processing keys.
enum InputAction {
    Send(ClientMsg),
    Detach,
    ScrollUp,
    ScrollDown,
    EnterCopyMode,
    EnterSearchMode,
    SourceConfig,
    OpenSettings,
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
    /// Prefix key (e.g. "ctrl-a", "ctrl-b")
    prefix_key: char,
    /// User-defined keybindings from Lua config
    bindings: Vec<KeyBinding>,
    /// Status bar colors
    pub status_bg: (u8, u8, u8),
    pub status_fg: (u8, u8, u8),
}

impl VtxClient {
    pub fn new(config: &Config) -> Self {
        // Parse prefix key character from config (e.g. "ctrl-a" -> 'a')
        let prefix_key = config
            .prefix
            .strip_prefix("ctrl-")
            .and_then(|s| s.chars().next())
            .unwrap_or('a');

        VtxClient {
            socket_path: config.socket_path.clone(),
            prefix_key,
            bindings: config.bindings.clone(),
            status_bg: config.status_bg,
            status_fg: config.status_fg,
        }
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
        renderer.status_fg = vtx_core::cell::Color::Rgb(self.status_fg.0, self.status_fg.1, self.status_fg.2);
        renderer.status_bg = vtx_core::cell::Color::Rgb(self.status_bg.0, self.status_bg.1, self.status_bg.2);

        let mut key_config = KeyConfig {
            prefix_key: self.prefix_key,
            bindings: self.bindings.clone(),
        };
        let mut prefix_active = false;
        let mut scroll_offset: i32 = 0;

        // Search mode state
        let mut search_mode = false;
        let mut search_query = String::new();

        // Mouse selection state
        let mut selection: Option<Selection> = None;
        let mut mouse_dragging = false;
        // Pane bounds for constraining selection (set on mouse down)
        let mut sel_pane_x: u16 = 0;
        let mut sel_pane_y: u16 = 0;
        let mut sel_pane_cols: u16 = 0;
        let mut sel_pane_rows: u16 = 0;

        // Border drag state for divider resizing
        let mut border_dragging = false;
        let mut border_drag_x: u16 = 0;
        let mut border_drag_y: u16 = 0;
        let mut border_drag_horizontal = false;
        let mut border_drag_last_pos: u16 = 0; // last mouse position along drag axis

        // Context menu state
        let mut context_menu_open = false;
        let mut context_menu_x: u16 = 0;
        let mut context_menu_y: u16 = 0;
        let mut context_menu_selected: usize = 0;
        const CONTEXT_MENU_ITEMS: &[&str] = &[
            "Split Horizontal",
            "Split Vertical",
            "Swap Up",
            "Swap Down",
            "Kill Pane",
            "Respawn Pane",
            "Zoom",
        ];

        // Copy mode state
        let mut copy_mode = false;
        let mut copy_cursor_x: u16 = 0;
        let mut copy_cursor_y: u16 = 0;
        let mut copy_selecting = false;
        let mut copy_sel_start_x: u16 = 0;
        let mut copy_sel_start_y: u16 = 0;

        // Settings menu state
        let mut settings_menu_open = false;
        let mut settings_menu_selected: usize = 0;
        let mut settings_themes: Vec<String> = Vec::new();
        let mut settings_active_theme: String = "Tokyo Night".to_string();

        // Store last render frame for re-rendering with selection
        let mut last_frame: Option<(Vec<vtx_core::ipc::PaneRender>, vtx_core::PaneId, Vec<(u16, u16, u16, bool)>, StyledStatus, u16)> = None;

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
                            let status_display = if search_mode {
                                StyledStatus::simple(
                                    &format!("search: {}_", search_query),
                                    status.left.first().map(|s| s.fg).unwrap_or(status.bg),
                                    status.bg,
                                )
                            } else {
                                let mut s = status;
                                if copy_mode {
                                    s.left.push(vtx_core::ipc::StatusSegment {
                                        text: " [COPY] ".to_string(),
                                        fg: (0x1a, 0x1b, 0x26),
                                        bg: (0xbb, 0x9a, 0xf7),
                                        bold: true,
                                        click: None,
                                    });
                                }
                                if scroll_offset > 0 {
                                    s.left.push(vtx_core::ipc::StatusSegment {
                                        text: format!(" [SCROLL: +{}] ", scroll_offset),
                                        fg: (0x1a, 0x1b, 0x26),
                                        bg: (0x7a, 0xa2, 0xf7),
                                        bold: true,
                                        click: None,
                                    });
                                }
                                s
                            };
                            if let Err(e) = renderer.render_frame(
                                &panes, focused, &borders, &status_display, total_rows, prefix_active,
                                selection.as_ref(),
                            ) {
                                error!("Render error: {e}");
                            }
                            last_frame = Some((panes, focused, borders, status_display, total_rows));

                            // If context menu is open, redraw it on top of the new frame
                            if context_menu_open {
                                let _ = renderer.render_context_menu(
                                    context_menu_x, context_menu_y,
                                    CONTEXT_MENU_ITEMS, context_menu_selected,
                                );
                            }
                            // If settings menu is open, redraw it on top of the new frame
                            if settings_menu_open {
                                let _ = renderer.render_settings_menu(
                                    &settings_themes, settings_menu_selected, &settings_active_theme,
                                );
                            }
                        }
                        Some(ServerMsg::SearchResult { offset, matches }) => {
                            scroll_offset = offset;
                            // Request a render at the new scroll position
                            send_msg(&mut writer, &ClientMsg::ScrollBack { offset: scroll_offset }).await?;
                            // Update status bar with match info on next render
                            if matches == 0 {
                                // Re-render status bar with "no matches" info
                                if let Some((ref panes, focused, ref borders, ref _status, total_rows)) = last_frame {
                                    let search_status = StyledStatus::simple(
                                        &format!("search: {} (no matches)", search_query),
                                        _status.left.first().map(|s| s.fg).unwrap_or(_status.bg),
                                        _status.bg,
                                    );
                                    let _ = renderer.render_frame(
                                        panes, focused, borders, &search_status, total_rows, prefix_active,
                                        selection.as_ref(),
                                    );
                                }
                            }
                        }
                        Some(ServerMsg::ThemeList { themes, active }) => {
                            settings_themes = themes;
                            settings_active_theme = active;
                            settings_menu_open = true;
                            settings_menu_selected = settings_themes.iter()
                                .position(|t| t == &settings_active_theme)
                                .unwrap_or(0);
                            // Re-render frame then overlay menu
                            if let Some((ref panes, focused, ref borders, ref status, total_rows)) = last_frame {
                                let _ = renderer.render_frame(
                                    panes, focused, borders, status, total_rows, prefix_active,
                                    selection.as_ref(),
                                );
                            }
                            let _ = renderer.render_settings_menu(
                                &settings_themes, settings_menu_selected, &settings_active_theme,
                            );
                        }
                        Some(ServerMsg::ConfigReloaded) => {
                            // Reload our local config to pick up new prefix, bindings, colors
                            let new_config = vtx_core::config::Config::default();
                            let new_prefix = new_config.prefix
                                .strip_prefix("ctrl-")
                                .and_then(|s| s.chars().next())
                                .unwrap_or('a');
                            key_config = KeyConfig {
                                prefix_key: new_prefix,
                                bindings: new_config.bindings,
                            };
                            renderer.status_fg = vtx_core::cell::Color::Rgb(
                                new_config.status_fg.0, new_config.status_fg.1, new_config.status_fg.2,
                            );
                            renderer.status_bg = vtx_core::cell::Color::Rgb(
                                new_config.status_bg.0, new_config.status_bg.1, new_config.status_bg.2,
                            );
                            renderer.invalidate();
                            info!("Config reloaded — prefix: ctrl-{new_prefix}");
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
                        TermEvent::Mouse(me) if context_menu_open => {
                            // Context menu is open — handle menu mouse interactions
                            let menu_width = CONTEXT_MENU_ITEMS.iter().map(|s| s.len()).max().unwrap_or(0) + 4;
                            let menu_height = CONTEXT_MENU_ITEMS.len() as u16 + 2;
                            let (scols, srows) = renderer.screen_size();
                            let mx = if context_menu_x + menu_width as u16 > scols { scols.saturating_sub(menu_width as u16) } else { context_menu_x };
                            let my = if context_menu_y + menu_height > srows { srows.saturating_sub(menu_height) } else { context_menu_y };

                            match me.kind {
                                MouseEventKind::Down(MouseButton::Left) => {
                                    // Check if click is inside menu
                                    let item_x_start = mx;
                                    let item_x_end = mx + menu_width as u16;
                                    let item_y_start = my + 1;
                                    let item_y_end = my + 1 + CONTEXT_MENU_ITEMS.len() as u16;

                                    if me.column >= item_x_start && me.column < item_x_end
                                        && me.row >= item_y_start && me.row < item_y_end
                                    {
                                        let idx = (me.row - item_y_start) as usize;
                                        context_menu_open = false;
                                        renderer.invalidate();
                                        let action = match idx {
                                            0 => Some(ClientMsg::Split { horizontal: true }),
                                            1 => Some(ClientMsg::Split { horizontal: false }),
                                            2 => Some(ClientMsg::SwapPane { dir: Direction::Up }),
                                            3 => Some(ClientMsg::SwapPane { dir: Direction::Down }),
                                            4 => Some(ClientMsg::KillPane),
                                            5 => Some(ClientMsg::RespawnPane),
                                            6 => Some(ClientMsg::ZoomPane),
                                            _ => None,
                                        };
                                        if let Some(msg) = action {
                                            send_msg(&mut writer, &msg).await?;
                                        }
                                        // Force full re-render to clear menu
                                        if let Some((ref panes, focused, ref borders, ref status, total_rows)) = last_frame {
                                            let _ = renderer.render_frame(
                                                panes, focused, borders, status, total_rows, prefix_active,
                                                selection.as_ref(),
                                            );
                                        }
                                    } else {
                                        // Click outside — close menu
                                        context_menu_open = false;
                                        renderer.invalidate();
                                        if let Some((ref panes, focused, ref borders, ref status, total_rows)) = last_frame {
                                            let _ = renderer.render_frame(
                                                panes, focused, borders, status, total_rows, prefix_active,
                                                selection.as_ref(),
                                            );
                                        }
                                    }
                                }
                                MouseEventKind::Moved | MouseEventKind::Drag(_) => {
                                    // Highlight item under cursor
                                    let item_y_start = my + 1;
                                    let item_y_end = my + 1 + CONTEXT_MENU_ITEMS.len() as u16;
                                    if me.row >= item_y_start && me.row < item_y_end
                                        && me.column >= mx && me.column < mx + menu_width as u16
                                    {
                                        let new_sel = (me.row - item_y_start) as usize;
                                        if new_sel != context_menu_selected {
                                            context_menu_selected = new_sel;
                                            let _ = renderer.render_context_menu(
                                                context_menu_x, context_menu_y,
                                                CONTEXT_MENU_ITEMS, context_menu_selected,
                                            );
                                        }
                                    }
                                }
                                _ => {
                                    // Any other mouse event — close menu
                                    context_menu_open = false;
                                    renderer.invalidate();
                                    if let Some((ref panes, focused, ref borders, ref status, total_rows)) = last_frame {
                                        let _ = renderer.render_frame(
                                            panes, focused, borders, status, total_rows, prefix_active,
                                            selection.as_ref(),
                                        );
                                    }
                                }
                            }
                        }
                        TermEvent::Key(key_event) if context_menu_open => {
                            // Context menu keyboard navigation
                            match key_event.code {
                                KeyCode::Up | KeyCode::Char('k') => {
                                    if context_menu_selected > 0 {
                                        context_menu_selected -= 1;
                                    } else {
                                        context_menu_selected = CONTEXT_MENU_ITEMS.len() - 1;
                                    }
                                    let _ = renderer.render_context_menu(
                                        context_menu_x, context_menu_y,
                                        CONTEXT_MENU_ITEMS, context_menu_selected,
                                    );
                                }
                                KeyCode::Down | KeyCode::Char('j') => {
                                    context_menu_selected = (context_menu_selected + 1) % CONTEXT_MENU_ITEMS.len();
                                    let _ = renderer.render_context_menu(
                                        context_menu_x, context_menu_y,
                                        CONTEXT_MENU_ITEMS, context_menu_selected,
                                    );
                                }
                                KeyCode::Enter => {
                                    context_menu_open = false;
                                    renderer.invalidate();
                                    let action = match context_menu_selected {
                                        0 => Some(ClientMsg::Split { horizontal: true }),
                                        1 => Some(ClientMsg::Split { horizontal: false }),
                                        2 => Some(ClientMsg::SwapPane { dir: Direction::Up }),
                                        3 => Some(ClientMsg::SwapPane { dir: Direction::Down }),
                                        4 => Some(ClientMsg::KillPane),
                                        5 => Some(ClientMsg::RespawnPane),
                                        6 => Some(ClientMsg::ZoomPane),
                                        _ => None,
                                    };
                                    if let Some(msg) = action {
                                        send_msg(&mut writer, &msg).await?;
                                    }
                                    if let Some((ref panes, focused, ref borders, ref status, total_rows)) = last_frame {
                                        let _ = renderer.render_frame(
                                            panes, focused, borders, status, total_rows, prefix_active,
                                            selection.as_ref(),
                                        );
                                    }
                                }
                                KeyCode::Esc | KeyCode::Char('q') => {
                                    context_menu_open = false;
                                    renderer.invalidate();
                                    if let Some((ref panes, focused, ref borders, ref status, total_rows)) = last_frame {
                                        let _ = renderer.render_frame(
                                            panes, focused, borders, status, total_rows, prefix_active,
                                            selection.as_ref(),
                                        );
                                    }
                                }
                                _ => {}
                            }
                        }
                        TermEvent::Key(key_event) if settings_menu_open => {
                            match key_event.code {
                                KeyCode::Up | KeyCode::Char('k') => {
                                    if settings_menu_selected > 0 {
                                        settings_menu_selected -= 1;
                                    } else {
                                        settings_menu_selected = settings_themes.len().saturating_sub(1);
                                    }
                                    let _ = renderer.render_settings_menu(
                                        &settings_themes, settings_menu_selected, &settings_active_theme,
                                    );
                                }
                                KeyCode::Down | KeyCode::Char('j') => {
                                    if !settings_themes.is_empty() {
                                        settings_menu_selected = (settings_menu_selected + 1) % settings_themes.len();
                                    }
                                    let _ = renderer.render_settings_menu(
                                        &settings_themes, settings_menu_selected, &settings_active_theme,
                                    );
                                }
                                KeyCode::Enter => {
                                    // Apply selected theme
                                    if let Some(theme_name) = settings_themes.get(settings_menu_selected) {
                                        settings_active_theme = theme_name.clone();
                                        send_msg(&mut writer, &ClientMsg::SwitchTheme { name: theme_name.clone() }).await?;
                                    }
                                    settings_menu_open = false;
                                    renderer.invalidate();
                                    if let Some((ref panes, focused, ref borders, ref status, total_rows)) = last_frame {
                                        let _ = renderer.render_frame(
                                            panes, focused, borders, status, total_rows, prefix_active,
                                            selection.as_ref(),
                                        );
                                    }
                                }
                                KeyCode::Esc | KeyCode::Char('q') => {
                                    settings_menu_open = false;
                                    renderer.invalidate();
                                    if let Some((ref panes, focused, ref borders, ref status, total_rows)) = last_frame {
                                        let _ = renderer.render_frame(
                                            panes, focused, borders, status, total_rows, prefix_active,
                                            selection.as_ref(),
                                        );
                                    }
                                }
                                _ => {}
                            }
                        }
                        TermEvent::Mouse(_) if settings_menu_open => {
                            // Consume mouse events while settings menu is open
                        }
                        TermEvent::Mouse(me) => {
                            // In copy mode, only handle scroll events from mouse
                            if copy_mode {
                                match me.kind {
                                    MouseEventKind::ScrollUp => {
                                        scroll_offset += 3;
                                        send_msg(&mut writer, &ClientMsg::ScrollBack { offset: scroll_offset }).await?;
                                    }
                                    MouseEventKind::ScrollDown => {
                                        scroll_offset = (scroll_offset - 3).max(0);
                                        send_msg(&mut writer, &ClientMsg::ScrollBack { offset: scroll_offset }).await?;
                                    }
                                    _ => {}
                                }
                            } else {
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
                                        // Check if click is on the status bar
                                        let (_scols, srows) = renderer.screen_size();
                                        let status_row = srows.saturating_sub(1);
                                        if me.row == status_row {
                                            // Hit-test click zones
                                            let mut clicked_action: Option<String> = None;
                                            for zone in &renderer.click_zones {
                                                if me.column >= zone.col_start && me.column < zone.col_end {
                                                    clicked_action = Some(zone.action.clone());
                                                    break;
                                                }
                                            }
                                            if let Some(action) = clicked_action {
                                                let msg = match action.as_str() {
                                                    "new-window" => Some(ClientMsg::NewWindow { name: None }),
                                                    "next-window" => Some(ClientMsg::NextWindow),
                                                    "prev-window" => Some(ClientMsg::PrevWindow),
                                                    a if a.starts_with("select-window-") => {
                                                        a.strip_prefix("select-window-")
                                                            .and_then(|n| n.parse::<usize>().ok())
                                                            .map(|index| ClientMsg::SelectWindow { index })
                                                    }
                                                    _ => None,
                                                };
                                                if let Some(msg) = msg {
                                                    send_msg(&mut writer, &msg).await?;
                                                }
                                            }
                                            // Don't start selection on status bar
                                        } else {
                                        // Check if click is on a border/divider
                                        let mut on_border = false;
                                        if let Some((_, _, ref borders, _, _)) = last_frame {
                                            for &(bx, by, blen, bhoriz) in borders {
                                                if bhoriz {
                                                    // Horizontal border at row=by, from x=bx to x=bx+blen-1
                                                    if me.row == by && me.column >= bx && me.column < bx + blen {
                                                        border_dragging = true;
                                                        border_drag_x = bx;
                                                        border_drag_y = by;
                                                        border_drag_horizontal = true;
                                                        border_drag_last_pos = me.row;
                                                        on_border = true;
                                                        break;
                                                    }
                                                } else {
                                                    // Vertical border at col=bx, from y=by to y=by+blen-1
                                                    if me.column == bx && me.row >= by && me.row < by + blen {
                                                        border_dragging = true;
                                                        border_drag_x = bx;
                                                        border_drag_y = by;
                                                        border_drag_horizontal = false;
                                                        border_drag_last_pos = me.column;
                                                        on_border = true;
                                                        break;
                                                    }
                                                }
                                            }
                                        }

                                        if !on_border {
                                            // Find the pane that was clicked to constrain selection
                                            let mut found_pane = false;
                                            if let Some((ref panes, _, _, _, _)) = last_frame {
                                                for pane in panes {
                                                    if me.column >= pane.x
                                                        && me.column < pane.x + pane.cols
                                                        && me.row >= pane.y
                                                        && me.row < pane.y + pane.rows
                                                    {
                                                        sel_pane_x = pane.x;
                                                        sel_pane_y = pane.y;
                                                        sel_pane_cols = pane.cols;
                                                        sel_pane_rows = pane.rows;
                                                        found_pane = true;
                                                        break;
                                                    }
                                                }
                                            }

                                            if found_pane {
                                                let cx = me.column.max(sel_pane_x).min(sel_pane_x + sel_pane_cols - 1);
                                                let cy = me.row.max(sel_pane_y).min(sel_pane_y + sel_pane_rows - 1);
                                                selection = Some(Selection {
                                                    start_x: cx,
                                                    start_y: cy,
                                                    end_x: cx,
                                                    end_y: cy,
                                                    pane_x: sel_pane_x,
                                                    pane_y: sel_pane_y,
                                                    pane_cols: sel_pane_cols,
                                                    pane_rows: sel_pane_rows,
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
                                        }
                                        } // end else (not status bar)
                                    }
                                    MouseEventKind::Drag(MouseButton::Left) => {
                                        if border_dragging {
                                            // Compute delta from last position
                                            let (current_pos, delta) = if border_drag_horizontal {
                                                let cur = me.row;
                                                let d = cur as i16 - border_drag_last_pos as i16;
                                                (cur, d)
                                            } else {
                                                let cur = me.column;
                                                let d = cur as i16 - border_drag_last_pos as i16;
                                                (cur, d)
                                            };
                                            if delta != 0 {
                                                border_drag_last_pos = current_pos;
                                                send_msg(&mut writer, &ClientMsg::DragBorder {
                                                    border_x: border_drag_x,
                                                    border_y: border_drag_y,
                                                    horizontal: border_drag_horizontal,
                                                    delta,
                                                }).await?;
                                                // Update tracked border position to follow the border
                                                if border_drag_horizontal {
                                                    border_drag_y = (border_drag_y as i16 + delta) as u16;
                                                } else {
                                                    border_drag_x = (border_drag_x as i16 + delta) as u16;
                                                }
                                            }
                                        } else if mouse_dragging {
                                            if let Some(ref mut sel) = selection {
                                                // Clamp to the pane where selection started
                                                sel.end_x = me.column.max(sel_pane_x).min(sel_pane_x + sel_pane_cols.saturating_sub(1));
                                                sel.end_y = me.row.max(sel_pane_y).min(sel_pane_y + sel_pane_rows.saturating_sub(1));
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
                                        if border_dragging {
                                            border_dragging = false;
                                        } else if mouse_dragging {
                                            mouse_dragging = false;

                                            if let Some(ref mut sel) = selection {
                                                // Clamp to the pane where selection started
                                                sel.end_x = me.column.max(sel_pane_x).min(sel_pane_x + sel_pane_cols.saturating_sub(1));
                                                sel.end_y = me.row.max(sel_pane_y).min(sel_pane_y + sel_pane_rows.saturating_sub(1));

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
                                    MouseEventKind::Down(MouseButton::Right) => {
                                        // Open context menu at click position
                                        context_menu_open = true;
                                        context_menu_x = me.column;
                                        context_menu_y = me.row;
                                        context_menu_selected = 0;

                                        // First focus the pane that was right-clicked
                                        if let Some((ref panes, _, _, _, _)) = last_frame {
                                            for pane in panes {
                                                if me.column >= pane.x
                                                    && me.column < pane.x + pane.cols
                                                    && me.row >= pane.y
                                                    && me.row < pane.y + pane.rows
                                                {
                                                    send_msg(&mut writer, &ClientMsg::FocusPane { pane: pane.id }).await?;
                                                    break;
                                                }
                                            }
                                        }

                                        // Render the context menu overlay
                                        let _ = renderer.render_context_menu(
                                            context_menu_x, context_menu_y,
                                            CONTEXT_MENU_ITEMS, context_menu_selected,
                                        );
                                    }
                                    _ => {}
                                }
                            }
                        }

                        TermEvent::Key(key_event) => {
                            // === Search mode: intercept all keys ===
                            if search_mode {
                                match key_event.code {
                                    KeyCode::Esc => {
                                        // Cancel search, exit search mode
                                        search_mode = false;
                                        search_query.clear();
                                        // Re-render status bar without search prompt
                                        if let Some((ref panes, focused, ref borders, ref _status, total_rows)) = last_frame {
                                            let normal_text = if scroll_offset > 0 {
                                                format!("vtx [SCROLL: +{}]", scroll_offset)
                                            } else {
                                                "vtx".to_string()
                                            };
                                            let normal_status = StyledStatus::simple(
                                                &normal_text,
                                                _status.left.first().map(|s| s.fg).unwrap_or(_status.bg),
                                                _status.bg,
                                            );
                                            let _ = renderer.render_frame(
                                                panes, focused, borders, &normal_status, total_rows, prefix_active,
                                                selection.as_ref(),
                                            );
                                        }
                                        // Request a fresh render from server to restore normal status
                                        send_msg(&mut writer, &ClientMsg::ScrollBack { offset: scroll_offset }).await?;
                                    }
                                    KeyCode::Enter => {
                                        // Submit search query
                                        if !search_query.is_empty() {
                                            send_msg(&mut writer, &ClientMsg::SearchScrollback { query: search_query.clone() }).await?;
                                        }
                                        search_mode = false;
                                    }
                                    KeyCode::Backspace => {
                                        search_query.pop();
                                        // Update status bar with current query
                                        if let Some((ref panes, focused, ref borders, ref _status, total_rows)) = last_frame {
                                            let search_status = StyledStatus::simple(
                                                &format!("search: {}_", search_query),
                                                _status.left.first().map(|s| s.fg).unwrap_or(_status.bg),
                                                _status.bg,
                                            );
                                            let _ = renderer.render_frame(
                                                panes, focused, borders, &search_status, total_rows, prefix_active,
                                                selection.as_ref(),
                                            );
                                        }
                                    }
                                    KeyCode::Char(c) => {
                                        if !key_event.modifiers.contains(KeyModifiers::CONTROL)
                                            && !key_event.modifiers.contains(KeyModifiers::ALT)
                                        {
                                            search_query.push(c);
                                            // Update status bar with current query
                                            if let Some((ref panes, focused, ref borders, ref _status, total_rows)) = last_frame {
                                                let search_status = StyledStatus::simple(
                                                    &format!("search: {}_", search_query),
                                                    _status.left.first().map(|s| s.fg).unwrap_or(_status.bg),
                                                    _status.bg,
                                                );
                                                let _ = renderer.render_frame(
                                                    panes, focused, borders, &search_status, total_rows, prefix_active,
                                                    selection.as_ref(),
                                                );
                                            }
                                        }
                                    }
                                    _ => {}
                                }
                                continue;
                            }

                            // === Copy mode: intercept all keys ===
                            if copy_mode {
                                // Get pane bounds for clamping cursor
                                let (pane_x, pane_y, pane_cols, pane_rows) = if let Some((ref panes, focused, _, _, _)) = last_frame {
                                    if let Some(p) = panes.iter().find(|p| p.id == focused) {
                                        (p.x, p.y, p.cols, p.rows)
                                    } else {
                                        (0, 0, 80, 24)
                                    }
                                } else {
                                    (0, 0, 80, 24)
                                };
                                let min_x = pane_x;
                                let max_x = pane_x + pane_cols.saturating_sub(1);
                                let min_y = pane_y;
                                let max_y = pane_y + pane_rows.saturating_sub(1);

                                let mut copy_handled = true;
                                match (key_event.code, key_event.modifiers) {
                                    // Movement: h/Left
                                    (KeyCode::Char('h'), m) | (KeyCode::Left, m) if !m.contains(KeyModifiers::CONTROL) => {
                                        copy_cursor_x = copy_cursor_x.saturating_sub(1).max(min_x);
                                    }
                                    // Movement: l/Right
                                    (KeyCode::Char('l'), m) | (KeyCode::Right, m) if !m.contains(KeyModifiers::CONTROL) => {
                                        copy_cursor_x = (copy_cursor_x + 1).min(max_x);
                                    }
                                    // Movement: k/Up
                                    (KeyCode::Char('k'), m) | (KeyCode::Up, m) if !m.contains(KeyModifiers::CONTROL) => {
                                        if copy_cursor_y > min_y {
                                            copy_cursor_y -= 1;
                                        } else {
                                            // Scroll up if at top of visible area
                                            scroll_offset += 1;
                                            send_msg(&mut writer, &ClientMsg::ScrollBack { offset: scroll_offset }).await?;
                                        }
                                    }
                                    // Movement: j/Down
                                    (KeyCode::Char('j'), m) | (KeyCode::Down, m) if !m.contains(KeyModifiers::CONTROL) => {
                                        if copy_cursor_y < max_y {
                                            copy_cursor_y += 1;
                                        } else if scroll_offset > 0 {
                                            scroll_offset = (scroll_offset - 1).max(0);
                                            send_msg(&mut writer, &ClientMsg::ScrollBack { offset: scroll_offset }).await?;
                                        }
                                    }
                                    // Ctrl+U: half page up
                                    (KeyCode::Char('u'), m) if m.contains(KeyModifiers::CONTROL) => {
                                        let half = pane_rows / 2;
                                        scroll_offset += half as i32;
                                        send_msg(&mut writer, &ClientMsg::ScrollBack { offset: scroll_offset }).await?;
                                    }
                                    // Ctrl+D: half page down
                                    (KeyCode::Char('d'), m) if m.contains(KeyModifiers::CONTROL) => {
                                        let half = pane_rows / 2;
                                        scroll_offset = (scroll_offset - half as i32).max(0);
                                        send_msg(&mut writer, &ClientMsg::ScrollBack { offset: scroll_offset }).await?;
                                    }
                                    // 0: beginning of line
                                    (KeyCode::Char('0'), _) => {
                                        copy_cursor_x = min_x;
                                    }
                                    // $: end of line
                                    (KeyCode::Char('$'), _) => {
                                        copy_cursor_x = max_x;
                                    }
                                    // w: forward word (simplified: +5 cols)
                                    (KeyCode::Char('w'), _) => {
                                        copy_cursor_x = (copy_cursor_x + 5).min(max_x);
                                    }
                                    // b: backward word (simplified: -5 cols)
                                    (KeyCode::Char('b'), _) => {
                                        copy_cursor_x = copy_cursor_x.saturating_sub(5).max(min_x);
                                    }
                                    // g: jump to top of scrollback
                                    (KeyCode::Char('g'), _) => {
                                        scroll_offset = 100_000;
                                        copy_cursor_x = min_x;
                                        copy_cursor_y = min_y;
                                        send_msg(&mut writer, &ClientMsg::ScrollBack { offset: scroll_offset }).await?;
                                    }
                                    // G: jump to bottom
                                    (KeyCode::Char('G'), _) => {
                                        scroll_offset = 0;
                                        copy_cursor_x = min_x;
                                        copy_cursor_y = max_y;
                                        send_msg(&mut writer, &ClientMsg::ScrollBack { offset: scroll_offset }).await?;
                                    }
                                    // v: toggle visual selection
                                    (KeyCode::Char('v'), _) => {
                                        if copy_selecting {
                                            copy_selecting = false;
                                        } else {
                                            copy_selecting = true;
                                            copy_sel_start_x = copy_cursor_x;
                                            copy_sel_start_y = copy_cursor_y;
                                        }
                                    }
                                    // y: yank selected text and exit copy mode
                                    (KeyCode::Char('y'), _) => {
                                        if copy_selecting {
                                            let yank_sel = Selection {
                                                start_x: copy_sel_start_x,
                                                start_y: copy_sel_start_y,
                                                end_x: copy_cursor_x,
                                                end_y: copy_cursor_y,
                                                pane_x, pane_y, pane_cols, pane_rows,
                                            };
                                            let text = renderer.extract_selection_text(&yank_sel);
                                            if !text.is_empty() {
                                                let b64 = base64_encode(text.as_bytes());
                                                let osc = format!("\x1b]52;c;{b64}\x07");
                                                let mut out = std::io::stdout();
                                                let _ = std::io::Write::write_all(&mut out, osc.as_bytes());
                                                let _ = std::io::Write::flush(&mut out);
                                            }
                                        }
                                        // Exit copy mode
                                        copy_mode = false;
                                        copy_selecting = false;
                                        selection = None;
                                        scroll_offset = 0;
                                        send_msg(&mut writer, &ClientMsg::ScrollBack { offset: 0 }).await?;
                                    }
                                    // Enter: copy and exit (like y)
                                    (KeyCode::Enter, _) => {
                                        if copy_selecting {
                                            let yank_sel = Selection {
                                                start_x: copy_sel_start_x,
                                                start_y: copy_sel_start_y,
                                                end_x: copy_cursor_x,
                                                end_y: copy_cursor_y,
                                                pane_x, pane_y, pane_cols, pane_rows,
                                            };
                                            let text = renderer.extract_selection_text(&yank_sel);
                                            if !text.is_empty() {
                                                let b64 = base64_encode(text.as_bytes());
                                                let osc = format!("\x1b]52;c;{b64}\x07");
                                                let mut out = std::io::stdout();
                                                let _ = std::io::Write::write_all(&mut out, osc.as_bytes());
                                                let _ = std::io::Write::flush(&mut out);
                                            }
                                        }
                                        copy_mode = false;
                                        copy_selecting = false;
                                        selection = None;
                                        scroll_offset = 0;
                                        send_msg(&mut writer, &ClientMsg::ScrollBack { offset: 0 }).await?;
                                    }
                                    // q or Escape: exit copy mode
                                    (KeyCode::Char('q'), _) | (KeyCode::Esc, _) => {
                                        copy_mode = false;
                                        copy_selecting = false;
                                        selection = None;
                                        scroll_offset = 0;
                                        send_msg(&mut writer, &ClientMsg::ScrollBack { offset: 0 }).await?;
                                    }
                                    _ => {
                                        copy_handled = false;
                                    }
                                }

                                if copy_handled {
                                    // Update the visible selection/cursor highlight
                                    if copy_selecting {
                                        selection = Some(Selection {
                                            start_x: copy_sel_start_x,
                                            start_y: copy_sel_start_y,
                                            end_x: copy_cursor_x,
                                            end_y: copy_cursor_y,
                                            pane_x, pane_y, pane_cols, pane_rows,
                                        });
                                    } else {
                                        // Single-cell cursor highlight
                                        selection = Some(Selection {
                                            start_x: copy_cursor_x,
                                            start_y: copy_cursor_y,
                                            end_x: copy_cursor_x,
                                            end_y: copy_cursor_y,
                                            pane_x, pane_y, pane_cols, pane_rows,
                                        });
                                    }
                                    // Re-render with updated selection
                                    if let Some((ref panes, focused, ref borders, ref status, total_rows)) = last_frame {
                                        let _ = renderer.render_frame(
                                            panes, focused, borders, status, total_rows, prefix_active,
                                            selection.as_ref(),
                                        );
                                    }
                                }
                                // In copy mode, never pass keys to the shell
                                continue;
                            }

                            // Any keypress clears mouse selection (normal mode)
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

                            let action = process_key(key_event, &mut prefix_active, &key_config);
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
                                InputAction::SourceConfig => {
                                    send_msg(&mut writer, &ClientMsg::SourceConfig { path: None }).await?;
                                }
                                InputAction::EnterCopyMode => {
                                    copy_mode = true;
                                    copy_selecting = false;
                                    // Position cursor at bottom-right of focused pane
                                    if let Some((ref panes, focused, _, _, _)) = last_frame {
                                        if let Some(p) = panes.iter().find(|p| p.id == focused) {
                                            copy_cursor_x = p.x + p.cols.saturating_sub(1);
                                            copy_cursor_y = p.y + p.rows.saturating_sub(1);
                                            sel_pane_x = p.x;
                                            sel_pane_y = p.y;
                                            sel_pane_cols = p.cols;
                                            sel_pane_rows = p.rows;
                                        }
                                    }
                                    // Show cursor highlight
                                    selection = Some(Selection {
                                        start_x: copy_cursor_x,
                                        start_y: copy_cursor_y,
                                        end_x: copy_cursor_x,
                                        end_y: copy_cursor_y,
                                        pane_x: sel_pane_x,
                                        pane_y: sel_pane_y,
                                        pane_cols: sel_pane_cols,
                                        pane_rows: sel_pane_rows,
                                    });
                                    if let Some((ref panes, focused, ref borders, ref status, total_rows)) = last_frame {
                                        let _ = renderer.render_frame(
                                            panes, focused, borders, status, total_rows, prefix_active,
                                            selection.as_ref(),
                                        );
                                    }
                                }
                                InputAction::OpenSettings => {
                                    // Request theme list from server, which will open the menu
                                    send_msg(&mut writer, &ClientMsg::ListThemes).await?;
                                }
                                InputAction::EnterSearchMode => {
                                    search_mode = true;
                                    search_query.clear();
                                    // Show search prompt on status bar
                                    if let Some((ref panes, focused, ref borders, ref _status, total_rows)) = last_frame {
                                        let search_status = StyledStatus::simple(
                                            "search: _",
                                            _status.left.first().map(|s| s.fg).unwrap_or(_status.bg),
                                            _status.bg,
                                        );
                                        let _ = renderer.render_frame(
                                            panes, focused, borders, &search_status, total_rows, prefix_active,
                                            selection.as_ref(),
                                        );
                                    }
                                }
                                InputAction::None => {}
                            }
                        }

                        TermEvent::Resize(cols, rows) => {
                            selection = None;
                            if copy_mode {
                                copy_mode = false;
                                copy_selecting = false;
                                scroll_offset = 0;
                                send_msg(&mut writer, &ClientMsg::ScrollBack { offset: 0 }).await?;
                            }
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

    /// Launch the GPU-accelerated attach loop.  This takes over the calling
    /// thread (winit requirement) and blocks until the window is closed or
    /// the session is detached.
    #[cfg(feature = "gpu")]
    pub fn run_attach_gpu(&self, session_name: Option<String>) -> vtx_core::Result<()> {
        crate::gpu_attach::run_gpu_attach(self.socket_path.clone(), session_name)
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

/// Keybinding configuration passed to key processing functions.
struct KeyConfig {
    prefix_key: char,
    bindings: Vec<KeyBinding>,
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

/// Convert an action name (from Lua config) into an InputAction.
fn action_from_name(name: &str) -> InputAction {
    match name {
        "split-horizontal" => InputAction::Send(ClientMsg::Split { horizontal: true }),
        "split-vertical" => InputAction::Send(ClientMsg::Split { horizontal: false }),
        "kill-pane" => InputAction::Send(ClientMsg::KillPane),
        "detach" => InputAction::Detach,
        "focus-left" => InputAction::Send(ClientMsg::FocusDirection { dir: Direction::Left }),
        "focus-right" => InputAction::Send(ClientMsg::FocusDirection { dir: Direction::Right }),
        "focus-up" => InputAction::Send(ClientMsg::FocusDirection { dir: Direction::Up }),
        "focus-down" => InputAction::Send(ClientMsg::FocusDirection { dir: Direction::Down }),
        "resize-left" => InputAction::Send(ClientMsg::ResizePane { dir: Direction::Left, amount: 5 }),
        "resize-right" => InputAction::Send(ClientMsg::ResizePane { dir: Direction::Right, amount: 5 }),
        "resize-up" => InputAction::Send(ClientMsg::ResizePane { dir: Direction::Up, amount: 5 }),
        "resize-down" => InputAction::Send(ClientMsg::ResizePane { dir: Direction::Down, amount: 5 }),
        "scroll-up" => InputAction::ScrollUp,
        "scroll-down" => InputAction::ScrollDown,
        "zoom" => InputAction::Send(ClientMsg::ZoomPane),
        "copy-mode" => InputAction::EnterCopyMode,
        "search" => InputAction::EnterSearchMode,
        "new-window" => InputAction::Send(ClientMsg::NewWindow { name: None }),
        "next-window" => InputAction::Send(ClientMsg::NextWindow),
        "prev-window" => InputAction::Send(ClientMsg::PrevWindow),
        "layout-cycle" => InputAction::Send(ClientMsg::LayoutCycle),
        "layout-even-h" => InputAction::Send(ClientMsg::SelectLayout { preset: LayoutPreset::EvenHorizontal }),
        "layout-even-v" => InputAction::Send(ClientMsg::SelectLayout { preset: LayoutPreset::EvenVertical }),
        "layout-main-v" => InputAction::Send(ClientMsg::SelectLayout { preset: LayoutPreset::MainVertical }),
        "layout-main-h" => InputAction::Send(ClientMsg::SelectLayout { preset: LayoutPreset::MainHorizontal }),
        "layout-tiled" => InputAction::Send(ClientMsg::SelectLayout { preset: LayoutPreset::Tiled }),
        "popup" => InputAction::Send(ClientMsg::PopupPane { command: None }),
        "close-popup" => InputAction::Send(ClientMsg::ClosePopup),
        "source-config" => InputAction::SourceConfig,
        "settings" => InputAction::OpenSettings,
        _ => InputAction::None,
    }
}

/// Check if any user-defined binding matches the current key event.
fn check_user_binding(key: &KeyEvent, prefix_active: bool, cfg: &KeyConfig) -> Option<InputAction> {
    let key_char = match key.code {
        KeyCode::Char(c) => Some(c.to_string()),
        KeyCode::Up => Some("up".into()),
        KeyCode::Down => Some("down".into()),
        KeyCode::Left => Some("left".into()),
        KeyCode::Right => Some("right".into()),
        KeyCode::PageUp => Some("pageup".into()),
        KeyCode::PageDown => Some("pagedown".into()),
        _ => None,
    }?;

    let modifier = if prefix_active {
        "prefix"
    } else if key.modifiers.contains(KeyModifiers::ALT) && key.modifiers.contains(KeyModifiers::SHIFT) {
        "alt-shift"
    } else if key.modifiers.contains(KeyModifiers::ALT) {
        "alt"
    } else if key.modifiers.contains(KeyModifiers::CONTROL) {
        "ctrl"
    } else if key.modifiers.contains(KeyModifiers::SHIFT) {
        "shift"
    } else {
        "none"
    };

    for binding in &cfg.bindings {
        if binding.modifier == modifier && binding.key.eq_ignore_ascii_case(&key_char) {
            return Some(action_from_name(&binding.action));
        }
    }

    None
}

/// Process a key event, handling prefix mode and direct bindings.
fn process_key(key: KeyEvent, prefix_active: &mut bool, cfg: &KeyConfig) -> InputAction {
    // === Scroll (Shift+PageUp/Down from keyboard) ===
    if key.modifiers.contains(KeyModifiers::SHIFT) {
        match key.code {
            KeyCode::PageUp => return InputAction::ScrollUp,
            KeyCode::PageDown => return InputAction::ScrollDown,
            _ => {}
        }
    }

    // === Check user-defined bindings first (non-prefix) ===
    if !*prefix_active {
        if let Some(action) = check_user_binding(&key, false, cfg) {
            return action;
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
            // Alt+1-9: switch to window by index
            KeyCode::Char(c @ '1'..='9') => {
                let index = (c as usize) - ('1' as usize);
                InputAction::Send(ClientMsg::SelectWindow { index })
            }
            _ => InputAction::None,
        };
    }

    // === Prefix key (configurable) ===
    if !*prefix_active
        && key.code == KeyCode::Char(cfg.prefix_key)
        && key.modifiers.contains(KeyModifiers::CONTROL)
    {
        *prefix_active = true;
        return InputAction::None;
    }

    if *prefix_active {
        *prefix_active = false;
        // Check user-defined prefix bindings first
        if let Some(action) = check_user_binding(&key, true, cfg) {
            return action;
        }
        return handle_prefix_key(key, cfg);
    }

    // === Normal mode — forward input to shell ===
    let data = key_event_to_bytes(&key);
    if data.is_empty() {
        InputAction::None
    } else {
        InputAction::Send(ClientMsg::Input { data })
    }
}

/// Handle the key pressed after the prefix key.
fn handle_prefix_key(key: KeyEvent, cfg: &KeyConfig) -> InputAction {
    match key.code {
        KeyCode::Char('|') => InputAction::Send(ClientMsg::Split { horizontal: true }),
        KeyCode::Char('-') => InputAction::Send(ClientMsg::Split { horizontal: false }),
        KeyCode::Char('x') => InputAction::Send(ClientMsg::KillPane),
        KeyCode::Char('z') => InputAction::Send(ClientMsg::ZoomPane),
        KeyCode::Char('[') => InputAction::EnterCopyMode,
        KeyCode::Char('/') => InputAction::EnterSearchMode,
        KeyCode::Char('c') => InputAction::Send(ClientMsg::NewWindow { name: None }),
        KeyCode::Char('n') => InputAction::Send(ClientMsg::NextWindow),
        KeyCode::Char('p') => InputAction::Send(ClientMsg::PrevWindow),
        KeyCode::Char('D') | KeyCode::Char('d') => InputAction::Detach,
        KeyCode::Char('R') | KeyCode::Char('r') => InputAction::SourceConfig,
        KeyCode::Char('S') | KeyCode::Char('s') => InputAction::OpenSettings,
        KeyCode::Char(c) if c == cfg.prefix_key && key.modifiers.contains(KeyModifiers::CONTROL) => {
            // Double-tap prefix sends the literal key
            let byte = (c as u8).to_ascii_lowercase() - b'a' + 1;
            InputAction::Send(ClientMsg::Input { data: vec![byte] })
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
        KeyCode::Char(' ') => InputAction::Send(ClientMsg::LayoutCycle),
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
