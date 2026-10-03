Name:           localsend-gtk
Version:        0.1.0
Release:        1%{?dist}
Summary:        Native GTK4/Adwaita GUI for LocalSend in Rust

License:        MIT AND Apache-2.0
URL:            https://github.com/SamSeven777/localsend-gtk
Source0:        %{name}-%{version}.tar.gz

BuildRequires:  cargo
BuildRequires:  rust
BuildRequires:  gtk4-devel >= 4.12
BuildRequires:  libadwaita-devel >= 1.5
BuildRequires:  openssl-devel
BuildRequires:  desktop-file-utils
Requires:       gtk4 >= 4.12
Requires:       libadwaita >= 1.5

%description
LocalSend GTK is a Rust + GTK4 / libadwaita client for LocalSend protocol v2.
Designed for Linux and Wayland desktops, with encrypted local file and text transfers,
native GTK file pickers, favorites, and persistent settings.

%prep
%autosetup

%build
cargo build --release --locked

%install
rm -rf $RPM_BUILD_ROOT
install -D -p -m 0755 target/release/localsend-gtk %{buildroot}%{_bindir}/localsend-gtk
install -D -p -m 0644 assets/logo.png %{buildroot}%{_datadir}/icons/hicolor/512x512/apps/localsend-gtk.png
install -D -p -m 0644 packaging/rpm/localsend-gtk.desktop %{buildroot}%{_datadir}/applications/org.localsend.localsend_gtk.desktop
install -D -p -m 0644 packaging/desktop/localsend-dolphin.desktop %{buildroot}%{_datadir}/kio/servicemenus/localsend-dolphin.desktop
install -D -p -m 0755 "packaging/desktop/nautilus-scripts/Send with LocalSend" "%{buildroot}%{_datadir}/nautilus-scripts/Send with LocalSend"
install -D -p -m 0644 vendor/localsend-rs/UPSTREAM.md %{buildroot}%{_docdir}/%{name}/localsend-rs/UPSTREAM.md
install -D -p -m 0644 vendor/localsend-rs/THIRD_PARTY_NOTICES.md %{buildroot}%{_docdir}/%{name}/localsend-rs/THIRD_PARTY_NOTICES.md

%check
desktop-file-validate %{buildroot}%{_datadir}/applications/org.localsend.localsend_gtk.desktop

%files
%{_bindir}/localsend-gtk
%{_datadir}/icons/hicolor/512x512/apps/localsend-gtk.png
%{_datadir}/applications/org.localsend.localsend_gtk.desktop
%{_datadir}/kio/servicemenus/localsend-dolphin.desktop
"%{_datadir}/nautilus-scripts/Send with LocalSend"
%license LICENSE LICENSES/*.txt
%{_docdir}/%{name}/localsend-rs/
%doc README.md THIRD_PARTY_NOTICES.md

%changelog
* Sat Oct 03 2026 SamSeven777 <qiryse7en@gmail.com> - 0.1.0-1
- Initial release with native GTK4 interface, corrected receive consent and animated receive logo.
