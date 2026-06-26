# Animation System Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a server-side animation system driven by the existing frame clock, and use it to animate pane-close geometry — the vertical slice that proves the architecture end-to-end. Remaining scopes (split, window-switch, popup-fade, accents) are follow-on tasks listed at the end.

**Architecture:** Pure easing + rect-interpolation helpers (unit-tested in isolation), an `AnimationRegistry` held on `ServerState`, and integration with the server-owned drain tick: each tick advances animations and bumps the `frame_tx` watch counter while any animation is in flight, so clients re-render interpolated frames even without PTY output. The render composition interpolates a pane's rect when an animation targets it.

**Tech Stack:** Rust (edition 2024), the existing `vtx-core`, `vtx-layout`, `vtx-server` crates, `std::time::Instant`/`Duration`. Builds on the frame clock from `2026-06-25-frame-clock-foundation.md` (already merged): `drain_all_sessions`, `ServerState.frame_tx: watch::Sender<u64>`, and the drain tick in `VtxServer::run`.

---

## File Structure

- Create: `crates/core/src/animation.rs` — `Easing` enum + `ease(easing, t) -> f32`. Pure, shared (config names an easing).
- Modify: `crates/core/src/lib.rs` — `pub mod animation;`.
- Modify: `crates/layout/src/lib.rs` — add `Rect::lerp(from, to, t) -> Rect`.
- Modify: `crates/core/src/lua_config.rs` — parse `animations { enabled, duration_ms, easing }`.
- Create: `crates/server/src/animation.rs` — `AnimationKind`, `Animation`, `AnimationRegistry` (pure registry logic).
- Modify: `crates/server/src/lib.rs` — `mod animation;`.
- Modify: `crates/server/src/server.rs` — hold an `AnimationRegistry` on `ServerState`; advance it + bump `frame_tx` in the drain tick; interpolate the closing pane's rect in `build_render_msg_scrolled`; register a close animation on `KillPane`.

Confirmed facts (already read):
- `Rect { x: u16, y: u16, cols: u16, rows: u16 }` with `Rect::center` in `crates/layout/src/lib.rs:135`.
- `LayoutNode::resolve(area: Rect) -> Vec<(PaneId, Rect)>` (layout/src/lib.rs:187); `remove(target) -> bool` (257).
- `build_render_msg_scrolled(session, cols, total_rows, scroll_offset, status_cfg) -> ServerMsg` at server.rs:1396; it calls `win.layout.resolve(area)` (1446) and builds `PaneRender { id, x, y, cols, rows, content, ... }` from each `(pid, rect)` (1464-1475).
- Drain tick in `VtxServer::run` (server.rs ~101): `interval(8ms)`, `drain_all_sessions(&mut st)`, `st.frame_tx.send_modify(|v| *v = v.wrapping_add(1))` when output.
- `ServerState::new(config: Config)` exists.

---

## Task 1: Easing functions

**Files:**
- Create: `crates/core/src/animation.rs`
- Modify: `crates/core/src/lib.rs`

- [ ] **Step 1: Write the failing test**

Create `crates/core/src/animation.rs` with:

