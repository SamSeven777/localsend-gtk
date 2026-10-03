//! Linux desktop integration: StatusNotifierItem and an app-owned XDG autostart entry.
//!
//! Tray callbacks only enqueue events. GTK window operations belong on its main loop.
use gdk_pixbuf::prelude::PixbufLoaderExt;
use ksni::TrayMethods;
use std::{
    io,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};

const APP_ID: &str = "org.localsend.localsend_gtk";
const AUTOSTART_FILE: &str = "org.localsend.localsend_gtk.desktop";
const OWNER_KEY: &str = "X-LocalSend-GTK-Owner";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrayEvent {
    Open,
    Receive,
    Send,
    Settings,
    Quit,
    /// Read `TrayHandle::is_available()` when handling this event. A boolean in
    /// the event itself could be stale after toggling the tray or a shell restart.
    AvailabilityChanged,
}

struct Availability {
    online: AtomicBool,
    events: async_channel::Sender<TrayEvent>,
}

impl Availability {
    fn set(&self, online: bool) {
        if self.online.swap(online, Ordering::AcqRel) != online {
            let _ = self.events.try_send(TrayEvent::AvailabilityChanged);
        }
    }
}

struct NativeTray {
    availability: Arc<Availability>,
    icon: ksni::Icon,
}

impl NativeTray {
    fn emit(&self, event: TrayEvent) {
        let _ = self.availability.events.try_send(event);
    }
}

impl ksni::Tray for NativeTray {
    fn id(&self) -> String {
        APP_ID.into()
    }
    fn title(&self) -> String {
        "LocalSend".into()
    }
    fn category(&self) -> ksni::Category {
        ksni::Category::Communications
    }
    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        vec![self.icon.clone()]
    }
    fn activate(&mut self, _: i32, _: i32) {
        self.emit(TrayEvent::Open);
    }
    fn secondary_activate(&mut self, _: i32, _: i32) {
        self.emit(TrayEvent::Open);
    }
    fn tool_tip(&self) -> ksni::ToolTip {
        ksni::ToolTip {
            title: "LocalSend".into(),
            description: "Send and receive files on your local network".into(),
            icon_pixmap: vec![self.icon.clone()],
            ..Default::default()
        }
    }
    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        let mut menu = Vec::new();
        for (label, event) in [
            ("Open LocalSend", TrayEvent::Open),
            ("Receive", TrayEvent::Receive),
            ("Send", TrayEvent::Send),
            ("Settings", TrayEvent::Settings),
        ] {
            menu.push(
                ksni::menu::StandardItem {
                    label: label.into(),
                    activate: Box::new(move |tray: &mut Self| tray.emit(event)),
                    ..Default::default()
                }
                .into(),
            );
        }
        menu.push(ksni::MenuItem::Separator);
        menu.push(
            ksni::menu::StandardItem {
                label: "Quit".into(),
                activate: Box::new(|tray: &mut Self| tray.emit(TrayEvent::Quit)),
                ..Default::default()
            }
            .into(),
        );
        menu
    }
    fn watcher_online(&self) {
        self.availability.set(true);
    }
    fn watcher_offline(&self, reason: ksni::OfflineReason) -> bool {
        tracing::debug!(?reason, "Desktop tray became unavailable");
        self.availability.set(false);
        // Recover if the desktop shell restarts. The UI must reveal its window
        // while no tray host exists, even though this service keeps waiting.
        true
    }
}

impl Drop for NativeTray {
    fn drop(&mut self) {
        self.availability.set(false);
    }
}

/// Sole owner of a running tray. Dropping it unregisters the item.
pub struct TrayHandle {
    service: ksni::Handle<NativeTray>,
    availability: Arc<Availability>,
}

impl TrayHandle {
    pub fn is_available(&self) -> bool {
        self.availability.online.load(Ordering::Acquire) && !self.service.is_closed()
    }

    pub fn shutdown(&self) {
        self.availability.set(false);
        // shutdown() submits immediately; awaiting its result is optional.
        drop(self.service.shutdown());
    }
}

