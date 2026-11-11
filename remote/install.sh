#!/bin/sh
# Builds Veyron Remote and puts it in the user's own folders (no root): the program in ~/.local/bin, and a
# launcher entry with its icon so the desktop lists it. Linux; needs Rust, and SDL3 and FFmpeg with their
# development files (or run with FEATURES=bundled-sdl to build SDL3 in).
set -e
cd "$(dirname "$0")"
# A copy that is already built (the one GitHub Actions packs next to this script) is installed as it is.
if [ -x veyron-remote ]; then
    BUILT=veyron-remote
else
    cargo build --release ${FEATURES:+--features "$FEATURES"}
    BUILT=target/release/veyron-remote
fi
install -Dm755 "$BUILT" "$HOME/.local/bin/veyron-remote"
install -Dm644 assets/veyron-remote.svg "$HOME/.local/share/icons/hicolor/scalable/apps/veyron-remote.svg"
mkdir -p "$HOME/.local/share/applications"
cat > "$HOME/.local/share/applications/veyron-remote.desktop" <<END
[Desktop Entry]
Type=Application
Name=Veyron Remote
Comment=Show and control the Mac that Veyron runs on an iPhone
Exec=$HOME/.local/bin/veyron-remote
Icon=veyron-remote
Categories=Network;RemoteAccess;
StartupWMClass=veyron-remote
END
echo "Installed: $HOME/.local/bin/veyron-remote"
