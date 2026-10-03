# LocalSend GTK Flatpak

This directory contains the Flatpak manifest and AppStream metadata for `org.localsend.localsend_gtk`.
It builds the current local checkout and downloads locked Cargo dependencies during
the build. It is a local development package, not a published Flathub application.

## Building with flatpak-builder

To build and test the Flatpak package locally:

```bash
# Install Flatpak GNOME 50 SDK and Platform
flatpak install flathub org.gnome.Platform//50 org.gnome.Sdk//50 org.freedesktop.Sdk.Extension.rust-stable//25.08

# Build the package
flatpak-builder --user --install --force-clean build-dir packaging/flatpak/org.localsend.localsend_gtk.json

# Run the installed Flatpak
flatpak run org.localsend.localsend_gtk
```

The manifest mounts the Rust SDK extension into the build environment; installing
the extension alone does not make its tools available to the builder.
If the build host cannot mount `rofiles-fuse` (for example, some WSL setups), add
`--disable-rofiles-fuse` to the builder command. This changes the build cache's
copy strategy; it does not disable the Flatpak sandbox.

## Current sandbox limitations

Login autostart is not supported by this Flatpak package yet. The application
detects the Flatpak sandbox and disables both **Launch at startup** and **Launch
minimized**, with an explanation in the selected language. Any obsolete private
autostart entry created by an older version is removed if it belongs to this app,
and the corresponding saved settings are reset. Host autostart support requires
the desktop Background portal. Launch the package manually.

The native packages' Dolphin service menu and Nautilus script are not installed
by this manifest. Select files through the application's file chooser or the
desktop's Open With action instead.
