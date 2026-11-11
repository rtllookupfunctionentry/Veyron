#!/bin/sh
# Builds Henge Remote and puts it in the user's own folders (no root): the program in ~/.local/bin, and a
# launcher entry with its icon so the desktop lists it. Linux; needs Rust, and SDL3 and FFmpeg with their
# development files (or run with FEATURES=bundled-sdl to build SDL3 in).
set -e
cd "$(dirname "$0")"
cargo build --release ${FEATURES:+--features "$FEATURES"}
install -Dm755 target/release/henge-remote "$HOME/.local/bin/henge-remote"
install -Dm644 assets/henge-remote.svg "$HOME/.local/share/icons/hicolor/scalable/apps/henge-remote.svg"
mkdir -p "$HOME/.local/share/applications"
cat > "$HOME/.local/share/applications/henge-remote.desktop" <<END
[Desktop Entry]
Type=Application
Name=Henge Remote
Comment=Show and control the Mac that Henge runs on an iPhone
Exec=$HOME/.local/bin/henge-remote
Icon=henge-remote
Categories=Network;RemoteAccess;
StartupWMClass=henge-remote
END
echo "Installed: $HOME/.local/bin/henge-remote"
