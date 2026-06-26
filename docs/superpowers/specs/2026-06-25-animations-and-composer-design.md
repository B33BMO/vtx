# Design: Animations + IRC-style Composer

Date: 2026-06-25
Status: Approved (design), pending implementation plan
Branch base: `fix/audit-criticals` (audit fixes merged/committed)

## Overview

Two user-facing features for vtx, built on one shared piece of infrastructure:

1. **Animations** — smooth transitions for pane splits/close, window/tab switching,
   floating popups, and composer/status accents.
2. **IRC-style composer** — an opt-in input line pinned to the bottom of the screen.
   You compose a line and press Enter to send it to the focused pane's shell, with
   command output scrolling above — like a chat client or classic line terminal.

The shared substrate is a **server-side frame clock**. Today rendering is purely
event-driven (server `Render` messages triggered by PTY output + client input
events); there is no frame tick. Both features need a steady clock, and building it
also resolves two deferred audit findings:

- **H1** (detached sessions never drained → unbounded memory): the frame clock drains
  *all* sessions each tick, regardless of attached clients.
- **H9** (plugin hooks block the event loop): hook dispatch moves off the loop with a
  timeout as part of the loop restructure.

## Goals

- A 60 FPS-capped, render-on-demand server tick that drives drain + animation + composer.
- Configurable, opt-out-able animations for the four scopes above.
- An opt-in composer that feels like IRC: line-buffered input, output scrolls above,
  auto-bypass for full-screen apps.
- No regressions to the existing event-driven responsiveness when idle (idle ⇒ no renders).

## Non-goals (YAGNI for v1)

- True alpha/blur compositing (a TTY can't do it; "fade" is a dimming ramp).
- Multiline composer, per-pane composers, tab-completion in the composer.
- GPU-renderer animation parity (TTY-first; the GPU path is feature-gated and already
  behind the TTY renderer in features).
- Smooth-scroll / cursor-trail micro-animations beyond the chosen four scopes.

---

## Architecture

### Component 1 — Server-side frame clock

**What it does.** Replaces the current per-client poll tasks (`server.rs` poll loop,
~8ms `try_recv` drain) with a single server-owned `tokio::time::interval` running at a
target frame rate (default 60 FPS, configurable). It is the single driver of time-based
work.

**Per tick:**
1. **Drain all sessions** — iterate every session's panes and `try_recv` pending PTY
   output into the parser grids. This runs regardless of whether any client is attached
   (fixes H1). Cheap when there's nothing to drain.
2. **Advance animations** — step the animation registry (Component 2) by elapsed wall time.
3. **Mark dirty** — a session is dirty if it drained new output, has an in-flight
   animation, or has a composer cursor blink due.
4. **Emit `Render` on demand** — for each attached client whose session is dirty, compose
   and send one `Render`. If nothing is dirty, emit nothing. Fully idle ⇒ zero renders.

**How it uses it / depends on.** Owns the session map and layout (already does). Needs a
monotonic clock — `std::time::Instant` (note: the codebase forbids `Instant::now()` only
inside *workflow scripts*; normal server code uses it freely, e.g. `status.rs`). Replaces
the disconnect-aborted per-client poll task, so the lock-scope fix (H2) and single-drain
goal (audit M13) are subsumed here.

**Plugin dispatch (H9).** Hook dispatch moves to `tokio::task::spawn_blocking` wrapped in a
bounded `tokio::time::timeout`, so a slow/looping plugin can't stall the tick. Combined with
the already-shipped Lua sandbox (C4) and WASM fuel (C5), runaway plugins are bounded.

**Backpressure.** Render writes already drop the global lock before the socket write (H2,
shipped). A slow client stalls only its own send, not the tick.

### Component 2 — Animation system

**What it does.** A registry of active animations on the server. Each animation:

```
struct Animation {
    kind: AnimationKind,     // PaneSplit, PaneClose, WindowSwitch, PopupFade, Accent
    start: Instant,
    duration: Duration,
    easing: Easing,          // Linear, EaseOut, EaseInOut
    target: AnimationTarget, // rects / pane id / popup id / accent descriptor
}
```

On each tick, `progress = ease(elapsed / duration)` in `[0,1]`; the layout/render
composition interpolates from this. Completed animations are removed (and any deferred
state change, e.g. actually removing a closed pane, is applied at `progress == 1`).

**Effects, TTY-appropriate:**
- **PaneSplit / PaneClose / Zoom** — interpolate the pane rect(s) over the duration so
  regions slide/resize rather than snap. Layout resolution (`vtx-layout`) gains an
  interpolation helper; the resolved (interpolated) rects feed the existing `Render`.
- **WindowSwitch** — a short horizontal slide/offset of the new window's content.
- **PopupFade** — popups and context/settings menus ramp their fg/bg from dim → full
  (dimming approximation of a fade), optionally combined with a 1–2 frame box scale-in.
- **Accent** — composer send-flash and status-segment update pulses (brief color ramp).

The renderer stays dumb: it draws whatever interpolated rects/colors the server computes.

**Config (Lua):**
```lua
animations = {
  enabled = true,        -- master switch (false = reduce motion / accessibility)
  duration_ms = 150,
  easing = "ease_out",   -- "linear" | "ease_out" | "ease_in_out"
}
```

**Depends on.** The frame clock (advance), `vtx-layout` (rect interpolation), config.

### Component 3 — IRC composer

**Placement & model.** One composer line at the bottom of the screen, directly above the
status bar, targeting the **focused pane**. Opt-in; default off.

```
┌──────────────────────────────┐
│ $ ls                         │  pane area = rows - 2 when composer visible
│ file1  file2  file3          │
│   (output scrolls up)        │
├──────────────────────────────┤
│ › type a line, press Enter_  │  composer row
├──────────────────────────────┤
│ session  1:zsh  2:vim  14:02 │  status row
└──────────────────────────────┘
```

**Auto-bypass.** When the focused pane's grid has `using_alt_screen == true` (vim, htop,
less, etc.), the composer hides and input passes through raw; pane area returns to
`rows - 1`. When the app exits the alt screen, the composer returns. This is the key
behavior that lets the composer coexist with full-screen apps.

