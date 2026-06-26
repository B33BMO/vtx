# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

vtx is a terminal multiplexer (tmux alternative) written in Rust (edition 2024, Rust 1.85+). It is **Linux-only**: it reads `/proc` for system stats and manages PTYs via `nix`. The README is comprehensive — read it for CLI reference, keybindings, config, and plugin authoring. This file covers what isn't obvious from the README.

## Commands

```bash
cargo build --release                 # build (binary at target/release/vtx)
cargo build --release --features gpu  # include the wgpu/winit GPU renderer
cargo test                            # run the workspace test suite
cargo test -p vtx-terminal            # test a single crate
cargo test --features gpu             # gpu code is excluded unless the feature is on
cargo test <name>                     # run tests matching a substring
cargo clippy --all-targets            # lint
./install.sh                          # build + install to ~/.local/bin (ENABLE_GPU=1, INSTALL_DIR=... to override)
```

There is no CI config and no separate lint/format script — use the cargo commands directly.

## Architecture (the big picture)

Client-server over a Unix domain socket at `$XDG_RUNTIME_DIR/vtx.sock`. The `vtx` binary is *both* client and server: most subcommands connect as a client, auto-spawning the server daemon if it isn't running. A stale socket from a crashed server blocks startup — `rm $XDG_RUNTIME_DIR/vtx.sock` clears it.

Crate dependency flow (all depend on `vtx-core`):

```
vtx (cli) ── client ── renderer-tty / renderer-gpu(opt)
           └ server ── terminal, layout, plugin
                 core  (shared types, IPC protocol, config)
```

- **vtx-core** — shared `PaneId`/`SessionId` types, the IPC message enums (`ipc.rs`), and config parsing. Both Lua config (`lua_config.rs`) and tmux.conf import (`tmux_compat.rs`) live here. Changing the wire protocol means editing `ipc.rs` and both the client and server handlers.
- **vtx-server** (`server.rs`, 1700 lines) — owns the async event loop, session > window > pane hierarchy, PTY spawning, status-bar template resolution (`status.rs`), and plugin hook dispatch. This is where session state lives.
- **vtx-client** (`client.rs`, 1500 lines) — captures terminal input, runs the prefix-key state machine and keybinding dispatch, talks to the server. `gpu_attach.rs` is the parallel winit event loop used in `--gpu` mode.
- **vtx-terminal** — the VT100/xterm parser (`parser.rs`, built on `vte`) and the `grid.rs` cell grid with 100k-line scrollback and alternate-screen buffer. Pure data structures; no I/O.
- **vtx-layout** — binary split tree (leaves = panes, internal nodes = direction + ratio). Resolves trees to screen rects and builds the preset layouts.
- **vtx-plugin** — plugin manager hosting both a Lua runtime (mlua, Lua 5.4) and a WASM runtime (wasmtime). Dispatches lifecycle hooks and collects the actions plugins request.

### Things that will bite you if you don't know them

- **Cancel-safe IPC**: never call `read_line` inside a tokio `select!` — cancellation mid-read corrupts the buffer. Both client and server use a dedicated reader task that owns the socket read half and forwards complete newline-delimited JSON messages through a channel; `select!` only ever receives from channels. Preserve this pattern when touching IPC.
- **PTY reads use blocking std::threads, not async**: each pane spawns a dedicated `std::thread` reading in a tight loop into an `mpsc` channel; the async loop drains it with `try_recv()`. Don't try to async-wrap PTY reads.
- **Differential rendering**: `renderer-tty` keeps front/back cell buffers and only emits escape sequences for cells that changed. The initial front buffer is filled with sentinel (impossible) cells to force a full first draw. If you add cell fields, make sure the diff and the sentinel both account for them.
- **GPU code is feature-gated**: `renderer-gpu`, the `winit` dep, and the `--gpu` paths only compile under `--features gpu`. Default builds and `cargo test` skip them entirely.

### Config & plugins

User config is Lua at `~/.config/vtx/config.lua`; plugins (Lua + WASM) auto-load from `~/.config/vtx/plugins/`. Both are hot-reloadable via `vtx source` (or prefix+R) — there's a `SourceConfig` IPC round-trip rather than a process restart. `examples/` holds the reference config, eight themes, and sample plugins; use these as the source of truth for the config schema.
