# vtx Code Audit — 2026-06-25

Full-codebase audit of vtx (~11.5k lines Rust, 9 crates). Findings were produced by
per-subsystem review and cross-checked against source. The five Critical items were
manually re-verified at the cited line numbers.

Severity key: **Critical** = data loss, crash, or security escape reachable from normal
use or untrusted input · **High** = crash/corruption/DoS under realistic conditions ·
**Medium** = wrong behavior or fragility · **Low** = polish / latent risk.

---

## Critical

### C1 — PTY child processes are never killed or reaped
`crates/server/src/pane.rs:48-97` (all `spawn*` fns); no `Drop for Pane`.
`spawn` reads `child.process_id()` then drops the `Child` handle immediately, so the
process is never `wait()`ed (→ zombie on exit) and never `kill()`able. On pane/session
close the backend drops `master`, but the reader thread holds a *cloned* read fd, so the
PTY master stays open, the child never gets EOF/HUP, the child keeps running, and the
reader thread blocks in `read()` forever. Net per closed pane: a leaked OS thread + an
orphaned child + leaked fds. Killing a session orphans every child in it.
**Fix:** store the `Box<dyn Child + Send + Sync>` in `Pane`; add `Drop` that `kill()`s
then `wait()`s the child (closing the slave → reader hits EOF and the thread joins). Reap
on `dead` detection too (see H-series).

### C2 — Out-of-bounds panic in `insert_chars` (ICH) at the deferred-wrap column
`crates/terminal/src/grid.rs:298`.
After printing into the last column the cursor sits at `cursor_x == cols` (deferred wrap).
A subsequent `CSI @` runs `*self.cell_mut(x, y)` with `x == cols` → `idx = y*cols + cols`;
on the last row that equals `cells.len()` → panic (crashes the whole multiplexer). Reachable
from ordinary PTY output. `delete_chars`/`erase_chars` share the unclamped pattern.
**Fix:** clamp at entry — `let x = self.cursor_x.min(self.cols.saturating_sub(1));` and
early-return if `cols == 0`. Apply to all three ICH/DCH/ECH paths.

### C3 — Terminal resize is silently ignored; the screen corrupts until restart
`crates/renderer-tty/src/lib.rs:160-162` + `crates/client/src/client.rs:1162-1170`.
`render_frame` calls `ensure_size(self.screen_cols, self.screen_rows)` — passing the
renderer's *own* current size to the only function that would update it, so the
`cols != self.screen_cols` check is always false and the buffers never reallocate. The
client's `TermEvent::Resize` arm only forwards a `Resize` to the server; it never tells the
renderer. After any resize the server lays panes out for the new geometry but `set_back`
indexes with the stale `screen_cols` → content wraps/clips and the status bar lands on the
wrong row, permanently. `screen_size()` (mouse hit-testing) is stale too.
**Fix:** add `pub fn resize(&mut self, cols, rows)` that calls `ensure_size` with the *new*
size, and call it from the client's Resize arm; or have `render_frame` query
`terminal::size()` each frame.

### C4 — Lua plugins run completely unsandboxed
`crates/plugin/src/lua_plugin.rs:57` (`Lua::new()`).
`Lua::new()` loads `os` and `io`. A third-party plugin auto-loaded from
`~/.config/vtx/plugins/` can `os.execute("rm -rf ~")`, open/read/write arbitrary files,
exfiltrate env via `os.getenv`, or `os.exit()` the whole server at load time. No
`sandbox(true)`, and `require`/`load`/`loadfile` are available.
**Fix:** construct with an explicit allowlist
(`Lua::new_with(StdLib::TABLE | STRING | MATH, …)`), and/or `lua.sandbox(true)`; strip
`os`/`io`/`require`/`load*` from globals. Decide and document the plugin trust model.

### C5 — WASM plugins have no fuel/epoch limit and no memory cap
`crates/plugin/src/wasm_plugin.rs:46` (`Engine::default()`), `:52` (`Store::new`).
No `consume_fuel`/`epoch_interruption`, no `StoreLimits`/`ResourceLimiter`. A WASM plugin
can `loop {}` in a hook (blocking every session — hooks run on the event loop, see H-series)
or grow memory to wasmtime's 4 GiB default and OOM the host.
**Fix:** `Config::consume_fuel(true)` (+ `set_fuel` per call) or `epoch_interruption(true)`
with a watchdog; install `StoreLimitsBuilder` to cap memory/tables. Refill fuel per dispatch.

---

## High

- **H1 — Detached sessions are never drained → unbounded memory.**
  `server.rs:126-149`, `pane.rs:68`. Draining is driven only by the per-client poll task,
  which is `abort()`ed on disconnect. A detached session whose child keeps producing output
  (`yes`, a build, `tail -f`) pushes into the unbounded mpsc channel with no consumer → RAM
  grows without bound. **Fix:** a single server-owned drain task that runs regardless of
  client count, or a bounded/coalescing channel.