```rust
//! Easing functions for time-based animations. `t` is normalized progress in
//! `[0,1]`; the return value is the eased progress, also in `[0,1]`.

/// Easing curve, selectable from config.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Easing {
    Linear,
    EaseOut,
    EaseInOut,
}

impl Easing {
    /// Parse a config string; unknown values fall back to `EaseOut`.
    pub fn from_name(s: &str) -> Easing {
        match s {
            "linear" => Easing::Linear,
            "ease_in_out" => Easing::EaseInOut,
            _ => Easing::EaseOut,
        }
    }
}

/// Apply `easing` to normalized progress `t` (clamped to `[0,1]`).
pub fn ease(easing: Easing, t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    match easing {
        Easing::Linear => t,
        // Quadratic ease-out: fast then settle.
        Easing::EaseOut => 1.0 - (1.0 - t) * (1.0 - t),
        // Smoothstep.
        Easing::EaseInOut => t * t * (3.0 - 2.0 * t),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ease_endpoints_are_fixed_and_clamped() {
        for e in [Easing::Linear, Easing::EaseOut, Easing::EaseInOut] {
            assert_eq!(ease(e, 0.0), 0.0, "{e:?} at 0");
            assert_eq!(ease(e, 1.0), 1.0, "{e:?} at 1");
            assert_eq!(ease(e, -5.0), 0.0, "{e:?} clamps below 0");
            assert_eq!(ease(e, 5.0), 1.0, "{e:?} clamps above 1");
        }
    }

    #[test]
    fn ease_out_is_ahead_of_linear_in_the_middle() {
        // Ease-out moves faster early, so at t=0.5 it is past halfway.
        assert!(ease(Easing::EaseOut, 0.5) > 0.5);
    }

    #[test]
    fn from_name_parses_known_and_falls_back() {
        assert_eq!(Easing::from_name("linear"), Easing::Linear);
        assert_eq!(Easing::from_name("ease_in_out"), Easing::EaseInOut);
        assert_eq!(Easing::from_name("ease_out"), Easing::EaseOut);
        assert_eq!(Easing::from_name("bogus"), Easing::EaseOut);
    }
}
```

Then add to `crates/core/src/lib.rs` (with the other `pub mod` lines): `pub mod animation;`

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p vtx-core animation::`
Expected: FAILs to compile first if `lib.rs` not updated; once it compiles, all three tests should pass (they describe the code in the same file). If they pass on first compile, that is acceptable here because the module is brand-new and self-contained — the test still guards the behavior. To honor red-green, temporarily change `EaseOut` to `t` (linear) and confirm `ease_out_is_ahead_of_linear_in_the_middle` FAILS, then restore.

- [ ] **Step 3: (Implementation already written above.)**

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p vtx-core animation::`
Expected: PASS (3 tests).

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/animation.rs crates/core/src/lib.rs
git commit -m "feat: easing functions for animations"
```

---

## Task 2: `Rect::lerp` interpolation

**Files:**
- Modify: `crates/layout/src/lib.rs` (the `impl Rect` block at ~142)

- [ ] **Step 1: Write the failing test**

Add a test module at the end of `crates/layout/src/lib.rs`:

```rust
#[cfg(test)]
mod rect_tests {
    use super::*;

