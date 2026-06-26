# IRC Composer Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add an opt-in "IRC composer" — a line-editor pinned to the bottom of the screen (above the status bar) that buffers a line and sends it to the focused pane's shell on Enter, with command output scrolling above. It auto-hides when the focused pane is a full-screen app (alt-screen), so vim/htop/less still get raw keys.

**Architecture:** `composer_enabled` is a per-session flag (default from config), so the render function — which already takes `&Session` — reserves the composer row with no new call-site threading. The server computes `composer_active = session.composer_enabled && !focused_pane_in_alt_screen`, shrinks the pane area by one row when active, and tells the client the composer row via a new optional `Render` field. The **client** owns the line buffer + editing + local rendering of the composer row, and on Enter sends `Input{ line + "\n" }`. A new `ClientMsg::ToggleComposer` (bound to prefix+i) flips the session flag.

**Tech Stack:** Rust (edition 2024); crates `vtx-core` (config + IPC), `vtx-server` (session + render), `vtx-client` (input + render), `vtx-renderer-tty` (draw the composer line), `crossterm`. Builds on the merged frame clock (the server-owned tick re-renders ~125x/s, so composer/cursor updates flow without extra plumbing).

---

## File Structure

- Modify: `crates/core/src/lua_config.rs` + `crates/core/src/config.rs` — `composer { enabled, prompt }` config.
- Modify: `crates/core/src/ipc.rs` — `composer_row: Option<u16>` on `ServerMsg::Render`; `ClientMsg::ToggleComposer`.
- Create: `crates/client/src/composer.rs` — `ComposerBuffer`, a pure line editor (text, cursor, history). The most-tested unit.
- Modify: `crates/client/src/lib.rs` — `mod composer;`.
- Modify: `crates/server/src/session.rs` — `composer_enabled: bool` on `Session`, defaulted.
- Modify: `crates/server/src/server.rs` — read `composer_enabled` in `build_render_msg_scrolled` (reserve row + set `composer_row`); init from config on session create; `ToggleComposer` handler.
- Modify: `crates/renderer-tty/src/lib.rs` — a pure `composer_line(prompt, text, width)` helper (tested) + a `render_composer` draw method.
- Modify: `crates/client/src/client.rs` — store `composer_row` from `Render`; draw the composer; route input to `ComposerBuffer` when active; prefix+i toggle; Enter sends.

Confirmed facts (already read):
- `ServerMsg::Render { panes, focused, borders, status, total_rows }` — only 2 construction sites (`server.rs:1537` zoomed, `server.rs:1641` normal, both inside `build_render_msg_scrolled`) and 2 destructuring sites (`client.rs:237`, `gpu_attach.rs:216`).
- `build_render_msg_scrolled(session, cols, total_rows, scroll_offset, status_cfg, anim)` (server.rs:1498); `pane_area_rows = total_rows.saturating_sub(1)`.
- Focused pane alt-screen state: `session.active_window().panes.get(&focused).unwrap().parser.grid.using_alt_screen` (`grid.using_alt_screen` is a `pub bool`, grid.rs:41). Confirm the `Session`/`Window` accessor names when reading session.rs (`active_window()`, `focused_pane`).
- `ClientMsg` enum (ipc.rs:42); add a new variant near `ZoomPane`.
- The client's render arm is at `client.rs:237`; `process_key` / input routing in `client.rs` (~1300+); the renderer has a `render_context_menu` that draws directly to stdout (model for `render_composer`).

---

## Task 1: Composer config

**Files:**
- Modify: `crates/core/src/lua_config.rs`, `crates/core/src/config.rs`

- [ ] **Step 1: Read the pattern.** Look at how the just-added `animations` config is declared on `LuaConfig`, defaulted, parsed in the `__newindex` table arm, and mirrored onto runtime `Config` (in `config.rs` `Default` + `reload_from_lua`). Mirror that exactly for `composer`.

