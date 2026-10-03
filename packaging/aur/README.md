# Arch binary package recipe

Use `PKGBUILD` and `.SRCINFO` from the repository's current `main` branch.
The recipe version and SHA-256 are updated together only after a GitHub release
archive is published and verified. A release tag can therefore contain the
previous release's valid recipe; it never uses a placeholder checksum.

On Arch Linux, review the recipe and run `makepkg -si` in this directory.
This recipe packages the prebuilt Linux x86_64 archive and needs glibc 2.39+,
GTK 4.12+ and libadwaita 1.5+.

The package includes the Dolphin menu and Nautilus script. Nautilus requires the
per-user registration described in the root README. The package installation
does not modify users' home directories.

Providing these files in this repository does not publish a package on AUR.
