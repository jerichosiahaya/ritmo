#!/bin/sh
# Builds Ritmo-<version>-<arch>.AppImage in the repository root.
#
# The AppImage runs on distributions with the same or a newer glibc than the one it was built
# on, so release builds are made on the oldest supported Ubuntu (see .github/workflows). ALSA,
# OpenGL and the windowing libraries come from the system, as on any desktop.
#
# Uses appimagetool from $APPIMAGETOOL or PATH, else downloads it into target/.
set -eu
cd "$(dirname "$0")/.."
version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n1)
arch=$(uname -m)
id=io.github.jerichosiahaya.Ritmo

cargo build --release --locked
export RITMO_BIN="${CARGO_TARGET_DIR:-target}/release/ritmo"
app=target/AppDir
rm -rf "$app"
packaging/install.sh "$app" /usr
cp "packaging/$id.desktop" "packaging/$id.png" "$app/"
ln -s usr/bin/ritmo "$app/AppRun"

tool=${APPIMAGETOOL:-$(command -v appimagetool || true)}
if [ -z "$tool" ]; then
    tool=target/appimagetool-$arch.AppImage
    if [ ! -x "$tool" ]; then
        curl -fL --retry 3 -o "$tool" "https://github.com/AppImage/appimagetool/releases/download/continuous/appimagetool-$arch.AppImage"
        chmod +x "$tool"
    fi
fi
# The runtime that starts the AppImage (fetched here so proxies and CA settings apply).
runtime=${APPIMAGE_RUNTIME:-target/runtime-$arch}
if [ ! -f "$runtime" ]; then
    curl -fL --retry 3 -o "$runtime" "https://github.com/AppImage/type2-runtime/releases/download/continuous/runtime-$arch"
fi
# appimagetool is an AppImage itself: extract-and-run works without FUSE (containers, CI).
APPIMAGE_EXTRACT_AND_RUN=1 ARCH=$arch "$tool" -n --runtime-file "$runtime" "$app" "Ritmo-$version-$arch.AppImage"
