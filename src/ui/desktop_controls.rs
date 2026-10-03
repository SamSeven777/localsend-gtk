use super::*;
use crate::desktop::{self, TrayEvent};

impl Ui {
    pub(super) fn desktop_settings(&self, general: &adw::PreferencesGroup) {
        // The desktop may reveal a hidden window directly (for example, a
        // second launch activating the existing GApplication). Release the
        // background hold in that path too, without retaining the Ui in a cycle.
        let weak = Rc::downgrade(&self.0);
        self.0.window.connect_visible_notify(move |window| {
            if window.is_visible() {
                if let Some(inner) = weak.upgrade() {
                    inner.hidden_start.set(false);
                    inner.background_hold.borrow_mut().take();
                }
            }
        });
        let show = gio::SimpleAction::new("show", None);
        let weak = Rc::downgrade(&self.0);
        show.connect_activate(move |_, _| {
            if let Some(inner) = weak.upgrade() {
                Ui(inner).show_window();
            }
        });
        self.0.window.add_action(&show);
        let tray = adw::SwitchRow::builder()
            .title("Show tray icon")
            .active(self.0.settings.borrow().tray_enabled)
            .build();
        i18n::bind_property(&tray, "title", "Show tray icon");
        let close = adw::SwitchRow::builder()
            .title("Minimize to tray")
            .subtitle("Keep receiving after closing the window, even without a tray icon")
            .active(self.0.settings.borrow().close_to_tray)
            .build();
        i18n::bind_property(&close, "title", "Minimize to tray");
        i18n::bind_property(
            &close,
            "subtitle",
            "Keep receiving after closing the window, even without a tray icon",
        );
        let startup_supported = desktop::autostart_supported();
        let (auto, hidden) = self.startup_rows(startup_supported);
        if !startup_supported && !self.0.offline_fixture {
            // Older Flatpak builds wrote unusable launchers into private config.
            // The backend removes only entries marked as owned by this app.
            if let Err(error) = desktop::set_autostart(false, false) {
                tracing::warn!(%error, "Could not remove obsolete Flatpak autostart entry");
            }
        }
        let status = adw::ActionRow::builder()
            .title("Tray status")
            .subtitle("Disabled")
            .build();
        i18n::bind_property(&status, "title", "Tray status");
        i18n::bind_property(&status, "subtitle", "Disabled");
        *self.0.tray_status.borrow_mut() = Some(status.clone());
        {
            let ui = self.clone();
            tray.connect_active_notify(move |row| {
                let previous = ui.0.settings.borrow().tray_enabled;
                if previous == row.is_active() {
                    return;
                }
                ui.0.settings.borrow_mut().tray_enabled = row.is_active();
                if !ui.save_settings() {
                    ui.0.settings.borrow_mut().tray_enabled = previous;
                    row.set_active(previous);
                    return;
                }
                ui.configure_desktop();
            });
        }
        {
            let ui = self.clone();
            close.connect_active_notify(move |row| {
                let previous = ui.0.settings.borrow().close_to_tray;
                if previous == row.is_active() {
                    return;
                }
                ui.0.settings.borrow_mut().close_to_tray = row.is_active();
                if !ui.save_settings() {
                    ui.0.settings.borrow_mut().close_to_tray = previous;
                    row.set_active(previous);
                }
            });
        }
        for row in [&tray, &close, &auto, &hidden] {
            general.add(row);
        }
        general.add(&status);
    }

    pub(super) fn startup_rows(&self, supported: bool) -> (adw::SwitchRow, adw::SwitchRow) {
        if !supported {
            let changed = {
                let mut settings = self.0.settings.borrow_mut();
                let changed = settings.autostart || settings.start_minimized;
                settings.autostart = false;
                settings.start_minimized = false;
                changed
            };
            if changed {
                self.save_settings();
            }
        }
        let auto = adw::SwitchRow::builder()
            .title("Launch at startup")
            .active(supported && self.0.settings.borrow().autostart)
            .sensitive(supported)
            .build();
        i18n::bind_property(&auto, "title", "Launch at startup");
        let hidden = adw::SwitchRow::builder()
            .title("Launch minimized")
            .active(supported && self.0.settings.borrow().start_minimized)
            .sensitive(supported && auto.is_active())
            .build();
        i18n::bind_property(&hidden, "title", "Launch minimized");
        if supported {
            i18n::bind_property(
                &hidden,
                "subtitle",
                "Start in the background, even without a tray icon",
            );
        } else {
            for row in [&auto, &hidden] {
                i18n::bind_property(
                    row,
                    "subtitle",
                    "Launch at startup is not available in Flatpak yet",
                );
            }
        }
        {
            let ui = self.clone();
            let hidden = hidden.clone();
            auto.connect_active_notify(move |row| {
                if !supported {
                    row.set_active(false);
                    return;
                }
                let minimized = ui.0.settings.borrow().start_minimized;
                if ui.set_startup_settings(row.is_active(), minimized) {
                    hidden.set_sensitive(row.is_active());
                } else {
                    let enabled = ui.0.settings.borrow().autostart;
                    row.set_active(enabled);
                }
            });
        }
        {
            let ui = self.clone();
            hidden.connect_active_notify(move |row| {
                if !supported {
                    row.set_active(false);
                    return;
                }
                let enabled = ui.0.settings.borrow().autostart;
                if !ui.set_startup_settings(enabled, row.is_active()) {
                    let hidden = ui.0.settings.borrow().start_minimized;
                    row.set_active(hidden);
                }
            });
        }
        (auto, hidden)
    }

