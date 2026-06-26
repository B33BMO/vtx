# Frame-Clock Foundation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the per-client PTY poll task with a single server-owned drain tick and a frame-notification channel, so all sessions are drained regardless of attached clients (fixes audit finding H1) and a steady frame clock exists for later animation/composer work.

**Architecture:** Today each connected client spawns its own task that drains *all* sessions every 8 ms and signals only itself. We extract the drain into a pure helper, move it to one server-owned task in `VtxServer::run`, and bump a `tokio::sync::watch<u64>` frame counter when any output is drained. Client render loops switch from a per-client `pty_rx` channel to `frame_rx.changed()`. This is the shared substrate for the animation and composer plans, which are written separately against this API.

**Tech Stack:** Rust (edition 2024), tokio (async runtime, `watch`/`mpsc` channels, `interval`), the existing `vtx-server` crate.

**Scope note:** This plan delivers the frame clock + H1 only. Audit finding **H9** (plugin hooks block the event loop) is intentionally *not* folded in here: the plugins live inside the `ServerState` mutex and hold Lua/WASM state, so moving dispatch off-loop needs its own small design (a plugin actor/thread), not a timeout wrapper. It gets its own plan. The animation system and IRC composer are also separate plans that build on this one.

---

## File Structure

- `crates/server/src/server.rs` — **modify.** Extract `drain_all_sessions`; add a `frame_tx: watch::Sender<u64>` to `ServerState`; add the server-owned drain task to `VtxServer::run`; remove the per-client poll task; switch the client render arm to `frame_rx.changed()`.
- No new files. No IPC or config changes in this plan (those land with the feature plans).

Confirmed current facts (already read):
- `ServerState` (server.rs:28) fields: `config, sessions, next_session_id, plugins, active_theme`.
- `ServerState::new` at server.rs:42.
- Per-client poll task at server.rs:126-149 drains via `pane.drain_output() -> bool`.
- Client render arm at server.rs:182-203 (`_ = pty_rx.recv()`), already drops the state lock before the socket write (H2 fix shipped).
- `Pane::drain_output(&mut self) -> bool` returns whether any output was processed.

---

## Task 1: Extract `drain_all_sessions` helper

**Files:**
- Modify: `crates/server/src/server.rs` (the poll task body at ~128-149, plus add a free function)
- Test: `crates/server/src/server.rs` (new `#[cfg(test)] mod tests`)

- [ ] **Step 1: Write the failing test**

Add at the end of `crates/server/src/server.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use vtx_core::config::Config;

    /// H1 (part 1): the drain helper must walk every session/window/pane.
    /// On an empty server it reports no output and does not panic.
    #[test]
    fn drain_all_sessions_on_empty_state_reports_no_output() {
        let mut state = ServerState::new(Config::default());
        assert!(!drain_all_sessions(&mut state));
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p vtx-server drain_all_sessions_on_empty_state -- --nocapture`
Expected: FAIL to compile — `cannot find function drain_all_sessions in this scope`.

- [ ] **Step 3: Add the helper and have the poll task call it**

Add this free function near the bottom of `server.rs` (outside any `impl`):

```rust
/// Drain pending PTY output from every pane of every session into its grid.
/// Returns `true` if any pane produced output. Runs regardless of whether a
/// client is attached, so detached sessions don't accumulate unbounded output.
fn drain_all_sessions(state: &mut ServerState) -> bool {
    let mut any_output = false;
    for session in state.sessions.values_mut() {
        for window in session.windows.iter_mut() {
            for pane in window.panes.values_mut() {
                if pane.drain_output() {
                    any_output = true;
                }
            }
        }
    }
    any_output
}
```

Then replace the body of the per-client poll task (server.rs ~132-147) so it calls the helper:

```rust
        loop {
            tokio::time::sleep(std::time::Duration::from_millis(8)).await;

            let mut st = poll_state.lock().await;
            let any_output = drain_all_sessions(&mut st);
            drop(st);

            if any_output {
                let _ = pty_tx.try_send(());
            }
        }
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p vtx-server drain_all_sessions_on_empty_state`
Expected: PASS. Also run `cargo build -p vtx-server` — expected: builds clean.

- [ ] **Step 5: Commit**

```bash
git add crates/server/src/server.rs
git commit -m "refactor: extract drain_all_sessions helper"
```

---

## Task 2: Add a frame-notification channel to `ServerState`

**Files:**
- Modify: `crates/server/src/server.rs` (imports, `ServerState` struct, `ServerState::new`)

