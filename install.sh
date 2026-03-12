#!/usr/bin/env bash
set -euo pipefail

INSTALL_DIR="${INSTALL_DIR:-$HOME/.local/bin}"
ENABLE_GPU="${ENABLE_GPU:-0}"

echo "=== vtx installer ==="
echo ""

# Check for Rust toolchain
if ! command -v cargo &>/dev/null; then
    echo "Error: cargo not found. Install Rust from https://rustup.rs"
    exit 1
fi

# Check Rust version (need 1.85+ for edition 2024)
RUST_VERSION=$(rustc --version | grep -oP '\d+\.\d+')
RUST_MAJOR=$(echo "$RUST_VERSION" | cut -d. -f1)
RUST_MINOR=$(echo "$RUST_VERSION" | cut -d. -f2)
if [ "$RUST_MAJOR" -lt 1 ] || { [ "$RUST_MAJOR" -eq 1 ] && [ "$RUST_MINOR" -lt 85 ]; }; then
    echo "Error: Rust 1.85+ required (found $RUST_VERSION). Run: rustup update"
    exit 1
fi

echo "Building vtx (release mode)..."
echo ""

if [ "$ENABLE_GPU" = "1" ]; then
    echo "  GPU renderer: enabled"
    cargo build --release --features gpu
else
    echo "  GPU renderer: disabled (set ENABLE_GPU=1 to enable)"
    cargo build --release
fi

echo ""

# Create install directory
mkdir -p "$INSTALL_DIR"

# Copy binary
cp target/release/vtx "$INSTALL_DIR/vtx"
chmod +x "$INSTALL_DIR/vtx"

echo "Installed vtx to $INSTALL_DIR/vtx"

# Check if install dir is in PATH
if ! echo "$PATH" | tr ':' '\n' | grep -qx "$INSTALL_DIR"; then
    echo ""
    echo "Warning: $INSTALL_DIR is not in your PATH."
    echo "Add it with:"
    echo ""
    echo "  echo 'export PATH=\"$INSTALL_DIR:\$PATH\"' >> ~/.bashrc"
    echo ""
fi

# Create config directory
CONFIG_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/vtx"
mkdir -p "$CONFIG_DIR/plugins"

# Write example config if none exists
if [ ! -f "$CONFIG_DIR/config.lua" ]; then
    cat > "$CONFIG_DIR/config.lua" << 'LUAEOF'
-- vtx configuration
-- See: https://github.com/bmo/vtx#configuration
--
-- Everything below is ALREADY the default — uncomment and change to customize.
-- Delete this file entirely to use pure defaults (Tokyo Night powerline).
-- Or copy a theme: cp /path/to/vtx/examples/dracula.lua ~/.config/vtx/config.lua

-- vtx.prefix = "ctrl-a"
-- vtx.shell = "/bin/zsh"
-- vtx.scrollback = 50000

-- ── Status Bar ─────────────────────────────────────────────
-- Each segment: { text, fg, bg, bold }
-- Variables: #{session} #{windows} #{git} #{cpu} #{mem} #{time} #{pane} #{cwd}
--
-- vtx.status_left = {
--     { text = " ▶ #{session} ",  fg = "#1a1b26", bg = "#7aa2f7", bold = true },
--     { text = " #{windows} ",    fg = "#c0caf5", bg = "#414868" },
--     { text = " #{git} ",        fg = "#1a1b26", bg = "#9ece6a" },
-- }
-- vtx.status_right = {
--     { text = " #{cpu} ",   fg = "#c0caf5", bg = "#414868" },
--     { text = " #{mem} ",   fg = "#c0caf5", bg = "#3b4261" },
--     { text = " #{time} ",  fg = "#1a1b26", bg = "#7aa2f7", bold = true },
-- }
-- vtx.status_bg = "#1a1b26"

-- ── Keybindings ────────────────────────────────────────────
-- vtx.bind("prefix", "|", "split-horizontal")
-- vtx.bind("prefix", "-", "split-vertical")
-- vtx.bind("alt", "h", "focus-left")
-- vtx.bind("alt", "j", "focus-down")
-- vtx.bind("alt", "k", "focus-up")
-- vtx.bind("alt", "l", "focus-right")
LUAEOF
    echo "Created example config at $CONFIG_DIR/config.lua"
fi

echo ""
echo "Done! Run 'vtx' to start."
