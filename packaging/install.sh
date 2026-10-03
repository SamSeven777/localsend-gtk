#!/usr/bin/env bash
set -euo pipefail

# Resolve relative prefixes against the caller's directory, not the archive.
PREFIX="$(realpath -m -- "${1:-/usr/local}")"
case "$PREFIX" in
    *'='*|*$'\n'*|*$'\r'*)
        echo "The installation prefix cannot contain '=', a newline or a carriage return." >&2
        exit 1
        ;;
esac
cd -- "$(dirname -- "${BASH_SOURCE[0]}")"

desktop_string() {
    local value="$1"
    value="${value//\\/\\\\}"
    value="${value//$'\t'/\\t}"
    printf '%s' "$value"
}

desktop_exec() {
    local value="$1"
    # Like the autostart entry, defer paths containing '%' until field codes are
    # expanded: GIO otherwise checks for a literal '%%' executable first.
    if [[ "$value" == *%* ]]; then
        printf '/usr/bin/env -- '
    fi
    value="${value//\\/\\\\}"
    value="${value//\"/\\\"}"
    value="${value//\`/\\\`}"
    value="${value//\$/\\\$}"
    value="${value//%/%%}"
    printf '"%s"' "$(desktop_string "$value")"
}

install_desktop() {
    local source="$1" destination="$2" mode="$3" line
    install -D -m "$mode" "$source" "$destination"
    # A custom prefix need not be on the desktop session's PATH or icon path.
    while IFS= read -r line || [[ -n "$line" ]]; do
        case "$line" in
            Exec=*) printf 'Exec=%s %%U\n' "$(desktop_exec "$PREFIX/bin/localsend-gtk")" ;;
            Icon=*) printf 'Icon=%s\n' "$(desktop_string "$PREFIX/share/icons/hicolor/512x512/apps/localsend-gtk.png")" ;;
            *) printf '%s\n' "$line" ;;
        esac
    done < "$source" > "$destination"
}

install_nautilus_script() {
    local destination="$1" line
    install -D -m 755 "share/nautilus-scripts/Send with LocalSend" "$destination"
    # Keep this installation independent of PATH, including custom prefixes.
    # Preserve the common URI decoding logic; %q protects the executable path.
    while IFS= read -r line || [[ -n "$line" ]]; do
        case "$line" in
            localsend_gtk_binary=*) printf 'localsend_gtk_binary=%q\n' "$PREFIX/bin/localsend-gtk" ;;
            *) printf '%s\n' "$line" ;;
        esac
    done < "share/nautilus-scripts/Send with LocalSend" > "$destination"
}

echo "Installing LocalSend GTK to $PREFIX..."
install -D -m 755 bin/localsend-gtk "$PREFIX/bin/localsend-gtk"
install_desktop share/applications/org.localsend.localsend_gtk.desktop \
    "$PREFIX/share/applications/org.localsend.localsend_gtk.desktop" 644
# Dolphin requires user-installed service menus to be executable to trust them.
install_desktop share/kio/servicemenus/localsend-dolphin.desktop \
    "$PREFIX/share/kio/servicemenus/localsend-dolphin.desktop" 755
if [[ -f "share/nautilus-scripts/Send with LocalSend" ]]; then
    install_nautilus_script "$PREFIX/share/nautilus-scripts/Send with LocalSend"
    if [[ "$PREFIX" == "$HOME/.local" || "$PREFIX" == "$HOME/.local/"* ]]; then
        install_nautilus_script \
            "${XDG_DATA_HOME:-$HOME/.local/share}/nautilus/scripts/Send with LocalSend"
    fi
fi
install -D -m 644 share/icons/hicolor/512x512/apps/localsend-gtk.png "$PREFIX/share/icons/hicolor/512x512/apps/localsend-gtk.png"
mkdir -p "$PREFIX/share/doc"
cp -R share/doc/localsend-gtk "$PREFIX/share/doc/"
if command -v update-desktop-database >/dev/null 2>&1; then
    update-desktop-database "$PREFIX/share/applications"
fi
echo "Installed. GTK4 >= 4.12, libadwaita >= 1.5 and compatible system libraries are required."
echo "Your desktop must include $PREFIX/share in its XDG data search paths to find the launcher and Dolphin menu."
echo "Nautilus reads scripts only from its per-user scripts directory; see README.md for setup."