impl Drop for TrayHandle {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Fails if the session has no tray host. Callers may hide a window only after
/// this succeeds AND `is_available()` is true. Use an unbounded event channel.
pub async fn start_tray(events: async_channel::Sender<TrayEvent>) -> Result<TrayHandle, String> {
    // gdk-pixbuf is a standalone image decoder, not a GTK widget. Its GObjects
    // remain in this synchronous helper and never cross an await/thread boundary.
    let icon = tray_icon()?;
    let availability = Arc::new(Availability {
        online: AtomicBool::new(true),
        events,
    });
    let tray = NativeTray {
        availability: availability.clone(),
        icon,
    };
    let service = tray
        .spawn()
        .await
        .map_err(|error| format!("The desktop tray is unavailable: {error}"))?;
    Ok(TrayHandle {
        service,
        availability,
    })
}

fn tray_icon() -> Result<ksni::Icon, String> {
    let loader = gdk_pixbuf::PixbufLoader::with_type("png").map_err(|e| e.to_string())?;
    loader
        .write(include_bytes!("../assets/logo.png"))
        .map_err(|e| e.to_string())?;
    loader.close().map_err(|e| e.to_string())?;
    let source = loader
        .pixbuf()
        .ok_or("Could not decode the LocalSend tray icon")?;
    let image = source
        .scale_simple(32, 32, gdk_pixbuf::InterpType::Bilinear)
        .ok_or("Could not scale the LocalSend tray icon")?;
    let pixels = image.read_pixel_bytes();
    let stride = image.rowstride() as usize;
    let channels = image.n_channels() as usize;
    let mut data = Vec::with_capacity(32 * 32 * 4);
    for y in 0..32 {
        for x in 0..32 {
            let offset = y * stride + x * channels;
            let rgb = &pixels[offset..offset + channels];
            // StatusNotifierItem pixmaps use network-order ARGB, not RGBA.
            data.extend_from_slice(&[
                if image.has_alpha() { rgb[3] } else { 255 },
                rgb[0],
                rgb[1],
                rgb[2],
            ]);
        }
    }
    Ok(ksni::Icon {
        width: 32,
        height: 32,
        data,
    })
}

fn config_directory() -> io::Result<PathBuf> {
    directories::BaseDirs::new()
        .map(|dirs| dirs.config_dir().to_owned())
        .ok_or_else(|| io::Error::other("Could not locate the user configuration directory"))
}

/// Flatpak requires the Background portal to register host login startup.
/// Check the sandbox marker rather than an inherited environment variable.
pub fn autostart_supported() -> bool {
    !Path::new("/.flatpak-info").is_file()
}

/// Update only this app's managed autostart file; enabling never launches it.
pub fn set_autostart(enabled: bool, start_minimized: bool) -> io::Result<()> {
    let executable = if enabled {
        Some(std::env::current_exe()?)
    } else {
        None
    };
    set_autostart_at(
        &config_directory()?,
        executable.as_deref(),
        start_minimized,
        autostart_supported(),
    )
}

pub fn is_autostart_enabled() -> io::Result<bool> {
    autostart_enabled_at(&config_directory()?, autostart_supported())
}

fn autostart_path(config: &Path) -> PathBuf {
    config.join("autostart").join(AUTOSTART_FILE)
}

fn owned_entry(path: &Path) -> io::Result<Option<glib::KeyFile>> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if !meta.file_type().is_file() => {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "The autostart path is not an app-owned regular file; it has not been changed.",
            ))
        }
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    }
    let keyfile = glib::KeyFile::new();
    let contents = std::fs::read_to_string(path)?;
    keyfile
        .load_from_data(&contents, glib::KeyFileFlags::NONE)
        .map_err(io::Error::other)?;
    if keyfile.string("Desktop Entry", OWNER_KEY).ok().as_deref() != Some(APP_ID) {
        return Err(io::Error::new(io::ErrorKind::AlreadyExists,
            "An autostart file not created by LocalSend already uses this name; it has not been changed."));
    }
    Ok(Some(keyfile))
}