- **H2 — Global state mutex held across the socket write in the render arm.**
  `server.rs:184-193`. `state.lock().await` stays in scope through
  `w.write_all(json).await`; a slow/backpressured client stalls every other client's task
  and autosave. (The `msg_rx` arm at 200-213 correctly drops the lock first — only this arm
  is wrong.) **Fix:** build JSON under the lock, `drop(st)`, then write.

- **H3 — Blocking, uncached `git` + `/proc` reads on the render hot path under the lock.**
  `server.rs:1533-1562`, `status.rs:38-88`. `git_info` forks up to 3 `git` processes
  synchronously per render (no cache, unlike `sys_info`), on the tokio worker, while holding
  the global mutex. A busy pane (renders up to every 8 ms) → dozens of `git` forks/sec
  serializing all clients. **Fix:** cache `git_info` per-cwd with a TTL; gather off the lock
  path via `spawn_blocking`/the drain task.

- **H4 — No Unicode width handling; wide & combining chars corrupt the grid.**
  `grid.rs:137-157` (model advances `cursor_x` by 1 for every char; no `unicode-width` in
  the workspace) and `renderer-tty/src/lib.rs:704-743` (renderer assumes 1 column per cell).
  CJK/emoji (width 2) shift everything after them left by one per glyph; zero-width/combining
  marks overwrite the next cell. **Fix:** add a width/continuation flag to `Cell`; use
  `unicode-width` in `put_char` (write glyph + spacer, advance 2; width-0 attaches to prior
  cell) and have the renderer skip spacers and account for the 2-col advance.

- **H5 — O(n²) scrollback eviction under output flood.**
  `grid.rs:189-191`. Scrollback is `Vec<Vec<Cell>>`; at the 100k limit every new line does
  `scrollback.remove(0)` (O(n) shift) → quadratic CPU that pins a core during `cat largefile`.
  **Fix:** `VecDeque` + `pop_front()`.

- **H6 — `Color::from_hex` panics on non-ASCII config and poisons the config mutex.**
  `crates/core/src/lua_config.rs:13-19`. The length guard checks byte length (`len()!=6`) then
  byte-slices `&s[0..2]`; a 6-byte string with a non-ASCII char (e.g. `"a€bc"`) slices mid-codepoint
  → panic. Reachable from `status_bg`/`status_fg`/segment colors. The panic fires inside the
  `__newindex` callback while the config mutex is locked → poisons it → later `lock().unwrap()`
  crashes the process. **Fix:** `if s.len() != 6 || !s.is_ascii() { return None; }`.

- **H7 — WASM host reads use unchecked `i32 as usize` ptr/len arithmetic.**
  `wasm_plugin.rs:415-420`, `381-386`. A plugin passing negative `ptr`/`len` makes
  `start + len` overflow (debug panic / release wrap-then-OOB-slice-panic), unwinding out of
  the host call and killing the server task. **Fix:** reject negatives, `checked_add`,
  bounds-check `end <= data.len()` before slicing.