- [ ] **Step 1: Add the `watch` import**

At the top of `server.rs`, change the tokio sync import (currently `use tokio::sync::{mpsc, Mutex};`) to:

```rust
use tokio::sync::{mpsc, watch, Mutex};
```

- [ ] **Step 2: Add the field to `ServerState`**

In `struct ServerState` (server.rs:28), add a field:

```rust
struct ServerState {
    config: Config,
    sessions: HashMap<SessionId, Session>,
    next_session_id: u32,
    plugins: PluginManager,
    /// Name of the currently active theme.
    active_theme: String,
    /// Frame counter bumped whenever any session drains new output. Clients
    /// subscribe and re-render their attached session when it changes.
    frame_tx: watch::Sender<u64>,
}
```

- [ ] **Step 3: Initialize it in `ServerState::new`**

In `ServerState::new` (server.rs:42), add to the struct construction (alongside the other fields):

```rust
        let (frame_tx, _frame_rx) = watch::channel(0u64);
```

and include `frame_tx,` in the returned `ServerState { ... }`.

- [ ] **Step 4: Verify it builds**

Run: `cargo build -p vtx-server`
Expected: builds clean (the field is unused for now — that is fine; it is wired in Task 3/4).

- [ ] **Step 5: Commit**

```bash
git add crates/server/src/server.rs
git commit -m "feat: add frame-notification watch channel to ServerState"
```

---

## Task 3: Server-owned drain tick; remove the per-client poll task

**Files:**
- Modify: `crates/server/src/server.rs` (`VtxServer::run` — add drain task; `handle_client` — remove poll task)

- [ ] **Step 1: Add the server-owned drain task in `VtxServer::run`**

In `VtxServer::run`, directly after the autosave `tokio::spawn` block (server.rs ~87), add:

```rust
        // Server-owned drain tick: drains all sessions ~125x/sec regardless of
        // attached clients (fixes the detached-session memory leak) and bumps
        // the frame counter so attached clients re-render.
        let drain_state = Arc::clone(&self.state);
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_millis(8));
            loop {
                interval.tick().await;
                let mut st = drain_state.lock().await;
                if drain_all_sessions(&mut st) {
                    st.frame_tx.send_modify(|v| *v = v.wrapping_add(1));
                }
            }
        });
```

- [ ] **Step 2: Remove the per-client poll task in `handle_client`**

Delete the entire poll-task block (server.rs ~126-149, the `let poll_state = ...; let poll_handle = tokio::spawn(async move { loop { sleep(8ms) ... } });`). Also delete the `poll_handle` abort if one exists later in the function (search for `poll_handle`).

- [ ] **Step 3: Verify it builds (the `pty_tx`/`pty_rx` will be addressed in Task 4)**

Run: `cargo build -p vtx-server` 2>&1
Expected: it may warn that `pty_tx` is now unused — that is expected; Task 4 removes the channel. If it is a hard error rather than a warning, proceed to Task 4 before committing.

- [ ] **Step 4: Commit**

```bash
git add crates/server/src/server.rs
git commit -m "feat: move PTY drain to a single server-owned tick (fixes H1)"
```

---

## Task 4: Client renders on `frame_rx.changed()`

**Files:**
- Modify: `crates/server/src/server.rs` (`handle_client`: subscribe `frame_rx`; replace the `pty_rx` select arm; remove the `pty_tx`/`pty_rx` channel)

- [ ] **Step 1: Subscribe to the frame channel and drop the pty channel**

In `handle_client`, remove the `let (pty_tx, mut pty_rx) = mpsc::channel::<()>(64);` line. After the `ClientState` is constructed (server.rs ~124), add:

```rust
    // Re-render this client whenever any session drains output.
    let mut frame_rx = {
        let st = state.lock().await;
        st.frame_tx.subscribe()
    };
```

- [ ] **Step 2: Replace the render select arm**

Change the first arm of the main `tokio::select!` from `_ = pty_rx.recv() => {` to:

```rust
            _ = frame_rx.changed() => {
```

Leave the arm body unchanged (it already builds the frame under the lock, drops the lock, then writes — the H2 fix).

- [ ] **Step 3: Verify it builds and the suite is green**

Run: `cargo build -p vtx-server && cargo test -p vtx-server`
Expected: builds clean (no unused `pty_tx`/`pty_rx` warnings) and all existing server tests pass.

- [ ] **Step 4: Manual smoke test (real terminal)**