- [ ] **Step 2: Write the failing test.** Add to the `mod tests` in `lua_config.rs`:

```rust
    #[test]
    fn composer_config_has_defaults() {
        let cfg = LuaConfig::default();
        assert!(!cfg.composer.enabled); // opt-in: off by default
        assert_eq!(cfg.composer.prompt, "\u{203a} "); // "› "
    }
```

- [ ] **Step 3: Run it, watch it fail.** `cargo test -p vtx-core composer_config_has_defaults` — compile error, no `composer` field.

- [ ] **Step 4: Implement.** Add near the other config structs in `lua_config.rs`:

```rust
/// IRC-style composer settings.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ComposerConfig {
    pub enabled: bool,
    pub prompt: String,
}

impl Default for ComposerConfig {
    fn default() -> Self {
        ComposerConfig { enabled: false, prompt: "\u{203a} ".to_string() }
    }
}
```

Add `pub composer: ComposerConfig,` to `LuaConfig` + `composer: ComposerConfig::default(),` to its `Default`. Add a `"composer"` arm to the `__newindex` handler mirroring the `animations` arm:

```rust
                "composer" => {
                    if let LuaValue::Table(tbl) = value {
                        if let Ok(enabled) = tbl.get::<bool>("enabled") {
                            c.composer.enabled = enabled;
                        }
                        if let Ok(prompt) = tbl.get::<String>("prompt") {
                            c.composer.prompt = prompt;
                        }
                    }
                }
```

Then in `crates/core/src/config.rs`: add `#[serde(default)] pub composer: lua_config::ComposerConfig,` to `Config`, wire it into `Default` and `reload_from_lua` (alongside `animations`).

- [ ] **Step 5: Run it, watch it pass.** `cargo test -p vtx-core` (all pass), `cargo build -p vtx-core` (clean).

- [ ] **Step 6: Commit.**
```bash
git add crates/core/src/lua_config.rs crates/core/src/config.rs
git commit -m "feat: composer config (enabled/prompt) with defaults"
```

---

## Task 2: `ComposerBuffer` line editor (pure)

**Files:**
- Create: `crates/client/src/composer.rs`
- Modify: `crates/client/src/lib.rs` (add `mod composer;`)

- [ ] **Step 1: Write the file with tests.** Create `crates/client/src/composer.rs`:

