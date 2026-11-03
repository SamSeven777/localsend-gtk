# LocalSend GTK AppImage

`scripts/package-appimage.sh` creates a thin x86_64 or aarch64 AppImage. It
bundles the application binary, launcher, icon, desktop entry, AppStream
metadata, notices and licenses. It deliberately uses the host's GTK 4.12+ and
libadwaita 1.5+ libraries, matching the native packages and keeping the image
small. A fully self-contained GTK runtime is not included yet.

## Build

Install `appimagetool` or download `appimagetool-x86_64.AppImage`, then run:

```sh
APPIMAGETOOL=/path/to/appimagetool-x86_64.AppImage \
APPIMAGETOOL_EXTRACT=1 \
bash scripts/package-appimage.sh
```

`APPIMAGETOOL_EXTRACT=1` passes `--appimage-extract-and-run` to an AppImage
tool when FUSE is unavailable. It is not needed for a native `appimagetool`
binary. The result is written to:

```text
target/dist/localsend-gtk-v<version>-linux-<arch>.AppImage
```

and a matching `.sha256` file.

The script validates the AppDir contract before packaging: one root desktop
file, a matching icon and `.DirIcon`, an executable `AppRun`, AppStream
metadata and `StartupWMClass=org.localsend.localsend_gtk`.

## Update information and zsync

The release workflow derives the AppImage update source from the repository
that triggered the build, so forks and upstream need no hardcoded owner:

```text
gh-releases-zsync|OWNER|REPO|latest|localsend-gtk-*-linux-<arch>.AppImage.zsync
```

It embeds that string with `appimagetool -u` and publishes the generated
`.AppImage.zsync` next to the AppImage. Gearlever and AppImageUpdate read the
embedded information, consult the latest published GitHub release and download
only the changed blocks.

For local release testing, provide the update information explicitly:

```sh
APPIMAGETOOL=/path/to/appimagetool-x86_64.AppImage \
APPIMAGETOOL_EXTRACT=1 \
APPIMAGE_UPDATE_INFO="gh-releases-zsync|Alarez777|localsend-gtk|latest|localsend-gtk-*-linux-x86_64.AppImage.zsync" \
bash scripts/package-appimage.sh
```

`zsyncmake` must be installed. Without `APPIMAGE_UPDATE_INFO` or
`GITHUB_REPOSITORY`, the script creates a plain AppImage and no `.zsync`, which
is useful for local testing.

The GitHub Actions release keeps the release in **draft** until it is validated
and published. Gearlever only sees the update after publication, because it
uses the latest published release.

## Run

```sh
chmod +x target/dist/localsend-gtk-*.AppImage
./target/dist/localsend-gtk-*.AppImage
```

Without FUSE, use `--appimage-extract-and-run`.

## Desktop integration

The AppImage contains `org.localsend.localsend_gtk.png`, a compatibility
`localsend-gtk.png` alias and `.DirIcon`. Its embedded desktop file includes
`StartupWMClass=org.localsend.localsend_gtk` and
`X-AppImage-Name=LocalSend GTK`.

Integration is delegated to tools such as Gearlever or AppImageLauncher. They
copy the desktop file, install the icon and associate the running window with
the launcher, so the dock icon and **Pin to dock** work. After replacing an
AppImage that was already integrated, remove and re-add the integration in the
tool so it reads the new desktop file.

The application does not install a desktop entry by itself. The file icon shown
by Nautilus for the `.AppImage` file itself also depends on AppImageLauncher,
Gearlever or a compatible thumbnailer.

## Autostart

The AppImage runtime sets `APPIMAGE` to the outer AppImage path. The application
uses that path for XDG autostart instead of the temporary mount, so **Launch at
startup** keeps working after the AppImage is closed or moved. The setting is
optional and never enabled automatically.

## Runtime requirements

The AppImage needs the host's GTK 4.12+, libadwaita 1.5+, a compatible glibc,
`xdg-desktop-portal` and a desktop portal backend for native file dialogs. It
does not bundle GTK, graphics drivers, D-Bus or the portal.