**Ownership split.**
- **Client** owns the composer buffer and line editing, and renders the composer row
  locally (the client already draws overlays like context menus directly). On Enter it
  sends `Input { data: <line> + "\n" }` to the focused pane. No per-keystroke round-trip.
- **Server** reserves the composer row in layout: when composer is active for the session,
  panes are laid out in `rows - 2` and the status bar/Render account for the reserved row.
  The `Render` message tells the client the composer row's `y` so the client draws there.

**Activation state.** Composer is active when: config/keybind enabled **AND** focused pane
not in alt-screen. The client tracks this and tells the server whether to reserve the row
(so layout and the client's overlay stay in sync). State transitions (focus change, app
enters/leaves alt-screen) re-evaluate activation each frame.

**v1 line editing.** Single line. Supported: insert, cursor left/right, Home/End,
Backspace/Delete, history Up/Down through previously sent lines (per session), Enter to
send, Esc to exit composer mode for that pane. Wide-char aware (uses the H4 width logic).

**Config (Lua):**
```lua
composer = {
  enabled = false,       -- opt-in
  prompt = "› ",
  -- toggle keybinding registered as a prefix action (e.g. prefix + i)
}
```

**Depends on.** Frame clock (cursor blink, send-flash accent), `using_alt_screen` (grid,
already exists), layout row reservation, IPC additions below. The composer row rendering
relies on the shipped C3 (resize) and M7 (set_back column-clip) fixes.

---

## Data flow

```
PTY output ─┐
            ▼
   [frame clock tick] ── drain all sessions ──► parser grids
            │
            ├── advance animation registry
            │
            └── per attached+dirty client:
                   compose Render (interpolated rects, status, composer row y)
                   ──► client
                         │
                         ├── render pane frame (server-composed)
                         └── draw composer row locally from local buffer

keystroke ─► client
              ├── composer active? → edit local buffer; Enter ⇒ Input{line+"\n"}
              └── else → existing prefix state machine / raw Input
```