```rust
//! The client-side composer line buffer: a single-line editor with history.
//! Pure logic — no I/O. Cursor positions are char indices (Unicode-correct).

#[derive(Default)]
pub struct ComposerBuffer {
    chars: Vec<char>,
    cursor: usize,
    history: Vec<String>,
    /// Index into history while browsing (None = editing the live line).
    history_pos: Option<usize>,
}

impl ComposerBuffer {
    pub fn text(&self) -> String {
        self.chars.iter().collect()
    }

    /// Cursor position as a char index in `[0, len]`.
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    pub fn insert(&mut self, c: char) {
        self.chars.insert(self.cursor, c);
        self.cursor += 1;
    }

    pub fn backspace(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
            self.chars.remove(self.cursor);
        }
    }

    pub fn delete(&mut self) {
        if self.cursor < self.chars.len() {
            self.chars.remove(self.cursor);
        }
    }

    pub fn left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    pub fn right(&mut self) {
        if self.cursor < self.chars.len() {
            self.cursor += 1;
        }
    }

    pub fn home(&mut self) {
        self.cursor = 0;
    }

    pub fn end(&mut self) {
        self.cursor = self.chars.len();
    }

    /// Take the current line: if non-empty, push it to history, clear the
    /// buffer, and return the line. Empty lines return None.
    pub fn take_line(&mut self) -> Option<String> {
        if self.chars.is_empty() {
            return None;
        }
        let line: String = self.chars.iter().collect();
        self.history.push(line.clone());
        self.chars.clear();
        self.cursor = 0;
        self.history_pos = None;
        Some(line)
    }

    /// Step back to an older history entry, replacing the current line.
    pub fn history_prev(&mut self) {
        if self.history.is_empty() {
            return;
        }
        let next = match self.history_pos {
            None => self.history.len() - 1,
            Some(0) => 0,
            Some(p) => p - 1,
        };
        self.history_pos = Some(next);
        self.set_from_history();
    }

    /// Step forward toward the live (empty) line.
    pub fn history_next(&mut self) {
        match self.history_pos {
            Some(p) if p + 1 < self.history.len() => {
                self.history_pos = Some(p + 1);
                self.set_from_history();
            }
            Some(_) => {
                // Past the newest entry: return to an empty live line.
                self.history_pos = None;
                self.chars.clear();
                self.cursor = 0;
            }
            None => {}
        }
    }

    fn set_from_history(&mut self) {
        if let Some(p) = self.history_pos {
            self.chars = self.history[p].chars().collect();
            self.cursor = self.chars.len();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_and_edit() {
        let mut b = ComposerBuffer::default();
        for c in "helo".chars() { b.insert(c); }
        assert_eq!(b.text(), "helo");
        assert_eq!(b.cursor(), 4);
        b.left();
        b.insert('l'); // "hello"
        assert_eq!(b.text(), "hello");
        b.home();
        b.delete(); // remove 'h'
        assert_eq!(b.text(), "ello");
        b.end();
        b.backspace(); // remove 'o'
        assert_eq!(b.text(), "ell");
    }

    #[test]
    fn cursor_bounds() {
        let mut b = ComposerBuffer::default();
        b.left();
        b.backspace(); // no-op on empty
        assert_eq!(b.text(), "");
        b.insert('a');
        b.right(); // already at end
        assert_eq!(b.cursor(), 1);
    }

    #[test]
    fn take_line_clears_and_records_history() {
        let mut b = ComposerBuffer::default();
        assert_eq!(b.take_line(), None); // empty -> None
        for c in "ls -l".chars() { b.insert(c); }
        assert_eq!(b.take_line(), Some("ls -l".to_string()));
        assert_eq!(b.text(), "");
        assert_eq!(b.cursor(), 0);
    }

    #[test]
    fn history_navigation() {
        let mut b = ComposerBuffer::default();
        for c in "one".chars() { b.insert(c); }
        b.take_line();
        for c in "two".chars() { b.insert(c); }
        b.take_line();

        b.history_prev();
        assert_eq!(b.text(), "two");
        b.history_prev();
        assert_eq!(b.text(), "one");
        b.history_prev(); // clamps at oldest
        assert_eq!(b.text(), "one");
        b.history_next();
        assert_eq!(b.text(), "two");
        b.history_next(); // past newest -> empty live line
        assert_eq!(b.text(), "");
    }
}
```

Add `mod composer;` to `crates/client/src/lib.rs`.

- [ ] **Step 2: Run, watch fail-then-pass.** `cargo test -p vtx-client composer::` — it should compile and pass (the code is in-file). To honor red-green, temporarily make `take_line` always return `None`, confirm `take_line_clears_and_records_history` and `history_navigation` FAIL, then restore.

