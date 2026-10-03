#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

# These packages contain a native, dynamically linked Linux binary. Build on
# Debian/Ubuntu so dpkg-shlibdeps can resolve the actual ELF dependencies.
for tool in cargo rustc python3 strip tar dpkg-deb dpkg-shlibdeps cargo-generate-rpm rpm sha256sum desktop-file-validate; do
    command -v "$tool" >/dev/null 2>&1 || { echo "Required packaging tool missing: $tool" >&2; exit 1; }
done
[[ -x /usr/lib/rpm/find-requires ]] || { echo "Install RPM's find-requires helper." >&2; exit 1; }

VERSION="$(python3 -c 'import tomllib; print(tomllib.load(open("Cargo.toml", "rb"))["package"]["version"])')"
[[ "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || { echo "Expected a stable Cargo package version, got: $VERSION" >&2; exit 1; }
if [[ -n "${RELEASE_TAG:-}" && "$RELEASE_TAG" != "v$VERSION" ]]; then
    echo "Release tag $RELEASE_TAG does not match Cargo.toml version v$VERSION." >&2
    exit 1
fi

# Validate source assets before spending time on the release build. Cargo's RPM
# metadata is the source of truth for CI; the standalone spec is not used there.
python3 - <<'PY'
import glob
import tomllib

with open("Cargo.toml", "rb") as source:
    assets = tomllib.load(source)["package"]["metadata"]["generate-rpm"]["assets"]
for asset in assets:
    path = asset["source"]
    if not path.startswith("target/") and not glob.glob(path):
        raise SystemExit(f"Missing RPM source asset: {path}")
PY
desktop-file-validate packaging/rpm/localsend-gtk.desktop

HOST_TARGET="$(rustc -vV | awk '/^host:/ { print $2 }')"
case "$HOST_TARGET" in
    x86_64-unknown-linux-gnu) ARCH="amd64"; RPM_ARCH="x86_64" ;;
    aarch64-unknown-linux-gnu) ARCH="arm64"; RPM_ARCH="aarch64" ;;
    *) echo "Unsupported native release target: $HOST_TARGET" >&2; exit 1 ;;
esac

# Always ask Cargo to validate/build the current sources. An existing binary
# alone does not establish that it belongs to the version being packaged.
cargo build --release --locked --target "$HOST_TARGET" --target-dir "$ROOT/target"
BIN_SRC="$ROOT/target/$HOST_TARGET/release/localsend-gtk"
strip -s "$BIN_SRC"

DIST_DIR="$ROOT/target/dist"
mkdir -p "$DIST_DIR"
# A Linux temporary directory preserves package permissions even when the
# checkout resides on a Windows/WSL mount without Unix permission metadata.
STAGING="$(mktemp -d /tmp/localsend-gtk-package.XXXXXXXX)"
trap 'rm -rf -- "$STAGING"' EXIT

