#!/bin/sh
# Installs a release build of Ritmo with its desktop entry, icon, file type, AppStream data and
# demo projects. Used by the AppImage build and the AUR package.
#
#   cargo build --release
#   packaging/install.sh [DESTDIR] [PREFIX]     # PREFIX defaults to /usr/local
#
# RITMO_BIN overrides the binary to install (default: target/release/ritmo).
set -eu
dest=${1:-}
prefix=${2:-/usr/local}
here=$(cd "$(dirname "$0")" && pwd)
src=$(dirname "$here")
bin=${RITMO_BIN:-$src/target/release/ritmo}
root=$dest$prefix
id=io.github.jerichosiahaya.Ritmo

install -Dm755 "$bin" "$root/bin/ritmo"
install -Dm644 "$here/$id.desktop" "$root/share/applications/$id.desktop"
install -Dm644 "$here/$id.metainfo.xml" "$root/share/metainfo/$id.metainfo.xml"
install -Dm644 "$here/$id.svg" "$root/share/icons/hicolor/scalable/apps/$id.svg"
install -Dm644 "$here/$id.png" "$root/share/icons/hicolor/256x256/apps/$id.png"
install -Dm644 "$here/$id.xml" "$root/share/mime/packages/$id.xml"
install -Dm644 "$src/LICENSE" "$root/share/licenses/ritmo/LICENSE"
for f in "$src"/demos/*.ritmo "$src"/demos/samples/*; do
    install -Dm644 "$f" "$root/share/ritmo/demos/${f#"$src/demos/"}"
done