## IPC changes (`vtx-core/src/ipc.rs`)

- `ServerMsg::Render` gains optional composer layout info: whether a composer row is
  reserved and its `y` coordinate (so the client knows where to draw and that pane area
  is `rows - 2`). Animation interpolation is already reflected in the rects/contents the
  server sends, so no per-animation wire type is needed.
- `ClientMsg` gains a message to toggle/inform composer activation (so the server reserves
  or releases the row): e.g. `SetComposer { active: bool }`. Sent on enable/disable and on
  alt-screen enter/leave for the focused pane.
- Respect the audit's L3 note: bound message sizes in the framed reader (composer lines and
  `Input` payloads are user-controlled).

## Config schema additions (`vtx-core` lua_config)

`animations { enabled, duration_ms, easing }` and `composer { enabled, prompt }` as above,
hot-reloadable via the existing `SourceConfig` path. Defaults preserve current behavior
(animations on with a subtle 150ms; composer off).

## How the deferred audit findings fold in

- **H1** — the frame clock's "drain all sessions every tick" replaces the per-client poll,
  so detached sessions are drained and the unbounded-channel growth is gone.
- **H9** — plugin hook dispatch becomes `spawn_blocking` + `timeout` within the new loop.
- **M13** (multi-client redundant drain) and the single-drain goal are subsumed by the
  single server-owned tick.

## Testing strategy

- **Animation math** — easing functions and rect interpolation are pure: unit-test
  `ease(t)` endpoints/monotonicity and that interpolated rects tile correctly at
  `progress` 0, 0.5, 1 (extends the existing `vtx-layout` coverage).
- **Composer buffer/editing** — the line editor (insert, cursor moves, backspace/delete,
  history) is pure client state: unit-test it directly, including wide-char cursor steps.
- **Activation logic** — `composer_active(enabled, alt_screen, focused)` is a pure
  predicate: unit-test the truth table (esp. auto-bypass on alt-screen).
- **Frame-clock dirty logic** — the "should this tick emit a Render" decision is a pure
  function of (drained, animating, blink-due): unit-test it; the async wiring is verified
  by inspection + manual run.
- **Drain-all-sessions (H1)** — a test that a detached session with pending output still
  gets drained by the tick (mirrors the C1 reaping test style).
- **Manual/visual verification** — animations and composer rendering need a real terminal
  (and ideally the Linux `srv` box). This is the same gate H4 (wide chars) needs; bundle a
  visual-verification pass for all rendering-affecting changes.

## Risks & open questions

- **Idle CPU** — a 60 FPS interval that wakes but emits nothing must be genuinely cheap.
  Mitigation: if no animation and no composer-blink, fall back to longer interval / pause
  the tick until the next event (output, input, animation start). The tick is a heartbeat,
  not a mandatory 60 Hz render.
- **Loop restructure risk** — replacing the per-client poll with a central tick is the
  highest-risk change (touches the cancel-safe IPC reader pattern; must preserve it).
  Sequence it first, behind the existing behavior, and verify drain + responsiveness before
  layering features on.
- **Composer/layout coordination** — the client-draws / server-reserves split must stay in
  sync across focus changes and alt-screen transitions; the per-frame activation
  re-evaluation is the guard.
- **Animation during resize** — animations in flight when the terminal resizes must clamp
  to the new geometry (relates to the C3 resize fix).

## Suggested build order

1. Server-side frame clock + drain-all (lands H1, M13); verify parity with current behavior.
2. Plugin dispatch off-loop with timeout (H9).
3. Animation registry + easing + layout interpolation; wire the four scopes one at a time.
4. Composer: activation predicate → buffer/editor → row reservation (IPC) → client render →
   auto-bypass.
5. Config plumbing + hot reload for both.
6. Visual verification pass (incl. H4 wide chars) on a real terminal.
