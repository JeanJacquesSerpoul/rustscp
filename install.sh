#!/bin/sh
# Installation locale de RustSCP : compilation, lanceur et entrée de menu.
# Prérequis : Rust (stable), cible wasm32-unknown-unknown, trunk, tauri-cli,
# bibliothèques WebKitGTK 4.1 (paquets libwebkit2gtk-4.1-dev, libssl-dev…).
set -e
DIR="$(cd "$(dirname "$0")" && pwd)"
cd "$DIR"
command -v trunk >/dev/null || { echo "trunk manquant : cargo install trunk"; exit 1; }
cargo tauri --version >/dev/null 2>&1 || { echo "tauri-cli manquant : cargo install tauri-cli --version '^2'"; exit 1; }
rustup target list --installed 2>/dev/null | grep -q wasm32-unknown-unknown || rustup target add wasm32-unknown-unknown
cargo tauri build --no-bundle
mkdir -p "$HOME/.local/bin" "$HOME/.local/share/applications" "$HOME/.local/share/icons/hicolor/scalable/apps"
install -m 755 target/release/rustscp "$HOME/.local/bin/rustscp"
cp src-tauri/icons/rustscp.svg "$HOME/.local/share/icons/hicolor/scalable/apps/rustscp.svg"
cat > "$HOME/.local/share/applications/rustscp.desktop" <<DESKTOP
[Desktop Entry]
Type=Application
Name=RustSCP
GenericName=Client SCP/SFTP
Comment=Transferts SCP/SFTP avec clé SSH + code TOTP
Exec=$HOME/.local/bin/rustscp
Icon=rustscp
Terminal=false
Categories=Network;FileTransfer;
Keywords=scp;sftp;ssh;ftp;filezilla;totp;2fa;
StartupWMClass=rustscp
DESKTOP
update-desktop-database "$HOME/.local/share/applications" 2>/dev/null || true
echo "RustSCP installé. Lancez « rustscp » ou cherchez RustSCP dans le menu des applications."