    fn set_startup_settings(&self, enabled: bool, hidden: bool) -> bool {
        let previous = self.0.settings.borrow().clone();
        if previous.autostart == enabled && previous.start_minimized == hidden {
            return true;
        }
        if !self.0.offline_fixture {
            if let Err(error) = desktop::set_autostart(enabled, hidden) {
                alert(
                    &self.0.window,
                    "Could not change startup settings",
                    &error.to_string(),
                );
                return false;
            }
        }
        {
            let mut settings = self.0.settings.borrow_mut();
            settings.autostart = enabled;
            settings.start_minimized = hidden;
        }
        if !self.save_settings() {
            *self.0.settings.borrow_mut() = previous.clone();
            if !self.0.offline_fixture {
                let _ = desktop::set_autostart(previous.autostart, previous.start_minimized);
            }
            return false;
        }
        true
    }

    fn tray_status(&self, text: &str) {
        if let Some(row) = self.0.tray_status.borrow().as_ref() {
            i18n::bind_property(row, "subtitle", text);
        }
    }

    fn tray_error(&self, error: &str) {
        if let Some(row) = self.0.tray_status.borrow().as_ref() {
            i18n::bind_format_property(
                row,
                "subtitle",
                "Unavailable: {error}",
                &[("error", error.into())],
            );
        }
    }

    fn reveal_after_tray_failure(&self) {
        // An explicit background start or a close-to-background request does
        // not depend on a tray host. A late tray startup failure must not undo it.
        if self.0.background_hold.borrow().is_none() {
            self.show_window();
        }
    }

    #[cfg(test)]
    pub(super) fn fixture_tray_failure(&self) {
        self.reveal_after_tray_failure();
    }

    pub(super) fn configure_desktop(&self) {
        let generation = self.0.tray_generation.get().wrapping_add(1);
        self.0.tray_generation.set(generation);
        self.0.tray.borrow_mut().take();
        if !self.0.settings.borrow().tray_enabled {
            self.tray_status("Disabled");
            if self.0.background_hold.borrow().is_some() {
                self.show_window();
            }
            return;
        }
        if self.0.offline_fixture {
            self.tray_status("Preview — no desktop integration started");
            return;
        }
        self.tray_status("Connecting to the desktop tray…");
        let (tx, rx) = async_channel::unbounded();
        let task = tokio::spawn(desktop::start_tray(tx));
        let weak = Rc::downgrade(&self.0);
        glib::spawn_future_local(async move {
            let result = task.await;
            let Some(inner) = weak.upgrade() else {
                return;
            };
            let ui = Ui(inner);
            if ui.0.tray_generation.get() != generation || ui.0.quitting.get() {
                return;
            }
            match result {
                Ok(Ok(handle)) => {
                    *ui.0.tray.borrow_mut() = Some(handle);
                    ui.desktop_event(TrayEvent::AvailabilityChanged);
                }
                Ok(Err(error)) => {
                    ui.tray_error(&error);
                    ui.reveal_after_tray_failure();
                    ui.toast(&error);
                    return;
                }
                Err(error) => {
                    ui.tray_error(&error.to_string());
                    ui.reveal_after_tray_failure();
                    ui.toast(&i18n::tr_format(
                        "Could not start the desktop tray: {error}",
                        &[("error", error.to_string())],
                    ));
                    return;
                }
            }
            drop(ui);
            while let Ok(event) = rx.recv().await {
                let Some(inner) = weak.upgrade() else {
                    break;
                };
                let ui = Ui(inner);
                if ui.0.tray_generation.get() != generation || ui.0.quitting.get() {
                    break;
                }
                ui.desktop_event(event);
            }
        });
    }

    pub(super) fn desktop_event(&self, event: TrayEvent) {
        match event {
            TrayEvent::Open => self.show_window(),
            TrayEvent::Receive | TrayEvent::Send | TrayEvent::Settings => {
                self.0.stack.set_visible_child_name(match event {
                    TrayEvent::Receive => "receive",
                    TrayEvent::Send => "send",
                    _ => "settings",
                });
                self.show_window();
            }
            TrayEvent::Quit => {
                self.0.quitting.set(true);
                self.0.window.close();
            }
            TrayEvent::AvailabilityChanged => {
                let available = self
                    .0
                    .tray
                    .borrow()
                    .as_ref()
                    .is_some_and(|tray| tray.is_available());
                self.tray_status(if available {
                    "Available"
                } else {
                    "Unavailable on this desktop"
                });
                if !available {
                    if !self.0.hidden_start.get() && self.0.background_hold.borrow().is_none() {
                        self.show_window();
                    }
                } else if self.0.hidden_start.replace(false) {
                    self.hide_in_tray();
                }
            }
        }
    }

    pub fn request_hidden_start(&self) {
        self.0.hidden_start.set(true);
        if self.hide_in_tray() || self.hide_to_background() {
            self.0.hidden_start.set(false);
        }
    }

    pub(super) fn hide_to_background(&self) -> bool {
        if self.0.incoming_pending.get() != 0 {
            return false;
        }
        if self.0.background_hold.borrow().is_none() {
            if let Some(app) = self.0.window.application() {
                *self.0.background_hold.borrow_mut() = Some(app.hold());
            }
        }
        self.0.window.set_visible(false);
        true
    }

    pub(super) fn hide_in_tray(&self) -> bool {
        if self.0.incoming_pending.get() != 0
            || !self.0.settings.borrow().tray_enabled
            || !self
                .0
                .tray
                .borrow()
                .as_ref()
                .is_some_and(|tray| tray.is_available())
        {
            return false;
        }
        if self.0.background_hold.borrow().is_none() {
            if let Some(app) = self.0.window.application() {
                *self.0.background_hold.borrow_mut() = Some(app.hold());
            }
        }
        self.0.window.set_visible(false);
        true
    }

    pub fn show_window(&self) {
        self.0.hidden_start.set(false);
        self.0.window.present();
        self.0.background_hold.borrow_mut().take();
    }
}