- [ ] **Step 3: Verify.** `cargo test -p vtx-client composer::` (4 pass), `cargo build -p vtx-client` (clean — `ComposerBuffer` is unused until Task 7, so expect dead-code warnings; that's fine).

- [ ] **Step 4: Commit.**
```bash
git add crates/client/src/composer.rs crates/client/src/lib.rs
git commit -m "feat: ComposerBuffer line editor with history"
```

---

## Task 3: IPC — `composer_row` on Render + `ToggleComposer`

**Files:**
- Modify: `crates/core/src/ipc.rs`, and the 2 construction + 2 destructuring sites.

- [ ] **Step 1: Add the IPC fields.** In `crates/core/src/ipc.rs`:
  - Add a field to the `Render` variant: change it to
    ```rust
    Render {
        panes: Vec<PaneRender>,
        focused: PaneId,
        borders: Vec<(u16, u16, u16, bool)>,
        status: StyledStatus,
        total_rows: u16,
        /// Screen row to draw the composer line on, if the composer is active.
        #[serde(default)]
        composer_row: Option<u16>,
    },
    ```
  - Add a `ClientMsg` variant (near `ZoomPane`):
    ```rust
    /// Toggle the IRC composer for the attached session.
    ToggleComposer,
    ```

- [ ] **Step 2: Fix the construction sites (server).** At `server.rs:1537` and `server.rs:1641`, the two `ServerMsg::Render { ... }` literals must now include `composer_row`. For THIS task add `composer_row: None,` to both (Task 5 computes the real value). Build will fail until done.

- [ ] **Step 3: Fix the destructuring sites (clients).** At `client.rs:237` and `gpu_attach.rs:216`, the `ServerMsg::Render { panes, focused, borders, status, total_rows }` patterns must bind or ignore the new field. For now add `, composer_row: _` to both patterns (Task 7 uses it in client.rs).

- [ ] **Step 4: Verify.** `cargo build --workspace` (clean), `cargo test --workspace` (all pass — no behavior change yet).

- [ ] **Step 5: Commit.**
```bash
git add -A
git commit -m "feat: IPC fields for composer (composer_row on Render, ToggleComposer)"
```

---

## Task 4: `composer_enabled` on `Session`

**Files:**
- Modify: `crates/server/src/session.rs`, `crates/server/src/server.rs`

- [ ] **Step 1: Read session.rs.** Find the `Session` struct and `Session::new(...)`. Note its fields and how `NewSession` (in `handle_message`, server.rs) constructs a session and what config is available there (`st.config`).

- [ ] **Step 2: Add the field.** Add `pub composer_enabled: bool,` to `struct Session`. In `Session::new`, default it to `false` (sessions start with the composer off; the per-session default is applied from config at creation in Step 3). Update any other `Session { ... }` literal constructions to include `composer_enabled: false,`.

- [ ] **Step 3: Initialize from config on create.** In the `NewSession` handler (and the resurrect path if it builds a `Session`), after constructing the session set `session.composer_enabled = st.config.composer.enabled;` before inserting it into `st.sessions`. (Search for where `Session::new` is called in `handle_message`.)

- [ ] **Step 4: Verify.** `cargo build -p vtx-server` (clean), `cargo test -p vtx-server` (pass).

- [ ] **Step 5: Commit.**
```bash
git add crates/server/src/session.rs crates/server/src/server.rs
git commit -m "feat: per-session composer_enabled flag, defaulted from config"
```

---

## Task 5: Server — reserve the row, compute `composer_row`, handle the toggle

**Files:**
- Modify: `crates/server/src/server.rs`

- [ ] **Step 1: Compute composer state in `build_render_msg_scrolled`.** Near the top of `build_render_msg_scrolled` (after `let pane_area_rows = total_rows.saturating_sub(1);`), insert:

```rust
    // Composer: active when enabled for the session AND the focused pane is not
    // a full-screen (alt-screen) app. When active, reserve one row above the
    // status bar for the composer line.
    let focused_id = session.active_window().focused_pane;
    let focused_alt = session
        .active_window()
        .panes
        .get(&focused_id)
        .map(|p| p.parser.grid.using_alt_screen)
        .unwrap_or(false);
    let composer_active = session.composer_enabled && !focused_alt;
    let composer_row = if composer_active && total_rows >= 2 {
        Some(total_rows - 2)
    } else {
        None
    };
    let pane_area_rows = pane_area_rows.saturating_sub(if composer_active { 1 } else { 0 });
```

(There is already a `let pane_area_rows = ...` and a `focused_id` later in the function — REPLACE/relocate so there is a single `pane_area_rows` and `focused_id` used by the rest of the function. Read the function and reconcile; the zoomed early-return path at ~1537 should also use the reduced `pane_area_rows` and pass `composer_row`.)

- [ ] **Step 2: Emit `composer_row` in both Render literals.** Change the `composer_row: None,` added in Task 3 (at the zoomed return ~1537 and the normal return ~1641) to `composer_row,` (the value computed in Step 1). Ensure `composer_row` is in scope at both returns (compute it before the zoomed early-return).

- [ ] **Step 3: Add the `ToggleComposer` handler.** In `handle_message`, add an arm near `ClientMsg::ZoomPane`:

```rust
        ClientMsg::ToggleComposer => {
            if let Some(sid) = cs.attached_session {
                if let Some(session) = st.sessions.get_mut(&sid) {
                    session.composer_enabled = !session.composer_enabled;
                    return build_render_msg(session, cs.cols, cs.rows, &st.config.status_bar);
                }
            }
            ServerMsg::Error { msg: "no attached session".into() }
        }
```
(Match the exact field/getter names used by neighboring arms — e.g. how they read `cs.attached_session` and `st.sessions.get_mut`.)

- [ ] **Step 4: Verify.** `cargo build --workspace` (clean), `cargo test -p vtx-server` (pass). Note: `build_render_msg` (the offset=0 wrapper) calls `build_render_msg_scrolled`, so toggling and normal renders both reserve the row consistently.

- [ ] **Step 5: Commit.**
```bash
git add crates/server/src/server.rs
git commit -m "feat: server reserves composer row and reports composer_row to clients"
```

---

## Task 6: Renderer — draw the composer line

**Files:**
- Modify: `crates/renderer-tty/src/lib.rs`

- [ ] **Step 1: Write the failing test for the pure helper.** Add to the `tests` module in `renderer-tty/src/lib.rs`:

```rust
    #[test]
    fn composer_line_renders_prompt_and_truncates() {
        // Fits: prompt + text, padded to width.
        let line = composer_line("> ", "hi", 6);
        assert_eq!(line, "> hi  ");
        // Too long: keep the tail near the cursor visible (truncate the front).
        let line = composer_line("> ", "abcdefghij", 6);
        assert_eq!(line.chars().count(), 6);
        assert!(line.ends_with('j'), "tail must stay visible: {line:?}");
    }
```

- [ ] **Step 2: Run, watch it fail.** `cargo test -p vtx-renderer-tty composer_line_renders` — compile error, no `composer_line`.

- [ ] **Step 3: Implement the pure helper + draw method.** Add the free function:

```rust
/// Compose the visible composer line: `prompt` + `text`, truncated to `width`
/// keeping the tail (cursor end) visible, then space-padded to `width`.
pub fn composer_line(prompt: &str, text: &str, width: usize) -> String {
    let mut s: String = format!("{prompt}{text}");
    let len = s.chars().count();
    if len > width {
        // Drop leading chars so the end stays visible.
        s = s.chars().skip(len - width).collect();
    } else {
        s.extend(std::iter::repeat(' ').take(width - len));
    }
    s
}
```

Add a method on `TtyRenderer` that draws it directly to stdout (mirror `render_context_menu`'s direct-draw style — `MoveTo`, set colors, `Print`, then leave the terminal cursor at the composer's edit position):

```rust
    /// Draw the composer line at `row`, then place the terminal cursor at the
    /// edit position. Drawn directly (not via the diff buffer), like menus.
    pub fn render_composer(&mut self, row: u16, prompt: &str, text: &str, cursor: usize) -> io::Result<()> {
        let width = self.screen_cols as usize;
        let line = composer_line(prompt, text, width);
        queue!(
            self.stdout,
            cursor::MoveTo(0, row),
            SetForegroundColor(to_ct_color(&self.status_fg)),
            SetBackgroundColor(to_ct_color(&self.status_bg)),
            style::Print(&line),
            SetForegroundColor(CtColor::Reset),
            SetBackgroundColor(CtColor::Reset),
        )?;
        // Cursor column = prompt width + cursor, clamped to the screen.
        let col = (prompt.chars().count() + cursor).min(width.saturating_sub(1)) as u16;
        queue!(self.stdout, cursor::MoveTo(col, row), cursor::Show)?;
        self.stdout.flush()?;
        Ok(())
    }
```
(If `to_ct_color`, `CtColor`, `cursor::Show`, or `style` aren't already imported/used in scope, check the top of the file — they are used elsewhere in this module. Adjust to the exact symbols the file already uses.)

- [ ] **Step 4: Run, watch it pass.** `cargo test -p vtx-renderer-tty` (all pass), `cargo build -p vtx-renderer-tty` (clean).

- [ ] **Step 5: Commit.**
```bash
git add crates/renderer-tty/src/lib.rs
git commit -m "feat: renderer composer line (pure helper + draw method)"
```

---

## Task 7: Client — wire the composer into input + render

**Files:**
- Modify: `crates/client/src/client.rs`

This is the integration task. Read the client's main loop, the `ServerMsg::Render` arm (~client.rs:237), the input/`process_key` path, and how `renderer.render_frame(...)` is called, before editing. Implement carefully; verify with a manual run.

- [ ] **Step 1: Add composer state to the client loop.** Near where other mutable loop state is declared (e.g. `copy_mode`, `selection`), add:
```rust
    let mut composer = crate::composer::ComposerBuffer::default();
    let mut composer_row: Option<u16> = None;
```

- [ ] **Step 2: Capture `composer_row` from Render and draw the composer.** In the `ServerMsg::Render { ..., composer_row: cr }` arm (rename the bound field from `_` to `cr`), set `composer_row = cr;` and, AFTER the existing `renderer.render_frame(...)` call in that arm, add:
```rust
                            if let Some(row) = composer_row {
                                let _ = renderer.render_composer(
                                    row,
                                    &self.composer_prompt,  // see Step 5 for where this comes from
                                    &composer.text(),
                                    composer.cursor(),
                                );
                            }
```
(Use the composer prompt from the client's config — see Step 5. If `self` is not in scope inside the loop, capture the prompt into a local `let composer_prompt = cfg.composer_prompt.clone();` before the loop and use that.)

- [ ] **Step 3: Route input to the composer when active.** Find where a `TermEvent::Key` / decoded key is handled in normal mode (after prefix handling, where it currently turns the key into `ClientMsg::Input`). Add a branch: when `composer_row.is_some()` AND the client is not in copy/search/menu mode AND the prefix is not pending, handle the key against the composer instead of sending raw:
```rust
    // composer-active key handling (printable + editing keys)
    match key.code {
        KeyCode::Char(c) if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT => {
            composer.insert(c);
        }
        KeyCode::Backspace => composer.backspace(),
        KeyCode::Delete => composer.delete(),
        KeyCode::Left => composer.left(),
        KeyCode::Right => composer.right(),
        KeyCode::Home => composer.home(),
        KeyCode::End => composer.end(),
        KeyCode::Up => composer.history_prev(),
        KeyCode::Down => composer.history_next(),
        KeyCode::Enter => {
            if let Some(line) = composer.take_line() {
                let mut data = line.into_bytes();
                data.push(b'\n');
                send_msg(&mut writer, &ClientMsg::Input { data }).await?;
            }
        }
        KeyCode::Esc => {
            send_msg(&mut writer, &ClientMsg::ToggleComposer).await?;
        }
        _ => {}
    }
    // then re-render the composer locally so typing echoes immediately:
    if let Some(row) = composer_row {
        let _ = renderer.render_composer(row, &composer_prompt, &composer.text(), composer.cursor());
    }
```
Make sure this branch is taken INSTEAD of the normal raw-input send when the composer is active, and that the prefix key + prefix commands still work (the prefix handling must run before this branch).

- [ ] **Step 4: Bind prefix+i to toggle.** In the prefix-command handling (`handle_prefix_key` / `process_key`), add a case so that after the prefix, pressing `i` sends `ClientMsg::ToggleComposer`. Mirror how other prefix commands map to a `ClientMsg` / `InputAction`. (If actions are returned as an `InputAction` enum, add a variant or reuse the `Send(ClientMsg::ToggleComposer)` pattern used by neighboring keys.)

- [ ] **Step 5: Composer prompt from config.** The client loads a config with a prefix key etc. Add the composer prompt to that client config (where `prefix_key` is read) sourced from `cfg.composer.prompt`, and capture it into the `composer_prompt` local used above. If the client config struct doesn't carry composer settings yet, add a `composer_prompt: String` field populated from the loaded `Config.composer.prompt`.

- [ ] **Step 6: Build, test, and manually verify.**
  - `cargo build --workspace` (clean — `ComposerBuffer` dead-code warnings from Task 2 are now gone), `cargo test` (all pass).
  - Manual (real terminal, ideally Linux `srv`): set `composer = { enabled = true }` in `~/.config/vtx/config.lua`. Start vtx. Confirm: a composer line appears above the status bar; typing echoes there (not in the pane); Enter sends the line to the shell and output appears in the pane above; Up/Down browses history; prefix+i toggles the composer off/on; opening `vim` (alt-screen) hides the composer and keys reach vim; quitting vim restores the composer.

- [ ] **Step 7: Commit.**
```bash
git add crates/client/src/client.rs
git commit -m "feat: wire IRC composer into client input and rendering"
```

---

## Self-Review

- **Spec coverage:** dedicated bottom composer line ✅ (Tasks 5-7), opt-in via config + prefix toggle ✅ (Tasks 1, 5, 7), alt-screen auto-bypass ✅ (Task 5 `composer_active` gate), client owns buffer/editing/render + server reserves row ✅ (Tasks 2, 5, 6, 7), Enter sends `line+"\n"` ✅ (Task 7), v1 editing incl. history ✅ (Task 2). Out-of-scope per spec (multiline, per-pane composers, completion) are not included. **Deviation from spec:** the spec floated a `SetComposer` client→server message and per-client activation; this plan uses per-session `composer_enabled` + `ToggleComposer` instead, which removes ~30 call-site changes and keeps multiple clients on a session consistent. Same user-visible behavior.
- **Placeholder scan:** Tasks 4, 5, 7 contain explicit "read the file and match the exact names" steps (session constructor, neighboring `ClientMsg` arms, the client input path) — these are discovery steps with concrete pointers, not vague placeholders. All pure-logic tasks (1, 2, 6 helper) have complete code.
- **Type consistency:** `ComposerBuffer` methods (`text`, `cursor`, `insert`, `backspace`, `delete`, `left`, `right`, `home`, `end`, `take_line`, `history_prev`, `history_next`) defined in Task 2 and used identically in Task 7. `composer_row: Option<u16>` consistent across ipc.rs (Task 3), server emit (Task 5), and client consume (Task 7). `ComposerConfig { enabled, prompt }` consistent (Tasks 1, 7). `render_composer(row, prompt, text, cursor)` / `composer_line(prompt, text, width)` consistent (Tasks 6, 7).

## Risks
- **Task 7 input precedence** is the main risk: the composer branch must not swallow the prefix key or break copy/search modes. Gate it on `composer_row.is_some() && !prefix_pending && !copy_mode && !search_mode`.
- **Direct-draw composer vs the diff renderer:** `render_composer` draws outside the diff buffer (like menus). Because the server keeps the composer row blank in the pane area, `render_frame` won't fight it; but a full invalidate (resize) followed by a frame will blank the row until the next `render_composer` call — drawing the composer after every `render_frame` (Step 2) and after every keystroke (Step 3) covers this.
- **Cursor ownership:** when the composer is active the terminal cursor belongs to the composer; when it toggles off, the next `render_frame` restores the pane cursor.