    #[test]
    fn lerp_endpoints_and_midpoint() {
        let a = Rect { x: 0, y: 0, cols: 100, rows: 40 };
        let b = Rect { x: 10, y: 20, cols: 0, rows: 0 };
        assert_eq!(Rect::lerp(a, b, 0.0).cols, 100);
        assert_eq!(Rect::lerp(a, b, 1.0).cols, 0);
        let mid = Rect::lerp(a, b, 0.5);
        assert_eq!((mid.x, mid.y, mid.cols, mid.rows), (5, 10, 50, 20));
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p vtx-layout rect_tests`
Expected: FAIL to compile — `no function lerp on Rect`.

- [ ] **Step 3: Implement `lerp`**

Add inside `impl Rect` (layout/src/lib.rs ~142):

```rust
    /// Linearly interpolate between two rects at progress `t` in `[0,1]`.
    /// Rounds to the nearest cell. Used for geometry animations.
    pub fn lerp(from: Rect, to: Rect, t: f32) -> Rect {
        let t = t.clamp(0.0, 1.0);
        let mix = |a: u16, b: u16| -> u16 {
            (a as f32 + (b as f32 - a as f32) * t).round() as u16
        };
        Rect {
            x: mix(from.x, to.x),
            y: mix(from.y, to.y),
            cols: mix(from.cols, to.cols),
            rows: mix(from.rows, to.rows),
        }
    }
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p vtx-layout rect_tests`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/layout/src/lib.rs
git commit -m "feat: Rect::lerp for geometry interpolation"
```

---

## Task 3: Parse `animations` config

**Files:**
- Modify: `crates/core/src/lua_config.rs`

- [ ] **Step 1: Read the current config shape**

Read `crates/core/src/lua_config.rs` to see how an existing nested config (e.g. the status bar or `scrollback`) is defined on the `LuaConfig` struct, defaulted in `Default`, and parsed from the Lua `__newindex`/table handling. Mirror that pattern exactly for a new `animations` table.

- [ ] **Step 2: Write the failing test**

Add to the existing `mod tests` in `lua_config.rs`:

```rust
    #[test]
    fn animations_config_has_defaults() {
        let cfg = LuaConfig::default();
        assert!(cfg.animations.enabled);
        assert_eq!(cfg.animations.duration_ms, 150);
        assert_eq!(cfg.animations.easing, vtx_core::animation::Easing::EaseOut);
    }
```
(If the test file is inside the crate, `vtx_core::animation::Easing` may need to be `crate::animation::Easing` — use whichever the crate uses for self-references.)

- [ ] **Step 3: Run test to verify it fails**

Run: `cargo test -p vtx-core animations_config_has_defaults`
Expected: FAIL to compile — no `animations` field.

- [ ] **Step 4: Implement**

Add a struct near the other config structs in `lua_config.rs`:

```rust
/// Animation settings.
#[derive(Debug, Clone)]
pub struct AnimationConfig {
    pub enabled: bool,
    pub duration_ms: u64,
    pub easing: crate::animation::Easing,
}

impl Default for AnimationConfig {
    fn default() -> Self {
        AnimationConfig {
            enabled: true,
            duration_ms: 150,
            easing: crate::animation::Easing::EaseOut,
        }
    }
}
```

Add `pub animations: AnimationConfig,` to the `LuaConfig` struct, and `animations: AnimationConfig::default(),` to its `Default` impl. Then, mirroring the existing nested-table parsing you read in Step 1, parse a Lua `animations = { enabled = bool, duration_ms = int, easing = "..." }` table (use `Easing::from_name` for the string). If wiring the full Lua table parse is large, it is acceptable for THIS task to land the struct + defaults + `LuaConfig` field and parse only the fields that are trivial to add; note any unparsed field in the commit message. The defaults are what the animation code reads.

- [ ] **Step 5: Run test to verify it passes**

Run: `cargo test -p vtx-core` (all, incl. the new test)
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/core/src/lua_config.rs
git commit -m "feat: animations config (enabled/duration/easing) with defaults"
```

---

## Task 4: `Animation` + `AnimationRegistry`

**Files:**
- Create: `crates/server/src/animation.rs`
- Modify: `crates/server/src/lib.rs` (add `mod animation;`)

- [ ] **Step 1: Write the failing test**

Create `crates/server/src/animation.rs`:

```rust
//! Active animations, advanced by the server frame clock.

use std::time::{Duration, Instant};
use vtx_core::animation::{ease, Easing};
use vtx_core::PaneId;
use vtx_layout::Rect;

/// What an animation drives.
#[derive(Debug, Clone)]
pub enum AnimationKind {
    /// A pane shrinking to nothing before removal: interpolate its rect to `to`.
    PaneClose { pane: PaneId, from: Rect, to: Rect },
}

/// A single in-flight animation.
#[derive(Debug, Clone)]
pub struct Animation {
    pub kind: AnimationKind,
    start: Instant,
    duration: Duration,
    easing: Easing,
}

/// All active animations for the server.
#[derive(Default)]
pub struct AnimationRegistry {
    items: Vec<Animation>,
}

impl AnimationRegistry {
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn push(&mut self, kind: AnimationKind, now: Instant, duration: Duration, easing: Easing) {
        self.items.push(Animation { kind, start: now, duration, easing });
    }

    /// Drop animations whose duration has elapsed as of `now`. Returns the
    /// kinds of the animations that just completed (so callers can apply the
    /// deferred end-state, e.g. actually remove a closed pane).
    pub fn prune(&mut self, now: Instant) -> Vec<AnimationKind> {
        let mut done = Vec::new();
        self.items.retain(|a| {
            if now.duration_since(a.start) >= a.duration {
                done.push(a.kind.clone());
                false
            } else {
                true
            }
        });
        done
    }

    /// Current interpolated rect for `pane`, if a PaneClose animation targets it.
    pub fn pane_rect(&self, pane: PaneId, now: Instant) -> Option<Rect> {
        self.items.iter().find_map(|a| match &a.kind {
            AnimationKind::PaneClose { pane: p, from, to } if *p == pane => {
                let raw = (now.duration_since(a.start).as_secs_f32()
                    / a.duration.as_secs_f32())
                .clamp(0.0, 1.0);
                Some(Rect::lerp(*from, *to, ease(a.easing, raw)))
            }
            _ => None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(cols: u16, rows: u16) -> Rect {
        Rect { x: 0, y: 0, cols, rows }
    }

    #[test]
    fn pane_rect_interpolates_then_prunes_on_completion() {
        let mut reg = AnimationRegistry::default();
        let t0 = Instant::now();
        reg.push(
            AnimationKind::PaneClose { pane: PaneId(1), from: rect(100, 40), to: rect(0, 0) },
            t0,
            Duration::from_millis(100),
            Easing::Linear,
        );

        // Halfway through: rect is partway shrunk.
        let mid = reg.pane_rect(PaneId(1), t0 + Duration::from_millis(50)).unwrap();
        assert!(mid.cols > 0 && mid.cols < 100, "got {}", mid.cols);
        assert!(!reg.is_empty());

        // After the duration: prune reports completion and empties the registry.
        let done = reg.prune(t0 + Duration::from_millis(101));
        assert_eq!(done.len(), 1);
        assert!(reg.is_empty());
        assert!(reg.pane_rect(PaneId(1), t0 + Duration::from_millis(101)).is_none());
    }
}
```

Add `mod animation;` to `crates/server/src/lib.rs`.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p vtx-server animation::`
Expected: FAIL to compile until the module is added; once compiling, the test should pass (the code is in the same file). To honor red-green, temporarily break `prune` (e.g. `>=` → `>` won't break this; instead make `prune` a no-op returning `vec![]`) and confirm the test FAILS, then restore.

- [ ] **Step 3: (Implementation written above.)**

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p vtx-server animation::`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/server/src/animation.rs crates/server/src/lib.rs
git commit -m "feat: AnimationRegistry with interpolation and completion pruning"
```

---

## Task 5: Hold the registry on `ServerState` and advance it on the tick

**Files:**
- Modify: `crates/server/src/server.rs`

- [ ] **Step 1: Add the field**

Add `use crate::animation::AnimationRegistry;` (and any needed `Instant`/`Duration` imports). Add to `struct ServerState`:

```rust
    /// In-flight animations, advanced by the drain tick.
    animations: AnimationRegistry,
```

and initialize it in `ServerState::new` with `animations: AnimationRegistry::default(),`.

- [ ] **Step 2: Advance animations + mark dirty in the drain tick**

In the drain tick in `VtxServer::run`, change the body so it bumps the frame counter when there is drained output OR an active animation, and prunes completed animations. Replace the tick body with:

```rust
                let mut st = drain_state.lock().await;
                let drained = drain_all_sessions(&mut st);
                let now = std::time::Instant::now();
                let _done = st.animations.prune(now);
                let animating = !st.animations.is_empty();
                if drained || animating {
                    st.frame_tx.send_modify(|v| *v = v.wrapping_add(1));
                }
```

(`_done` is the list of completed animations; Task 6 uses it to remove the closed pane. For now it is unused — that is fine.)

- [ ] **Step 3: Build & test**

Run: `cargo build -p vtx-server && cargo test -p vtx-server`
Expected: clean build (an unused-`_done` is underscore-suppressed), all tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/server/src/server.rs
git commit -m "feat: advance animations on the frame clock and render while animating"
```

---

## Task 6: Animate pane close (first visible scope)

**Files:**
- Modify: `crates/server/src/server.rs`

This task plumbs the registry into render composition and the KillPane handler. It requires reading the current `build_render_msg_scrolled` (server.rs:1396) and the `KillPane` handler. Implement carefully and verify by manual run.

- [ ] **Step 1: Interpolate a closing pane's rect during render**

`build_render_msg_scrolled` currently builds `PaneRender` from `win.layout.resolve(area)` rects (server.rs:1446-1478). The animation registry lives on `ServerState`, but this function takes `&Session`. Thread the needed animation info in: add a parameter `anim: &AnimationRegistry` to `build_render_msg` / `build_render_msg_scrolled` (and pass `&st.animations` at the two call sites — the `pty_rx`/`frame_rx` render arm and the `handle_message` responses). In the rect-to-`PaneRender` mapping, override the rect when an animation targets the pane:

```rust
            win.panes.get(pid).map(|pane| {
                let now = std::time::Instant::now();
                let r = anim.pane_rect(*pid, now).unwrap_or(*rect);
                // ... build PaneRender using r.x / r.y / r.cols / r.rows ...
            })
```

Note: a closing pane is *removed from the layout* in Step 2 BEFORE the animation finishes, so `resolve` won't return it. To keep drawing it while it shrinks, also iterate the registry's active PaneClose animations and push a `PaneRender` (floating-style, drawn on top) for any animating pane not in `rects`, using `anim.pane_rect`. Keep its last-known content (see Step 2). If retaining content is complex for a first cut, render the shrinking pane as an empty rect — the geometry motion is the visible effect; note the simplification in the commit.

- [ ] **Step 2: Register the animation on KillPane instead of removing immediately**

Find the `KillPane` handling (search for `ClientMsg::KillPane` / where `win.layout.remove(` is called). Today it removes the pane from the layout and `panes` map immediately. Change it to: compute the pane's current rect via `win.layout.resolve(area)`, register a `PaneClose` animation (`from = current rect`, `to = Rect { x, y, cols: 0, rows: 0 }` centered, `duration = config.animations.duration_ms`, `easing = config.animations.easing`) on `st.animations` (guard: only if `config.animations.enabled`, else remove immediately as today), then remove the pane from the layout so the surviving panes start reflowing. Keep the `Pane` itself in `win.panes` until the animation completes so its content can still be drawn (or accept the empty-rect simplification from Step 1). When `prune` (Task 5) reports a completed `PaneClose`, drop the `Pane` from `win.panes` (this triggers the `Drop`/child-reap from the audit C1 fix).

- [ ] **Step 3: Build, test, and manually verify**

Run: `cargo build --workspace && cargo test`
Expected: clean, all tests pass.
Manual (real terminal, ideally Linux `srv`): split a window into 2+ panes, kill one, and confirm it shrinks smoothly over ~150ms while the others reflow, rather than snapping. With `animations.enabled = false` in config, confirm it snaps instantly (no animation).

- [ ] **Step 4: Commit**

```bash
git add crates/server/src/server.rs
git commit -m "feat: animate pane close (shrink) via the animation registry"
```

---

## Self-Review

- **Spec coverage (animation section):** server-side registry ✅ (Task 4), advanced by frame clock + renders while animating ✅ (Task 5), easing config + reduce-motion via `enabled=false` ✅ (Tasks 3, 6), TTY-appropriate geometry interpolation ✅ (Tasks 2, 6). The four scopes: pane-close ✅ (Task 6); split / window-switch / popup-fade / accents are explicitly deferred (below) — each reuses Tasks 1-5 and adds one `AnimationKind` + one wiring task.
- **Placeholder scan:** Task 3 Step 1 and Task 6 require reading existing code to match patterns (config parsing; KillPane handler) — these are discovery steps with explicit pointers, not placeholders. Task 6 offers a documented simplification (empty-rect vs retained content) so the engineer is never blocked. All pure-logic tasks (1, 2, 4) have complete code.
- **Type consistency:** `Easing` (core::animation) used in `ease`, `AnimationConfig`, and `Animation`. `Rect::lerp(from, to, t)` signature consistent in Task 2 and Task 4. `AnimationRegistry` methods (`push`, `prune`, `is_empty`, `pane_rect`) defined in Task 4 and used identically in Tasks 5-6. `PaneClose { pane, from, to }` consistent.

## Follow-on tasks (after this vertical slice works)

Each adds one `AnimationKind` variant + one registration site + one render-interpolation site, reusing Tasks 1-5:
- **Pane split** — animate the new split growing from zero; register on `Split`.
- **Window switch** — slide/offset the new window's panes; register on `SelectWindow`.
- **Popup fade** — ramp popup fg/bg dim→full; register on popup open (needs a color-dim helper in the renderer, not just geometry).
- **Composer/status accents** — brief color pulses; wire once the composer plan lands.
