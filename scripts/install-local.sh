#!/bin/sh
# Build Margin and install it for the current user (no root): ~/.local/bin, the app launcher, icons.
# Run again to update; `--uninstall` removes it (settings and unsaved backups stay).
# With PREFIX=/usr/local (as root) it installs system-wide instead.
set -eu
cd "$(dirname "$0")/.."
PREFIX="${PREFIX:-$HOME/.local}"
BIN="$PREFIX/bin"
SHARE="${PREFIX}/share"
[ "$PREFIX" = "$HOME/.local" ] && SHARE="${XDG_DATA_HOME:-$HOME/.local/share}"
APPS="$SHARE/applications"
ICONS="$SHARE/icons/hicolor"

if [ "${1:-}" = "--uninstall" ]; then
  rm -f "$BIN/margin" "$APPS/margin.desktop" "$ICONS/256x256/apps/margin.png" "$ICONS/512x512/apps/margin.png" "$ICONS/scalable/apps/margin.svg"
  update-desktop-database "$APPS" 2>/dev/null || true
  echo "Margin removed."
  exit 0
fi

[ "${1:-}" = "--no-build" ] || cargo build --release --locked
mkdir -p "$BIN" "$APPS"
install -m 755 target/release/margin "$BIN/margin.new" && mv -f "$BIN/margin.new" "$BIN/margin"
install -Dm 644 assets/icon-256.png "$ICONS/256x256/apps/margin.png"
install -Dm 644 assets/icon-512.png "$ICONS/512x512/apps/margin.png"
install -Dm 644 assets/icon.svg "$ICONS/scalable/apps/margin.svg"
sed "s#@BIN@#$BIN/margin#g" packaging/margin.desktop.in > "$APPS/margin.desktop"
update-desktop-database "$APPS" 2>/dev/null || true
[ -f "$ICONS/icon-theme.cache" ] && gtk-update-icon-cache -q "$ICONS" 2>/dev/null || true  # refresh, never create, a cache
echo "Margin installed: $BIN/margin, and in the app launcher."
