#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

for tool in cargo rustc python3 install strip desktop-file-validate mksquashfs; do
    command -v "$tool" >/dev/null 2>&1 || {
        echo "Required AppImage packaging tool missing: $tool" >&2
        exit 1
    }
done

APPIMAGETOOL="${APPIMAGETOOL:-$(command -v appimagetool || true)}"
if [[ -z "$APPIMAGETOOL" ]]; then
    echo "Set APPIMAGETOOL to appimagetool or appimagetool-x86_64.AppImage." >&2
    exit 1
fi
if [[ ! -x "$APPIMAGETOOL" ]]; then
    echo "APPIMAGETOOL is not executable: $APPIMAGETOOL" >&2
    exit 1
fi

VERSION="$(python3 -c 'import tomllib; print(tomllib.load(open("Cargo.toml", "rb"))["package"]["version"])')"
[[ "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || {
    echo "Expected a stable Cargo package version, got: $VERSION" >&2
    exit 1
}
HOST_TARGET="$(rustc -vV | awk '/^host:/ { print $2 }')"
case "$HOST_TARGET" in
    x86_64-unknown-linux-gnu) HOST_ARCH="x86_64" ;;
    aarch64-unknown-linux-gnu) HOST_ARCH="aarch64" ;;
    *) echo "Unsupported native AppImage target: $HOST_TARGET" >&2; exit 1 ;;
esac
ARCH="${ARCH:-$HOST_ARCH}"
if [[ "$ARCH" != "$HOST_ARCH" ]]; then
    echo "ARCH=$ARCH does not match native build target $HOST_TARGET; cross-compilation is not supported." >&2
    exit 1
fi

# Use the repository that actually triggered the build. This makes fork and
# upstream releases self-contained: no owner or repository is hard-coded.
APPIMAGE_UPDATE_INFO="${APPIMAGE_UPDATE_INFO:-}"
if [[ -z "$APPIMAGE_UPDATE_INFO" && -n "${GITHUB_REPOSITORY:-}" ]]; then
    OWNER="${GITHUB_REPOSITORY%%/*}"
    REPO="${GITHUB_REPOSITORY#*/}"
    APPIMAGE_UPDATE_INFO="gh-releases-zsync|${OWNER}|${REPO}|latest|localsend-gtk-*-linux-${ARCH}.AppImage.zsync"
fi
if [[ -n "$APPIMAGE_UPDATE_INFO" ]]; then
    command -v zsyncmake >/dev/null 2>&1 || {
        echo "APPIMAGE_UPDATE_INFO is set but zsyncmake is missing." >&2
        exit 1
    }
fi

cargo build --release --locked --target "$HOST_TARGET" --target-dir "$ROOT/target"
BIN="$ROOT/target/$HOST_TARGET/release/localsend-gtk"
[[ -x "$BIN" ]] || { echo "Release binary is missing: $BIN" >&2; exit 1; }
strip -s "$BIN"

APPIMAGE_ROOT="$ROOT/target/appimage"
APPDIR="$APPIMAGE_ROOT/AppDir"
rm -rf "$APPIMAGE_ROOT"
mkdir -p "$APPDIR/usr/bin" \
    "$APPDIR/usr/share/applications" \
    "$APPDIR/usr/share/icons/hicolor/512x512/apps" \
    "$APPDIR/usr/share/metainfo" \
    "$APPDIR/usr/share/doc/localsend-gtk/localsend-rs" \
    "$APPDIR/usr/share/licenses/localsend-gtk"

install -m 755 "$BIN" "$APPDIR/usr/bin/localsend-gtk"
install -m 755 packaging/appimage/AppRun "$APPDIR/AppRun"
# AppImage tools and desktop environments resolve the embedded icon by the
# desktop file's Icon key. Keep the native alias too for the file manager.
sed 's/^Icon=.*/Icon=org.localsend.localsend_gtk/' \
    packaging/rpm/localsend-gtk.desktop > "$APPDIR/org.localsend.localsend_gtk.desktop"
install -m 644 "$APPDIR/org.localsend.localsend_gtk.desktop" \
    "$APPDIR/usr/share/applications/org.localsend.localsend_gtk.desktop"
install -m 644 assets/logo.png "$APPDIR/org.localsend.localsend_gtk.png"
install -m 644 assets/logo.png "$APPDIR/localsend-gtk.png"
ln -sfn org.localsend.localsend_gtk.png "$APPDIR/.DirIcon"
install -m 644 assets/logo.png \
    "$APPDIR/usr/share/icons/hicolor/512x512/apps/org.localsend.localsend_gtk.png"
install -m 644 assets/logo.png \
    "$APPDIR/usr/share/icons/hicolor/512x512/apps/localsend-gtk.png"
install -m 644 packaging/flatpak/org.localsend.localsend_gtk.metainfo.xml \
    "$APPDIR/usr/share/metainfo/org.localsend.localsend_gtk.appdata.xml"
install -m 644 README.md LICENSE THIRD_PARTY_NOTICES.md \
    "$APPDIR/usr/share/doc/localsend-gtk/"