Run the app, open a pane, run a command (e.g. `ls`), and confirm output renders. Detach, run a long-output command in the detached session via a second attach, confirm memory is stable. (This is the H1 behavior; it is not unit-testable here.)

Build/run: `cargo build --release && ./target/release/vtx` (Linux). Document the result in the commit message.

- [ ] **Step 5: Commit**

```bash
git add crates/server/src/server.rs
git commit -m "feat: clients render on the server frame clock instead of a per-client poll"
```

---

## Task 5: Drain-all integration test (H1 regression guard)

**Files:**
- Test: `crates/server/src/server.rs` (`mod tests`)

- [ ] **Step 1: Write the failing test**

Add to the `tests` module in `server.rs`:

```rust
    use crate::session::Session; // adjust if Session is re-exported elsewhere
    use std::time::{Duration, Instant};
    use vtx_core::PaneId;

    /// H1 (part 2): a session with a pane that produced output gets drained by
    /// `drain_all_sessions` even with no client attached, and the output lands
    /// in the pane's grid.
    #[test]
    fn drain_all_sessions_drains_a_detached_session() {
        let mut state = ServerState::new(Config::default());

        // Build a session with one pane running `/bin/echo hi`.
        let mut session = Session::new(SessionId(1), "t".into(), 40, 10);
        // NOTE: replace the next two lines with however Session exposes adding a
        // pane in this codebase (see how NewSession/Split build panes in
        // handle_message). The pane must run "/bin/echo" so it emits "hi\n".
        let pane = crate::pane::Pane::spawn(PaneId(1), 40, 10, "/bin/echo").unwrap();
        session.windows[0].panes.insert(PaneId(1), pane);
        state.sessions.insert(SessionId(1), session);

        // Poll the drain for up to 1s for the echo output to arrive.
        let deadline = Instant::now() + Duration::from_secs(1);
        let mut drained = false;
        while Instant::now() < deadline {
            if drain_all_sessions(&mut state) {
                drained = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(drained, "echo output should have been drained");
    }
```

- [ ] **Step 2: Run test to verify it fails (or compiles-then-passes)**

Run: `cargo test -p vtx-server drain_all_sessions_drains_a_detached_session -- --nocapture`
Expected: First run likely FAILS to compile because the exact `Session` constructor / pane-insertion API differs. Fix the two marked lines to match the real API (read `Session` and the `NewSession` handler in `handle_message`), then it should PASS. The point of the test is that draining works without any client task — which it now does via Task 3.

- [ ] **Step 3: (No implementation step — behavior already exists from Tasks 1-4.)**

This test characterizes the H1 fix; no new production code. If it fails *after* the API lines are correct, that is a real bug in Tasks 1-4 — fix there, not in the test.

- [ ] **Step 4: Run the full server suite**

Run: `cargo test -p vtx-server`
Expected: all pass.

- [ ] **Step 5: Commit**

```bash
git add crates/server/src/server.rs
git commit -m "test: guard that detached sessions are drained (H1)"
```

---

## Self-Review

- **Spec coverage (frame-clock section):** server-owned tick ✅ (Task 3), drain-all-sessions/H1 ✅ (Tasks 1,3,5), frame notification to clients ✅ (Tasks 2,4). Render-on-demand "only when dirty": partially — the watch counter only bumps on drained output, so idle ⇒ no client wakeups ✅; animation/composer dirtiness hooks are added by their own plans. H9 explicitly scoped out (documented). Animations and composer: separate plans.
- **Placeholder scan:** Task 5 contains two clearly-marked lines that must be adapted to the real `Session`/pane API — this is unavoidable because the constructor wasn't read in this plan; the step calls it out explicitly and tells the engineer exactly where to look (`handle_message` `NewSession`/`Split`). All other steps have complete code.
- **Type consistency:** `drain_all_sessions(&mut ServerState) -> bool` used identically in Tasks 1, 3, 5. `frame_tx: watch::Sender<u64>` defined (Task 2), bumped via `send_modify` (Task 3), subscribed via `.subscribe()` (Task 4) — consistent.

## Follow-on plans (write after this lands, against the real API)

- **Animation system** — registry + easing + layout interpolation, wired to the frame clock's per-tick advance and dirtiness.
- **IRC composer** — activation predicate, client line-editor, server row reservation + IPC, alt-screen auto-bypass.
- **H9** — plugin dispatch off the event loop (needs a plugin actor/thread design, not a timeout wrapper).
