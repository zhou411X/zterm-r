#!/bin/sh
# Install the Rust preview alongside the existing zterm command.
set -eu
project_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
binary="$project_dir/target/release/zterm-r"
if [ ! -x "$binary" ]; then
    echo 'Build first: cargo build --locked --release -j32' >&2
    exit 1
fi
data_dir="${XDG_DATA_HOME:-$HOME/.local/share}"
bin_dir="$HOME/.local/bin"
install -d "$bin_dir" "$data_dir/applications" "$data_dir/icons/hicolor/scalable/apps"
install -m 755 "$binary" "$bin_dir/zterm-r"
install -m 644 "$project_dir/workbench.svg" "$data_dir/icons/hicolor/scalable/apps/local.zterm.r.svg"
cat > "$data_dir/applications/local.zterm.r.desktop" <<DESKTOP
[Desktop Entry]
Type=Application
Name=zterm-r
Comment=Terminal and SSH workspace
Exec="$bin_dir/zterm-r"
Icon=local.zterm.r
Terminal=false
Categories=System;TerminalEmulator;
StartupNotify=true
DESKTOP
printf 'Installed: %s\n' "$bin_dir/zterm-r"