install -m 644 LICENSES/*.txt "$APPDIR/usr/share/licenses/localsend-gtk/"
install -m 644 vendor/localsend-rs/UPSTREAM.md \
    vendor/localsend-rs/THIRD_PARTY_NOTICES.md \
    "$APPDIR/usr/share/doc/localsend-gtk/localsend-rs/"

shopt -s nullglob
ROOT_DESKTOPS=("$APPDIR"/*.desktop)
shopt -u nullglob
if [[ ${#ROOT_DESKTOPS[@]} -ne 1 ]]; then
    echo "The AppDir must contain exactly one root .desktop file; found ${#ROOT_DESKTOPS[@]}." >&2
    exit 1
fi
ROOT_DESKTOP="${ROOT_DESKTOPS[0]}"
ICON_NAME="$(sed -n 's/^Icon=//p' "$ROOT_DESKTOP" | head -n 1)"
[[ -n "$ICON_NAME" ]] || { echo "The root desktop file has no Icon entry." >&2; exit 1; }
[[ -f "$APPDIR/$ICON_NAME.png" || -f "$APPDIR/$ICON_NAME.svg" ]] || {
    echo "The root icon for Icon=$ICON_NAME is missing." >&2
    exit 1
}
[[ -e "$APPDIR/.DirIcon" ]] || { echo "The AppDir is missing .DirIcon." >&2; exit 1; }
[[ -x "$APPDIR/AppRun" ]] || { echo "The AppDir is missing an executable AppRun." >&2; exit 1; }
[[ -f "$APPDIR/usr/share/metainfo/org.localsend.localsend_gtk.appdata.xml" ]] || {
    echo "The AppDir is missing its AppStream metadata." >&2
    exit 1
}
grep -q '^StartupWMClass=org\.localsend\.localsend_gtk$' "$ROOT_DESKTOP" || {
    echo "The root desktop file is missing the StartupWMClass association." >&2
    exit 1
}
grep -q '^X-AppImage-Name=LocalSend GTK$' "$ROOT_DESKTOP" || {
    echo "The root desktop file is missing the AppImage name." >&2
    exit 1
}
grep -q '^Categories=Network;FileTransfer;$' "$ROOT_DESKTOP" || {
    echo "The root desktop file has unexpected categories." >&2
    exit 1
}
desktop-file-validate "$ROOT_DESKTOP"
mkdir -p "$ROOT/target/dist"
OUTPUT="$ROOT/target/dist/localsend-gtk-v${VERSION}-linux-${ARCH}.AppImage"
STRAY_ZSYNC="$ROOT/$(basename "$OUTPUT").zsync"
rm -f "$OUTPUT" "$OUTPUT.zsync" "$STRAY_ZSYNC"

TOOL_ARGS=()
if [[ "${APPIMAGETOOL_EXTRACT:-0}" == "1" ]]; then
    TOOL_ARGS+=(--appimage-extract-and-run)
fi
if [[ -n "$APPIMAGE_UPDATE_INFO" ]]; then
    TOOL_ARGS+=(-u "$APPIMAGE_UPDATE_INFO")
fi

VERSION="$VERSION" ARCH="$ARCH" \
    "$APPIMAGETOOL" "${TOOL_ARGS[@]}" "$APPDIR" "$OUTPUT"
chmod 755 "$OUTPUT"
if [[ -n "$APPIMAGE_UPDATE_INFO" && ! -f "$OUTPUT.zsync" && -f "$STRAY_ZSYNC" ]]; then
    # appimagetool writes the zsync file in the current working directory.
    mv "$STRAY_ZSYNC" "$OUTPUT.zsync"
fi
if [[ -n "$APPIMAGE_UPDATE_INFO" ]]; then
    grep -aFq "$APPIMAGE_UPDATE_INFO" "$OUTPUT" || {
        echo "The AppImage does not contain the expected update information." >&2
        exit 1
    }
    [[ -f "$OUTPUT.zsync" ]] || {
        echo "appimagetool did not create $OUTPUT.zsync." >&2
        exit 1
    }
    grep -q '^zsync:' "$OUTPUT.zsync" || {
        echo "The generated .zsync file is invalid." >&2
        exit 1
    }
fi
CHECKSUM="$(sha256sum "$OUTPUT" | cut -d' ' -f1)"
{
    printf '%s  %s\n' "$CHECKSUM" "$(basename "$OUTPUT")"
    if [[ -f "$OUTPUT.zsync" ]]; then
        ZSYNC_CHECKSUM="$(sha256sum "$OUTPUT.zsync" | cut -d' ' -f1)"
        printf '%s  %s\n' "$ZSYNC_CHECKSUM" "$(basename "$OUTPUT.zsync")"
    fi
} > "$OUTPUT.sha256"
echo "Created $OUTPUT"
if [[ -f "$OUTPUT.zsync" ]]; then
    echo "Created $OUTPUT.zsync"
    echo "Update information: $APPIMAGE_UPDATE_INFO"
fi
echo "The AppImage uses the system GTK4/libadwaita runtime; see packaging/appimage/README.md."