copy_notices() {
    local dest="$1"
    mkdir -p "$dest/LICENSES" "$dest/localsend-rs"
    install -m 644 README.md LICENSE THIRD_PARTY_NOTICES.md "$dest/"
    install -m 644 LICENSES/*.txt "$dest/LICENSES/"
    install -m 644 vendor/localsend-rs/UPSTREAM.md vendor/localsend-rs/THIRD_PARTY_NOTICES.md "$dest/localsend-rs/"
}

TAR_NAME="localsend-gtk-v${VERSION}-linux-${RPM_ARCH}"
TAR_STAGING="$STAGING/$TAR_NAME"
mkdir -p "$TAR_STAGING/bin" "$TAR_STAGING/share/applications" "$TAR_STAGING/share/icons/hicolor/512x512/apps"
install -m 755 "$BIN_SRC" "$TAR_STAGING/bin/localsend-gtk"
install -m 644 packaging/rpm/localsend-gtk.desktop "$TAR_STAGING/share/applications/org.localsend.localsend_gtk.desktop"
install -D -m 644 packaging/desktop/localsend-dolphin.desktop "$TAR_STAGING/share/kio/servicemenus/localsend-dolphin.desktop"
install -D -m 755 "packaging/desktop/nautilus-scripts/Send with LocalSend" "$TAR_STAGING/share/nautilus-scripts/Send with LocalSend"
install -m 644 assets/logo.png "$TAR_STAGING/share/icons/hicolor/512x512/apps/localsend-gtk.png"
copy_notices "$TAR_STAGING/share/doc/localsend-gtk"

install -m 755 packaging/install.sh "$TAR_STAGING/install.sh"

DEB_STAGING="$STAGING/deb"
mkdir -p "$DEB_STAGING/DEBIAN" "$DEB_STAGING/usr" "$STAGING/deb-metadata/debian"
cp -R "$TAR_STAGING/bin" "$TAR_STAGING/share" "$DEB_STAGING/usr/"

# dpkg-shlibdeps needs source package metadata even for this binary-only build.
cat <<'EOF' > "$STAGING/deb-metadata/debian/control"
Source: localsend-gtk
Section: net
Priority: optional
Maintainer: SamSeven777 <93786755+SamSeven777@users.noreply.github.com>

Package: localsend-gtk
Architecture: any
Description: Native GTK4/libadwaita client for LocalSend in Rust
EOF
SHLIB_DEPENDS="$(cd "$STAGING/deb-metadata" && dpkg-shlibdeps -O -e"$BIN_SRC" | sed -n 's/^shlibs:Depends=//p')"
[[ -n "$SHLIB_DEPENDS" ]] || { echo "No native runtime dependencies were detected." >&2; exit 1; }
cat <<EOF > "$DEB_STAGING/DEBIAN/control"
Package: localsend-gtk
Version: ${VERSION}-1
Section: net
Priority: optional
Architecture: ${ARCH}
Depends: libgtk-4-1 (>= 4.12), libadwaita-1-0 (>= 1.5), ${SHLIB_DEPENDS}
Maintainer: SamSeven777 <93786755+SamSeven777@users.noreply.github.com>
Homepage: https://github.com/SamSeven777/localsend-gtk
Description: Native GTK4/libadwaita client for LocalSend in Rust
 LocalSend GTK is an independent community client for the LocalSend protocol v2,
 written in Rust using GTK4 and libadwaita, with native Wayland support.
EOF

TAR_FILE="$TAR_NAME.tar.gz"
DEB_FILE="localsend-gtk_${VERSION}-1_${ARCH}.deb"
RPM_FILE="localsend-gtk-${VERSION}-1.${RPM_ARCH}.rpm"
tar --owner=0 --group=0 -czf "$STAGING/$TAR_FILE" -C "$STAGING" "$TAR_NAME"
dpkg-deb --root-owner-group --build "$DEB_STAGING" "$STAGING/$DEB_FILE"
cargo-generate-rpm --target "$HOST_TARGET" --target-dir "$ROOT/target" \
    --auto-req find-requires --output "$STAGING/$RPM_FILE"

[[ "$(dpkg-deb -f "$STAGING/$DEB_FILE" Version)" == "$VERSION-1" ]]
[[ "$(dpkg-deb -f "$STAGING/$DEB_FILE" Architecture)" == "$ARCH" ]]
[[ "$(rpm -qp --queryformat '%{VERSION}-%{RELEASE}.%{ARCH}' "$STAGING/$RPM_FILE")" == "$VERSION-1.$RPM_ARCH" ]]

# Publish outputs only after all three formats have been generated successfully.
install -m 644 "$STAGING/$TAR_FILE" "$STAGING/$DEB_FILE" "$STAGING/$RPM_FILE" "$DIST_DIR/"
(cd "$DIST_DIR" && sha256sum "$TAR_FILE" "$DEB_FILE" "$RPM_FILE" > SHA256SUMS && sha256sum --check SHA256SUMS)
echo "Release $VERSION packages and SHA256SUMS are ready in $DIST_DIR"