fn autostart_enabled_at(config: &Path, supported: bool) -> io::Result<bool> {
    if !supported {
        return Ok(false);
    }
    let Some(entry) = owned_entry(&autostart_path(config))? else {
        return Ok(false);
    };
    Ok(!entry.boolean("Desktop Entry", "Hidden").unwrap_or(false)
        && entry
            .boolean("Desktop Entry", "X-GNOME-Autostart-enabled")
            .unwrap_or(true))
}

fn set_autostart_at(
    config: &Path,
    executable: Option<&Path>,
    start_minimized: bool,
    supported: bool,
) -> io::Result<()> {
    if executable.is_some() && !supported {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Launch at startup is not available in Flatpak yet",
        ));
    }
    let path = autostart_path(config);
    let previous = owned_entry(&path)?;
    if let Some(executable) = executable {
        let contents = desktop_entry(executable, start_minimized)?;
        crate::settings::atomic_write(&path, contents.as_bytes())
    } else if previous.is_some() {
        std::fs::remove_file(path)
    } else {
        Ok(())
    }
}

fn desktop_entry(executable: &Path, start_minimized: bool) -> io::Result<String> {
    let executable_text = executable.to_str().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "Autostart requires a UTF-8 executable path",
        )
    })?;
    if !executable.is_absolute() || executable_text.chars().any(|c| c.is_control() || c == '=') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Autostart requires an absolute executable path without control characters or '='.",
        ));
    }
    // The Desktop Entry spec applies string escaping before Exec argument
    // quoting. Escape both layers, including literal percent field-code markers.
    let mut argument = String::from("\"");
    for ch in executable_text.chars() {
        match ch {
            '%' => argument.push_str("%%"),
            '\\' | '"' | '$' | '`' => {
                argument.push('\\');
                argument.push(ch);
            }
            _ => argument.push(ch),
        }
    }
    argument.push('"');
    let command = argument.replace('\\', "\\\\");
    // GIO validates argv[0] before expanding desktop-entry field codes, so an
    // executable containing '%' otherwise looks for a non-existent '%%' path.
    // env resolves this one argument after expansion, without invoking a shell.
    let launcher = if executable_text.contains('%') {
        "/usr/bin/env -- "
    } else {
        ""
    };
    let hidden = if start_minimized { " --hidden" } else { "" };
    Ok(format!("[Desktop Entry]\nType=Application\nVersion=1.0\nName=LocalSend GTK\nComment=Send and receive files on your local network\nExec={launcher}{command}{hidden}\nIcon=localsend-gtk\nTerminal=false\nStartupNotify=false\nX-GNOME-Autostart-enabled=true\n{OWNER_KEY}={APP_ID}\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ksni::Tray;

    #[test]
    fn flatpak_autostart_rejects_enabling_and_cleans_only_stale_owned_entries() {
        let root = tempfile::tempdir().unwrap();
        let executable = root.path().join("localsend-gtk");
        let error = set_autostart_at(root.path(), Some(&executable), false, false).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Unsupported);
        assert!(!root.path().join("autostart").exists());

        // Migrate a private launcher written by a build without sandbox support.
        set_autostart_at(root.path(), Some(&executable), true, true).unwrap();
        let stale = std::fs::read(autostart_path(root.path())).unwrap();
        assert!(!autostart_enabled_at(root.path(), false).unwrap());
        assert!(set_autostart_at(root.path(), Some(&executable), false, false).is_err());
        assert_eq!(std::fs::read(autostart_path(root.path())).unwrap(), stale);
        set_autostart_at(root.path(), None, false, false).unwrap();
        assert!(!autostart_path(root.path()).exists());

        // A user-provided entry must survive the same migration.
        std::fs::write(
            autostart_path(root.path()),
            "[Desktop Entry]\nName=Custom\n",
        )
        .unwrap();
        assert!(set_autostart_at(root.path(), None, false, false).is_err());
        assert_eq!(
            std::fs::read_to_string(autostart_path(root.path())).unwrap(),
            "[Desktop Entry]\nName=Custom\n"
        );
        assert!(!autostart_enabled_at(root.path(), false).unwrap());
    }

    #[test]
    fn autostart_enable_update_disable_only_changes_the_owned_file() {
        let root = tempfile::tempdir().unwrap();
        assert!(!autostart_enabled_at(root.path(), true).unwrap());
        set_autostart_at(root.path(), None, false, true).unwrap();
        assert!(!root.path().join("autostart").exists());
        let executable = root.path().join("Local Send");
        set_autostart_at(root.path(), Some(&executable), true, true).unwrap();
        assert!(autostart_enabled_at(root.path(), true).unwrap());
        let other = root.path().join("autostart/another-app.desktop");
        std::fs::write(&other, "preserve me").unwrap();
        let contents = std::fs::read_to_string(autostart_path(root.path())).unwrap();
        assert!(contents
            .lines()
            .find(|line| line.starts_with("Exec="))
            .unwrap()
            .ends_with(" --hidden"));
        set_autostart_at(root.path(), Some(&executable), false, true).unwrap();
        assert!(!std::fs::read_to_string(autostart_path(root.path()))
            .unwrap()
            .contains("--hidden"));
        set_autostart_at(root.path(), None, false, true).unwrap();
        assert!(!autostart_enabled_at(root.path(), true).unwrap());
        assert_eq!(std::fs::read_to_string(other).unwrap(), "preserve me");
        assert_eq!(
            std::fs::read_dir(root.path().join("autostart"))
                .unwrap()
                .count(),
            1
        );
    }

    #[test]
    fn autostart_preserves_unowned_files_and_rejects_injected_exec_lines() {
        let root = tempfile::tempdir().unwrap();
        let target = autostart_path(root.path());
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        let original = "[Desktop Entry]\nType=Application\nName=Custom launcher\nExec=other-app\n";
        std::fs::write(&target, original).unwrap();
        assert!(
            set_autostart_at(root.path(), Some(&root.path().join("app")), false, true).is_err()
        );
        assert!(set_autostart_at(root.path(), None, false, true).is_err());
        assert_eq!(std::fs::read_to_string(target).unwrap(), original);
        for path in [
            Path::new("relative-app"),
            Path::new("/tmp/app\nExec=other"),
            Path::new("/tmp/app=other"),
        ] {
            assert!(desktop_entry(path, true).is_err());
        }
    }

    #[cfg(unix)]
    #[test]
    fn autostart_does_not_follow_or_remove_an_unowned_symlink() {
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("keep.desktop");
        std::fs::write(&target, "keep").unwrap();
        std::fs::create_dir_all(root.path().join("autostart")).unwrap();
        std::os::unix::fs::symlink(&target, autostart_path(root.path())).unwrap();
        assert!(set_autostart_at(root.path(), None, false, true).is_err());
        assert!(
            set_autostart_at(root.path(), Some(&root.path().join("app")), false, true).is_err()
        );
        assert_eq!(std::fs::read_to_string(target).unwrap(), "keep");
        assert!(std::fs::symlink_metadata(autostart_path(root.path()))
            .unwrap()
            .file_type()
            .is_symlink());
    }

    #[cfg(unix)]
    #[test]
    fn desktop_exec_launches_the_literal_path_with_reserved_characters() {
        use gio::prelude::*;
        use std::{
            os::unix::fs::PermissionsExt,
            time::{Duration, Instant},
        };
        let root = tempfile::tempdir().unwrap();
        for name in ["Local Send '$`\\\";青", "Local Send '$`\\\";%f青"] {
            let executable = root.path().join(name);
            std::fs::write(
                &executable,
                "#!/bin/sh\nprintf '%s\\n' \"$0\" \"$@\" > \"$0.args\"\n",
            )
            .unwrap();
            std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
            set_autostart_at(root.path(), Some(&executable), true, true).unwrap();
            let app = gio::DesktopAppInfo::from_filename(autostart_path(root.path())).unwrap();
            app.launch(&[], gio::AppLaunchContext::NONE).unwrap();
            let arguments = executable.with_file_name(format!(
                "{}.args",
                executable.file_name().unwrap().to_str().unwrap()
            ));
            let deadline = Instant::now() + Duration::from_secs(3);
            while !arguments.exists() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            assert_eq!(
                std::fs::read_to_string(arguments).unwrap(),
                format!("{}\n--hidden\n", executable.display())
            );
        }
    }

    #[test]
    fn tray_menu_activation_and_availability_are_delivered_without_gtk() {
        let (events, receiver) = async_channel::unbounded();
        let availability = Arc::new(Availability {
            online: AtomicBool::new(true),
            events,
        });
        let mut tray = NativeTray {
            availability: availability.clone(),
            icon: tray_icon().unwrap(),
        };
        assert_eq!(tray.icon.data.len(), 32 * 32 * 4);
        assert!(tray
            .icon
            .data
            .as_chunks::<4>()
            .0
            .iter()
            .any(|pixel| pixel[0] != 0));
        let actions = [
            TrayEvent::Open,
            TrayEvent::Receive,
            TrayEvent::Send,
            TrayEvent::Settings,
            TrayEvent::Quit,
        ];
        let mut index = 0;
        for item in tray.menu() {
            if let ksni::MenuItem::Standard(item) = item {
                (item.activate)(&mut tray);
                assert_eq!(receiver.try_recv().unwrap(), actions[index]);
                index += 1;
            }
        }
        assert_eq!(index, actions.len());
        tray.activate(0, 0);
        assert_eq!(receiver.try_recv().unwrap(), TrayEvent::Open);
        assert!(tray.watcher_offline(ksni::OfflineReason::No));
        assert!(!availability.online.load(Ordering::Acquire));
        assert_eq!(receiver.try_recv().unwrap(), TrayEvent::AvailabilityChanged);
        tray.watcher_online();
        assert!(availability.online.load(Ordering::Acquire));
        assert_eq!(receiver.try_recv().unwrap(), TrayEvent::AvailabilityChanged);
    }

    #[test]
    #[ignore = "run under dbus-run-session with LOCALSEND_TRAY_TEST_BUS=1; never use a real desktop bus"]
    fn tray_private_bus_registers_activates_recovers_and_unregisters() {
        use gio::prelude::*;
        use std::{cell::RefCell, rc::Rc, time::Duration};

        assert_eq!(
            std::env::var("LOCALSEND_TRAY_TEST_BUS").as_deref(),
            Ok("1"),
            "This test requires an explicitly isolated D-Bus session"
        );
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let _runtime = runtime.enter();
        let context = glib::MainContext::new();
        context
            .with_thread_default(|| {
                context.block_on(async {
                    let connection = gio::bus_get_future(gio::BusType::Session).await.unwrap();
                    let owns_name = connection
                        .call_future(
                            Some("org.freedesktop.DBus"),
                            "/org/freedesktop/DBus",
                            "org.freedesktop.DBus",
                            "NameHasOwner",
                            Some(&("org.kde.StatusNotifierWatcher",).to_variant()),
                            None,
                            gio::DBusCallFlags::NONE,
                            2000,
                        )
                        .await
                        .unwrap();
                    assert!(
                        !owns_name.child_get::<bool>(0),
                        "Refusing to replace an existing desktop watcher"
                    );

                    let (missing_events, _missing_receiver) = async_channel::unbounded();
                    assert!(
                        tokio::time::timeout(
                            Duration::from_secs(3),
                            tokio::spawn(start_tray(missing_events))
                        )
                        .await
                        .unwrap()
                        .unwrap()
                        .is_err(),
                        "No host must not allow a hidden window"
                    );
                    let (events, receiver) = async_channel::unbounded();

                    const WATCHER: &str = r#"<node><interface name="org.kde.StatusNotifierWatcher">
                <method name="RegisterStatusNotifierItem"><arg type="s" direction="in"/></method>
                <method name="RegisterStatusNotifierHost"><arg type="s" direction="in"/></method>
                <property name="IsStatusNotifierHostRegistered" type="b" access="read"/>
                <property name="ProtocolVersion" type="i" access="read"/>
                <property name="RegisteredStatusNotifierItems" type="as" access="read"/>
                </interface></node>"#;
                    let interface = gio::DBusNodeInfo::for_xml(WATCHER)
                        .unwrap()
                        .lookup_interface("org.kde.StatusNotifierWatcher")
                        .unwrap();
                    let registered = Rc::new(RefCell::new(Vec::<String>::new()));
                    let seen = registered.clone();
                    let registration = connection
                        .register_object("/StatusNotifierWatcher", &interface)
                        .method_call(move |_, _, _, _, method, parameters, invocation| {
                            if method == "RegisterStatusNotifierItem" {
                                seen.borrow_mut().push(parameters.child_get::<String>(0));
                            }
                            invocation.return_value(Some(&().to_variant()));
                        })
                        .property(|_, _, _, _, property| match property {
                            "IsStatusNotifierHostRegistered" => true.to_variant(),
                            "ProtocolVersion" => 0_i32.to_variant(),
                            "RegisteredStatusNotifierItems" => Vec::<String>::new().to_variant(),
                            other => panic!("Unexpected watcher property: {other}"),
                        })
                        .build()
                        .unwrap();
                    let request_name = connection
                        .call_future(
                            Some("org.freedesktop.DBus"),
                            "/org/freedesktop/DBus",
                            "org.freedesktop.DBus",
                            "RequestName",
                            Some(&("org.kde.StatusNotifierWatcher", 4_u32).to_variant()),
                            None,
                            gio::DBusCallFlags::NONE,
                            2000,
                        )
                        .await
                        .unwrap();
                    assert_eq!(request_name.child_get::<u32>(0), 1);

                    let handle = tokio::time::timeout(
                        Duration::from_secs(3),
                        tokio::spawn(start_tray(events)),
                    )
                    .await
                    .unwrap()
                    .unwrap()
                    .unwrap();
                    assert!(handle.is_available());
                    let item_name = registered.borrow()[0].clone();
                    connection
                        .call_future(
                            Some(&item_name),
                            "/StatusNotifierItem",
                            "org.kde.StatusNotifierItem",
                            "Activate",
                            Some(&(0_i32, 0_i32).to_variant()),
                            None,
                            gio::DBusCallFlags::NONE,
                            2000,
                        )
                        .await
                        .unwrap();
                    assert_eq!(
                        tokio::time::timeout(Duration::from_secs(2), receiver.recv())
                            .await
                            .unwrap()
                            .unwrap(),
                        TrayEvent::Open
                    );

                    connection
                        .call_future(
                            Some("org.freedesktop.DBus"),
                            "/org/freedesktop/DBus",
                            "org.freedesktop.DBus",
                            "ReleaseName",
                            Some(&("org.kde.StatusNotifierWatcher",).to_variant()),
                            None,
                            gio::DBusCallFlags::NONE,
                            2000,
                        )
                        .await
                        .unwrap();
                    assert_eq!(
                        tokio::time::timeout(Duration::from_secs(2), receiver.recv())
                            .await
                            .unwrap()
                            .unwrap(),
                        TrayEvent::AvailabilityChanged
                    );
                    assert!(!handle.is_available());
                    connection
                        .call_future(
                            Some("org.freedesktop.DBus"),
                            "/org/freedesktop/DBus",
                            "org.freedesktop.DBus",
                            "RequestName",
                            Some(&("org.kde.StatusNotifierWatcher", 4_u32).to_variant()),
                            None,
                            gio::DBusCallFlags::NONE,
                            2000,
                        )
                        .await
                        .unwrap();
                    assert_eq!(
                        tokio::time::timeout(Duration::from_secs(2), receiver.recv())
                            .await
                            .unwrap()
                            .unwrap(),
                        TrayEvent::AvailabilityChanged
                    );
                    assert!(handle.is_available());

                    let service = handle.service.clone();
                    drop(handle);
                    tokio::time::timeout(Duration::from_secs(2), async {
                        while !service.is_closed() {
                            tokio::time::sleep(Duration::from_millis(10)).await;
                        }
                    })
                    .await
                    .unwrap();
                    drop(service);
                    connection.unregister_object(registration).unwrap();
                    connection
                        .call_future(
                            Some("org.freedesktop.DBus"),
                            "/org/freedesktop/DBus",
                            "org.freedesktop.DBus",
                            "ReleaseName",
                            Some(&("org.kde.StatusNotifierWatcher",).to_variant()),
                            None,
                            gio::DBusCallFlags::NONE,
                            2000,
                        )
                        .await
                        .unwrap();
                })
            })
            .unwrap();
    }
}