- **H8 — `Ctrl`+non-letter key underflows the control-byte math.**
  `client.rs:1467` (also `1419`, `gpu_attach.rs:425`).
  `(c as u8).to_ascii_lowercase() - b'a' + 1` assumes `a..=z`. `Ctrl-Space`, `Ctrl-\`/`]`/`^`/`_`,
  `Ctrl-digit` underflow → debug panic / release junk byte to the PTY. **Fix:** compute control
  codes via `(c as u8) & 0x1f` with a guard, mapping the symbol keys correctly.

- **H9 — Plugin hooks run synchronously on the async event loop.**
  `plugin/src/lib.rs:74` dispatched from `server.rs:269,392,442,728,920`. No timeout, no
  `spawn_blocking`; combined with C4/C5 any slow/looping/`os.execute` plugin freezes every
  session. **Fix:** dispatch under a bounded timeout off the event-loop thread; add a Lua
  interrupt and use WASM fuel/epoch.

- **H10 — Unbounded plugin action queue → host OOM.**
  `lua_plugin.rs:37` + push sites; `server.rs:1029`. `while true do vtx.notify() end` grows the
  `Vec<PluginAction>` without bound; even finite floods do unbounded host work (spawning
  Split/RunCommand). **Fix:** cap actions per dispatch and individual payload sizes.

- **H11 — Prefix state gets stuck after `Ctrl-a` + Alt-combo / Shift+PageUp.**
  `client.rs:1296-1391`. The Shift+PageUp early-return and the whole Alt block run *before*
  the `prefix_active` reset at 1384, so those keys leave `prefix_active == true` and the next
  ordinary keystroke is wrongly eaten as a prefix command. **Fix:** snapshot and clear
  `prefix_active` at the top of `process_key`, then branch on the snapshot.

---

## Medium

- **M1 — Alt/Meta keys never reach the shell.** `client.rs:1314-1373,1460-1462` (+gpu). Unhandled
  `Alt+char` returns `None`/`vec![]`, so `Alt-f`/`Alt-b`/`Alt-.` etc. are eaten. **Fix:** forward
  ESC-prefixed (`0x1b` + char bytes).
- **M2 — `CSI 2 J` (ED 2) wrongly homes the cursor.** `grid.rs:350-355`. Erase-in-display must not
  move the cursor; `clear()` resets it to 0,0, so TUIs that clear-then-draw-relative mis-place output.
  **Fix:** drop the cursor reset from `clear()`; reset explicitly only at RIS/alt-screen sites.
- **M3 — Resize while on the alternate screen destroys the primary buffer.** `grid.rs:97-119`.
  `resize` unconditionally reallocates `alt_cells` to blanks; when on the alt screen that is the saved
  primary content, so leaving the alt screen (e.g. quitting vim) shows a blank shell. **Fix:** resize/copy
  `alt_cells` like `cells`.
- **M4 — Colon-form SGR color (`38:2:…`, `38:5:…`) is ignored.** `parser.rs:328-371`. Only the
  semicolon form is parsed; the ISO colon sub-parameter form (emitted by many modern apps) is dropped.
  **Fix:** inspect param sub-parameters for codes 38/48/58 first.
- **M5 — u16 overflow on large cursor-move counts.** `parser.rs:132,137,147`. `CSI 65535 B` overflows
  the u16 add before clamping (debug panic / release wrong position). **Fix:** `saturating_add`.
- **M6 — `cols==0`/`rows==0` underflow panics.** `parser.rs` & `grid.rs` mix bare `- 1` with
  `saturating_sub`. A pane collapsed to 0 in either axis panics on the next sequence. **Fix:** clamp
  `resize` to ≥1×1 or use `saturating_sub(1)` throughout.
- **M7 — `set_back` clips by linear index, not column.** `renderer-tty/src/lib.rs:680-685` (same in gpu).
  Border/popup loops can write past the row width and wrap onto the next row. **Fix:** early-return when
  `x >= screen_cols || y >= screen_rows`.
- **M8 — Byte-slice panic in the settings menu on non-ASCII labels.** `renderer-tty/src/lib.rs:498,581,
  609-610`. `&padded[..width-2]` slices by bytes; the active-theme `" ●"` marker (multibyte) makes the
  cut land mid-codepoint. **Fix:** truncate by `chars()`, size by `chars().count()`.
- **M9 — Right-status segments positioned by byte length.** `renderer-tty/src/lib.rs:380-385` (+gpu).
  Non-ASCII segment text reserves too much width → mis-aligned right side. **Fix:** count chars/display width.
- **M10 — Plugin `KillPane` leaves empty windows + stale `focused_pane`.** `server.rs:1147-1159`.
  Unlike the main handler it never collapses empty windows/sessions. **Fix:** share the collapse helper.
- **M11 — `pane.dead` is set but never acted on.** `pane.rs:284-286`. A child that exits on its own
  leaves a frozen pane and an unreaped zombie. **Fix:** sweep dead panes through the removal/collapse path.
- **M12 — `RespawnPane` removes the pane before spawning.** `server.rs:897-903`. On spawn failure the
  layout/`focused_pane` still reference the removed pane → broken window. **Fix:** spawn first, swap on success.
- **M13 — Multi-client size fighting + O(clients×panes) drain.** `server.rs:20-25,135-143,332-345`.
  Each client resizes shared panes (last-writer-wins) and independently drains all sessions every 8 ms.
  **Fix:** single shared drain task; size to the min of attached clients.
- **M14 — `RunCommand`/`Popup` = arbitrary host exec with fragile escaping.** `lua_plugin.rs:168,234`,
  `server.rs:1088` (`sh -c '…'`, only single-quotes escaped). **Fix:** spawn via argv vector; gate behind
  an explicit per-plugin permission.
- **M15 — `lock().unwrap()` poisoning crash paths.** plugin/server/`status.rs:92,127`. One panic under a
  lock cascades to every later `unwrap`. **Fix:** `unwrap_or_else(|e| e.into_inner())` or `parking_lot`.
- **M16 — GPU `send()` blocks the winit UI thread on socket I/O.** `gpu_attach.rs:68-84`. Every keystroke
  `block_on(write_all)`; backpressure freezes the window. **Fix:** move writes to an async task fed by a channel.
- **M17 — GPU client hardcodes the prefix to Ctrl-A.** `gpu_attach.rs:286`. Ignores `cfg.prefix_key`.
  **Fix:** thread the configured prefix in.

## Low

- **L1 — `scrollback`/`history-limit` config is unbounded** (`lua_config.rs:232-237`, `tmux_compat.rs:144-147`)
  → config-driven huge allocation; also truncates on 32-bit. Clamp to a sane max.
- **L2 — tmux tokenizer ignores backslash escaping** and conflates `default-command`/`default-shell`,
  `prefix2`/`prefix` (`tmux_compat.rs:83-147`) → silent mis-parse of valid configs.
- **L3 — No size bound on IPC `Input.data` / `PaneRender.content`** (`ipc.rs:46,149-155`); `Vec<u8>` also
  serializes as a JSON int array (~4-6×). Bound message length in the framed reader; consider base64.
- **L4 — Unbounded, unfiltered OSC window-title input** (`parser.rs:58-62`) → memory growth / control-char
  injection into the host title. Cap length, strip C0/C1.
- **L5 — `HIDDEN`/`BLINK` SGR attributes dropped by the diff** (`renderer-tty/src/lib.rs:781-806`) → SGR-8
  concealed text (password echo) renders visibly.
- **L6 — GPU atlas only covers ASCII 0x20-0x7E** (`renderer-gpu/src/atlas.rs:242-251`) → all box-drawing /
  powerline glyphs render blank in the GPU path.
- **L7 — Degenerate zero-size panes / zero-size GPU buffer on tiny dimensions** (`layout/src/lib.rs:462-499`,
  `renderer-gpu/src/lib.rs:398-404`). Clamp a minimum pane size; floor GPU buffer size at 1.
- **L8 — Plugin loading follows symlinks and silently overwrites name collisions**
  (`lua_plugin.rs:42-50`, `wasm_plugin.rs:31-41`). Skip non-regular files; warn on collision.
- **L9 — `serde_json::to_string(...).unwrap()` on the render/response hot path** (`server.rs:187,207`).
  Handle the error instead of panicking the client task.
- **L10 — `Detach` doesn't clear `cs.attached_session`** (`server.rs:909-927`); `input_handle.abort()`
  can't cancel the blocking reader (`client.rs:194-229`); selection-start uses non-saturating clamp
  (`client.rs:657-658`). Minor robustness.

---

## Verified-correct (no action)

- Cancel-safe IPC invariant holds in both client and server (dedicated reader task owns the socket read
  half; `select!` only receives from channels). `ipc.rs` has no `read_line`/`select!`.
- Terminal-state restoration is sound: `TtyRenderer::cleanup` runs in `Drop` (raw mode, alt-screen, mouse
  reporting restored on normal exit, `?`-error, and panic unwind).
- Layout tiling has no gap/overlap bug: `split_area` and `borders_inner` both reserve exactly one border
  cell; children + border tile the parent exactly. Split-tree remove/swap/neighbor paths are bounds-safe.
- All IPC enums/structs round-trip; newline framing is safe (serde_json escapes newlines in strings).

---

## Cross-cutting themes

1. **Process & resource lifecycle** (C1, H1, M11, M12) — children, threads, fds, and channels are not
   reclaimed on close; the cleanest fix is a real `Drop for Pane` + a single server-owned drain/reap task.
2. **Unicode correctness** (H4, M8, M9, plus byte-vs-char slicing in H6) — the codebase assumes 1 byte =
   1 char = 1 column in many places. A `Cell` width flag + `unicode-width` is the root fix.
3. **u16 coordinate arithmetic** (C2, M5, M6, M7) — inconsistent `- 1` vs `saturating_sub`; a sweep to
   saturating math + a 1×1 size floor removes a whole class of panics.
4. **Plugin trust model** (C4, C5, H7, H9, H10, M14) — plugins are currently fully-trusted arbitrary code.
   Decide the model (trusted vs sandboxed) and enforce it consistently; this is a prerequisite before
   encouraging third-party plugins.
5. **Async loop discipline** (H2, H3, H9, M16) — blocking work (git, plugins, socket writes) runs on the
   event loop, often under the global lock. Move blocking work off-thread and shrink lock scope.

## Relevance to the planned features

- **Animations** need a frame clock; today rendering is purely event-driven. That same new tick is the
  right place to also fix render coalescing (client L-series) and could drive cached status refresh (H3).
- **Pinned input bar** reserves bottom row(s) exactly like the status bar (`rows-1`). The resize bug (C3)
  and `set_back` column-clipping (M7) sit directly on that path and should be fixed first, or the input bar
  will inherit the corruption.
