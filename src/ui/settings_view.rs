use super::*;
use crate::appearance::{Color, ColorMode};
use crate::i18n::{self, Locale};
use crate::settings::QuickSaveMode;

pub(super) struct NetworkControls {
    applied: RefCell<Settings>,
    running: Cell<bool>,
    busy: Cell<bool>,
    status: adw::ActionRow,
    pending: gtk::Label,
    start: gtk::Button,
    restart: gtk::Button,
    stop: gtk::Button,
    alias: adw::EntryRow,
    pin: adw::PasswordEntryRow,
    port: adw::SpinRow,
}

const ALL_COLOR_MODES: [ColorMode; 5] = [
    ColorMode::System,
    ColorMode::LocalSend,
    ColorMode::Oled,
    ColorMode::Yaru,
    ColorMode::Custom,
];
const FALLBACK_COLOR_MODES: [ColorMode; 4] = [
    ColorMode::LocalSend,
    ColorMode::Oled,
    ColorMode::Yaru,
    ColorMode::Custom,
];

fn visible_color_modes(settings: &Settings, system_available: bool) -> &'static [ColorMode] {
    // A portal restart must not make a saved System selection look like
    // LocalSend. Keep its row until the user chooses another mode, so they can
    // explicitly select LocalSend while the accent is temporarily unavailable.
    if system_available || effective_color_mode(settings) == ColorMode::System {
        &ALL_COLOR_MODES
    } else {
        &FALLBACK_COLOR_MODES
    }
}

fn color_mode_source(mode: ColorMode) -> &'static str {
    match mode {
        ColorMode::System => "System",
        ColorMode::LocalSend => "LocalSend",
        ColorMode::Oled => "OLED",
        ColorMode::Yaru => "Yaru",
        ColorMode::Custom => "Custom",
    }
}

fn effective_color_mode(settings: &Settings) -> ColorMode {
    if settings.oled {
        ColorMode::Oled
    } else {
        settings.color_mode
    }
}

fn color_mode_index(settings: &Settings, system_available: bool) -> u32 {
    let mode = effective_color_mode(settings);
    visible_color_modes(settings, system_available)
        .iter()
        .position(|candidate| *candidate == mode)
        .or_else(|| {
            visible_color_modes(settings, system_available)
                .iter()
                .position(|candidate| *candidate == ColorMode::LocalSend)
        })
        .unwrap_or(0) as u32
}

fn should_fallback_initial_system(
    first_result: bool,
    system_available: bool,
    settings: &Settings,
) -> bool {
    first_result && !system_available && effective_color_mode(settings) == ColorMode::System
}

fn refresh_color_row(
    row: &adw::ComboRow,
    settings: &Settings,
    system_available: bool,
    changing: &Cell<bool>,
) {
    let list = row
        .model()
        .and_downcast::<gtk::StringList>()
        .expect("Color row StringList model");
    let labels: Vec<_> = visible_color_modes(settings, system_available)
        .iter()
        .map(|mode| i18n::tr(color_mode_source(*mode)))
        .collect();
    let unchanged = labels.len() == list.n_items() as usize
        && labels
            .iter()
            .enumerate()
            .all(|(index, label)| list.string(index as u32).as_deref() == Some(label));
    changing.set(true);
    if !unchanged {
        let labels: Vec<_> = labels.iter().map(String::as_str).collect();
        list.splice(0, list.n_items(), &labels);
    }
    row.set_selected(color_mode_index(settings, system_available));
    changing.set(false);
}

impl Ui {
    pub(super) fn server_is_running(&self) -> bool {
        self.0
            .network_controls
            .borrow()
            .as_ref()
            .is_some_and(|controls| controls.running.get())
    }
    pub(super) fn settings_page(&self) -> gtk::ScrolledWindow {
        let content = gtk::Box::new(gtk::Orientation::Vertical, 24);
        margins(&content, 24);
        content.set_margin_top(40);
        let heading = translated_label("Settings", "page-title");
        heading.set_halign(gtk::Align::Center);
        content.append(&heading);
        let general = adw::PreferencesGroup::new();
        general.set_widget_name("settings-general");
        i18n::bind_property(&general, "title", "General");
        let theme_choices = gtk::StringList::new(&["System", "Light", "Dark"]);
        let theme = adw::ComboRow::builder()
            .title("Theme")
            .model(&theme_choices)
            .selected(self.0.settings.borrow().theme)
            .build();
        i18n::bind_property(&theme, "title", "Theme");
        i18n::bind_combo_strings(&theme, &["System", "Light", "Dark"]);
        {
            let ui = self.clone();
            theme.connect_selected_notify(move |row| {
                if i18n::is_updating() {
                    return;
                }
                ui.0.settings.borrow_mut().theme = row.selected();
                apply_theme(row.selected());
                ui.0.appearance.update(&ui.0.settings.borrow());
                ui.save_settings();
            });
        }
        general.add(&theme);
        let system_available = Rc::new(Cell::new(self.0.appearance.system_accent().is_some()));
        let changing_color = Rc::new(Cell::new(false));
        let color_choices = gtk::StringList::new(&[]);
        let color = adw::ComboRow::builder()
            .title("Color")
            .model(&color_choices)
            .build();
        i18n::bind_property(&color, "title", "Color");
        refresh_color_row(
            &color,
            &self.0.settings.borrow(),
            system_available.get(),
            &changing_color,
        );
        let custom = adw::EntryRow::builder()
            .title("Custom color (#RRGGBB)")
            .text(&self.0.settings.borrow().custom_color)
            .show_apply_button(true)
            .visible(effective_color_mode(&self.0.settings.borrow()) == ColorMode::Custom)
            .build();
        i18n::bind_property(&custom, "title", "Custom color (#RRGGBB)");
        let picker = gtk::ColorDialogButton::new(Some(
            gtk::ColorDialog::builder()
                .title("Custom color")
                .with_alpha(false)
                .modal(true)
                .build(),
        ));
        i18n::bind_property(&picker.dialog().unwrap(), "title", "Custom color");
        picker.set_rgba(
            &Color::parse(&self.0.settings.borrow().custom_color)
                .expect("Validated color")
                .rgba(),
        );
        picker.set_valign(gtk::Align::Center);
        i18n::bind_property(&picker, "tooltip-text", "Choose custom color");
        i18n::bind_accessible_label(&picker, "Choose custom color");
        picker.add_css_class("color-swatch");
        custom.add_suffix(&picker);
        {
            let ui = self.clone();
            let theme = theme.clone();
            let custom = custom.clone();
            let system_available = system_available.clone();
            let changing = changing_color.clone();
            color.connect_selected_notify(move |row| {
                if i18n::is_updating() || changing.get() {
                    return;
                }
                let Some(mode) =
                    visible_color_modes(&ui.0.settings.borrow(), system_available.get())
                        .get(row.selected() as usize)
                        .copied()
                else {
                    return;
                };
                let oled = mode == ColorMode::Oled;
                {
                    let mut settings = ui.0.settings.borrow_mut();
                    settings.oled = oled;
                    settings.color_mode = mode;
                }
                if oled {
                    ui.0.window.add_css_class("oled");
                    theme.set_selected(2);
                } else {
                    ui.0.window.remove_css_class("oled");
                }
                custom.set_visible(mode == ColorMode::Custom);
                ui.0.appearance.update(&ui.0.settings.borrow());
                ui.save_settings();
                refresh_color_row(
                    row,
                    &ui.0.settings.borrow(),
                    system_available.get(),
                    &changing,
                );
            });
        }
        {
            let weak_ui = Rc::downgrade(&self.0);
            let weak_color = color.downgrade();
            let system_available = system_available.clone();
            let changing = changing_color.clone();
            i18n::on_locale_changed(move || {
                let (Some(inner), Some(color)) = (weak_ui.upgrade(), weak_color.upgrade()) else {
                    return false;
                };
                refresh_color_row(
                    &color,
                    &inner.settings.borrow(),
                    system_available.get(),
                    &changing,
                );
                true
            });
        }
        {
            let weak_ui = Rc::downgrade(&self.0);
            let weak_color = color.downgrade();
            let weak_custom = custom.downgrade();
            let system_available = system_available.clone();
            let changing = changing_color.clone();
            let received_initial = Cell::new(false);
            self.0.appearance.on_system_accent_changed(move |accent| {
                let (Some(inner), Some(color), Some(custom)) = (
                    weak_ui.upgrade(),
                    weak_color.upgrade(),
                    weak_custom.upgrade(),
                ) else {
                    return false;
                };
                let first = !received_initial.replace(true);
                let fallback_initial_system = {
                    let settings = inner.settings.borrow();
                    should_fallback_initial_system(first, accent.is_some(), &settings)
                };
                if fallback_initial_system {
                    let ui = Ui(inner.clone());
                    inner.settings.borrow_mut().color_mode = ColorMode::LocalSend;
                    inner.appearance.update(&inner.settings.borrow());
                    if !ui.save_settings() {
                        inner.settings.borrow_mut().color_mode = ColorMode::System;
                        inner.appearance.update(&inner.settings.borrow());
                    }
                }
                let available = accent.is_some();
                system_available.set(available);
                refresh_color_row(&color, &inner.settings.borrow(), available, &changing);
                custom.set_visible(
                    effective_color_mode(&inner.settings.borrow()) == ColorMode::Custom,
                );
                true
            });
        }
        {
            let ui = self.clone();
            let picker = picker.clone();
            custom.connect_apply(move |row| {
                let Some(color) = Color::parse(&row.text()) else {
                    row.add_css_class("error");
                    ui.toast_text("Use a six-digit color such as #009688.");
                    return;
                };
                if ui.set_custom_color(color) {
                    row.set_text(&color.hex());
                    picker.set_rgba(&color.rgba());
                    row.remove_css_class("error");
                }
            });
        }
        {
            let ui = self.clone();
            let custom = custom.clone();
            picker.connect_rgba_notify(move |picker| {
                let color = Color::from_rgba(&picker.rgba());
                if ui.set_custom_color(color) {
                    custom.set_text(&color.hex());
                    custom.remove_css_class("error");
                } else {
                    let old = Color::parse(&ui.0.settings.borrow().custom_color)
                        .expect("Validated color");
                    picker.set_rgba(&old.rgba());
                }
            });
        }
        custom.connect_changed(|row| row.remove_css_class("error"));
        general.add(&color);
        general.add(&custom);
        let languages: Vec<_> = Locale::ALL
            .iter()
            .map(|locale| locale.native_name())
            .collect();
        let language_choices = gtk::StringList::new(&languages);
        let language = adw::ComboRow::builder()
            .title("Language")
            .model(&language_choices)
            .selected(
                Locale::ALL
                    .iter()
                    .position(|locale| *locale == self.0.settings.borrow().language)
                    .unwrap_or(0) as u32,
            )
            .build();
        i18n::bind_property(&language, "title", "Language");
        i18n::bind_combo_strings(&language, &languages);
        {
            let ui = self.clone();
            language.connect_selected_notify(move |row| {
                if i18n::is_updating() {
                    return;
                }
                let Some(locale) = Locale::ALL.get(row.selected() as usize).copied() else {
                    return;
                };
                let previous = ui.0.settings.borrow().language;
                if locale == previous {
                    return;
                }
                ui.0.settings.borrow_mut().language = locale;
                if ui.save_settings() {
                    i18n::set_locale(locale);
                } else {
                    ui.0.settings.borrow_mut().language = previous;
                    row.set_selected(
                        Locale::ALL
                            .iter()
                            .position(|value| *value == previous)
                            .unwrap_or(0) as u32,
                    );
                }
            });
        }
        general.add(&language);
        self.desktop_settings(&general);
        let animations = adw::SwitchRow::builder()
            .title("Animations")
            .active(self.0.settings.borrow().animations)
            .build();
        i18n::bind_property(&animations, "title", "Animations");
        {
            let ui = self.clone();
            animations.connect_active_notify(move |row| {
                ui.0.settings.borrow_mut().animations = row.is_active();
                ui.0.stack
                    .set_transition_duration(if row.is_active() { 150 } else { 0 });
                ui.save_settings();
            });
        }
        general.add(&animations);
        let compact_rail = adw::SwitchRow::builder()
            .title("Compact sidebar on narrow windows")
            .subtitle("Keep the left navigation rail instead of showing a bottom bar")
            .active(self.0.settings.borrow().compact_rail_narrow)
            .build();
        i18n::bind_property(&compact_rail, "title", "Compact sidebar on narrow windows");
        i18n::bind_property(
            &compact_rail,
            "subtitle",
            "Keep the left navigation rail instead of showing a bottom bar",
        );
        {
            let ui = self.clone();
            compact_rail.connect_active_notify(move |row| {
                ui.0.settings.borrow_mut().compact_rail_narrow = row.is_active();
                ui.adapt_layout();
                ui.save_settings();
            });
        }
        general.add(&compact_rail);
        content.append(&general);
        let receive = adw::PreferencesGroup::new();
        receive.set_widget_name("settings-receive");
        i18n::bind_property(&receive, "title", "Receive");
        let quick_mode = self.0.settings.borrow().quick_save;
        let quick = adw::SwitchRow::builder()
            .title("Quick Save")
            .subtitle("Automatically accept incoming transfers")
            .active(quick_mode == QuickSaveMode::On)
            .build();
        i18n::bind_property(&quick, "title", "Quick Save");
        i18n::bind_property(
            &quick,
            "subtitle",
            "Automatically accept incoming transfers",
        );
        let favorite_quick = adw::SwitchRow::builder()
            .title("Quick Save for \"Favorites\"")
            .active(quick_mode == QuickSaveMode::Paired)
            .build();
        i18n::bind_property(&favorite_quick, "title", "Quick Save for \"Favorites\"");
        let changing_quick_mode = Rc::new(Cell::new(false));
        {
            let ui = self.clone();
            let favorite_quick = favorite_quick.clone();
            let changing = changing_quick_mode.clone();
            quick.connect_active_notify(move |row| {
                if changing.get() {
                    return;
                }
                let previous = ui.0.settings.borrow().quick_save;
                let next = if row.is_active() {
                    QuickSaveMode::On
                } else if previous == QuickSaveMode::On {
                    QuickSaveMode::Off
                } else {
                    previous
                };
                if next == previous {
                    return;
                }
                ui.0.settings.borrow_mut().quick_save = next;
                changing.set(true);
                favorite_quick.set_active(next == QuickSaveMode::Paired);
                changing.set(false);
                if ui.save_settings() {
                    let _ = ui.0.commands.try_send(Command::QuickSave(next));
                    if next == QuickSaveMode::On {
                        alert_text(
                            &ui.0.window,
                            "Quick Save",
                            "File requests are now accepted automatically. Be aware that everyone on the local network can send you files.",
                        );
                    }
                } else {
                    ui.0.settings.borrow_mut().quick_save = previous;
                    changing.set(true);
                    row.set_active(previous == QuickSaveMode::On);
                    favorite_quick.set_active(previous == QuickSaveMode::Paired);
                    changing.set(false);
                }
            });
        }
        {
            let ui = self.clone();
            let quick = quick.clone();
            let changing = changing_quick_mode;
            favorite_quick.connect_active_notify(move |row| {
                if changing.get() {
                    return;
                }
                let previous = ui.0.settings.borrow().quick_save;
                let next = if row.is_active() {
                    QuickSaveMode::Paired
                } else if previous == QuickSaveMode::Paired {
                    QuickSaveMode::Off
                } else {
                    previous
                };
                if next == previous {
                    return;
                }
                ui.0.settings.borrow_mut().quick_save = next;
                changing.set(true);
                quick.set_active(next == QuickSaveMode::On);
                changing.set(false);
                if ui.save_settings() {
                    let _ = ui.0.commands.try_send(Command::QuickSave(next));
                    if next == QuickSaveMode::Paired {
                        alert_text(
                            &ui.0.window,
                            "Quick Save for \"Favorites\"",
                            "File requests are now accepted automatically from devices in your favorites list.",
                        );
                    }
                } else {
                    ui.0.settings.borrow_mut().quick_save = previous;
                    changing.set(true);
                    quick.set_active(previous == QuickSaveMode::On);
                    row.set_active(previous == QuickSaveMode::Paired);
                    changing.set(false);
                }
            });
        }
        receive.add(&quick);
        receive.add(&favorite_quick);
        let pin = adw::PasswordEntryRow::builder()
            .title("Receive PIN (empty to disable)")
            .text(
                self.0
                    .settings
                    .borrow()
                    .receive_pin
                    .as_deref()
                    .unwrap_or(""),
            )
            .build();
        i18n::bind_property(&pin, "title", "Receive PIN (empty to disable)");
        receive.add(&pin);
        let destination = adw::ActionRow::builder()
            .title("Save to folder")
            .subtitle(self.0.settings.borrow().save_dir.to_string_lossy().as_ref())
            .activatable(true)
            .build();
        i18n::bind_property(&destination, "title", "Save to folder");
        destination.add_suffix(&gtk::Image::from_icon_name("folder-open-symbolic"));
        {
            let ui = self.clone();
            destination.connect_activated(move |row| {
                let row = row.clone();
                let ui = ui.clone();
                glib::spawn_future_local(async move {
                    let dialog = gtk::FileDialog::builder()
                        .title("Save received files to")
                        .build();
                    i18n::bind_property(&dialog, "title", "Save received files to");
                    if let Ok(file) = dialog.select_folder_future(Some(&ui.0.window)).await {
                        if let Some(path) = file.path() {
                            let previous = ui.0.settings.borrow().save_dir.clone();
                            ui.0.settings.borrow_mut().save_dir = path;
                            if ui.save_settings() {
                                row.set_subtitle(
                                    &ui.0.settings.borrow().save_dir.to_string_lossy(),
                                );
                                ui.refresh_network_controls();
                            } else {
                                ui.0.settings.borrow_mut().save_dir = previous;
                            }
                        }
                    }
                });
            });
        }
        receive.add(&destination);
        let history = adw::SwitchRow::builder()
            .title("Save to history")
            .active(self.0.settings.borrow().save_to_history)
            .build();
        i18n::bind_property(&history, "title", "Save to history");
        {
            let ui = self.clone();
            let restoring = Cell::new(false);
            history.connect_active_notify(move |row| {
                if restoring.get() {
                    return;
                }
                let previous = ui.0.settings.borrow().save_to_history;
                if row.is_active() == previous {
                    return;
                }
                ui.0.settings.borrow_mut().save_to_history = row.is_active();
                if !ui.save_settings() {
                    ui.0.settings.borrow_mut().save_to_history = previous;
                    restoring.set(true);
                    row.set_active(previous);
                    restoring.set(false);
                }
            });
        }
        receive.add(&history);
        content.append(&receive);
        let send = adw::PreferencesGroup::new();
        send.set_widget_name("settings-send");
        i18n::bind_property(&send, "title", "Send");
        let approve = adw::SwitchRow::builder()
            .title("Share via link: auto accept")
            .subtitle("Allow anyone with your active link to download the shared files")
            .active(self.0.settings.borrow().share_auto_accept)
            .build();
        i18n::bind_property(&approve, "title", "Share via link: auto accept");
        i18n::bind_property(
            &approve,
            "subtitle",
            "Allow anyone with your active link to download the shared files",
        );
        *self.0.share_auto_row.borrow_mut() = Some(approve.clone());
        {
            let ui = self.clone();
            approve.connect_active_notify(move |row| {
                if !ui.set_share_auto_accept(row.is_active()) {
                    let previous = ui.0.settings.borrow().share_auto_accept;
                    row.set_active(previous);
                }
            });
        }
        send.add(&approve);
        content.append(&send);
        let network = adw::PreferencesGroup::new();
        network.set_widget_name("settings-network");
        i18n::bind_property(&network, "title", "Network");
        let status = adw::ActionRow::new();
        i18n::bind_property(&status, "title", "Server");
        let actions = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        actions.set_homogeneous(true);
        margins(&actions, 12);
        let start = gtk::Button::with_label("Start");
        let restart = gtk::Button::with_label("Restart");
        let stop = gtk::Button::with_label("Stop");
        for (button, source) in [(&start, "Start"), (&restart, "Restart"), (&stop, "Stop")] {
            i18n::bind_property(button, "label", source);
            actions.append(button);
        }
        start.add_css_class("suggested-action");
        network.add(&status);
        network.add(&actions);
        let alias = adw::EntryRow::builder()
            .title("Device name")
            .text(&self.0.settings.borrow().alias)
            .build();
        i18n::bind_property(&alias, "title", "Device name");
        network.add(&alias);
        let port = adw::SpinRow::with_range(1.0, 65535.0, 1.0);
        i18n::bind_property(&port, "title", "Port");
        port.set_value(self.0.settings.borrow().port as f64);
        network.add(&port);
        let encryption = adw::ActionRow::builder()
            .title("Encryption")
            .subtitle("HTTPS")
            .build();
        i18n::bind_property(&encryption, "title", "Encryption");
        encryption.add_suffix(&gtk::Image::from_icon_name("channel-secure-symbolic"));
        network.add(&encryption);
        let pending = translated_label("Restart the server to apply the settings!", "dim-label");
        pending.set_wrap(true);
        pending.set_wrap_mode(gtk::pango::WrapMode::WordChar);
        pending.set_halign(gtk::Align::Center);
        content.append(&pending);
        content.append(&network);
        let about = gtk::Button::with_label("About LocalSend GTK");
        i18n::bind_property(&about, "label", "About LocalSend GTK");
        about.add_css_class("text-button");
        {
            let ui = self.clone();
            about.connect_clicked(move |_| alert_text(&ui.0.window, "LocalSend GTK", concat!("A community GTK4 client for LocalSend.\n\nVersion ", env!("CARGO_PKG_VERSION"), "\nRust · GTK4 · Wayland\n\nInterface and logo based on LocalSend (Apache-2.0). This project is independent of the official LocalSend app.")));
        }
        content.append(&about);
        *self.0.network_controls.borrow_mut() = Some(NetworkControls {
            applied: RefCell::new(self.0.settings.borrow().clone()),
            running: Cell::new(self.0.offline_fixture || self.0.identity.borrow().is_some()),
            busy: Cell::new(false),
            status,
            pending,
            start: start.clone(),
            restart: restart.clone(),
            stop: stop.clone(),
            alias: alias.clone(),
            pin: pin.clone(),
            port: port.clone(),
        });
        {
            let ui = self.clone();
            alias.connect_changed(move |_| ui.refresh_network_controls());
        }
        {
            let ui = self.clone();
            pin.connect_changed(move |_| ui.refresh_network_controls());
        }
        {
            let ui = self.clone();
            port.connect_value_notify(move |_| ui.refresh_network_controls());
        }
        {
            let ui = self.clone();
            start.connect_clicked(move |_| ui.change_server(true));
        }
        {
            let ui = self.clone();
            restart.connect_clicked(move |_| ui.apply_network_settings());
        }
        {
            let ui = self.clone();
            stop.connect_clicked(move |_| ui.stop_server());
        }
        self.refresh_network_controls();
        scrolled(&clamped(&content))
    }

    fn refresh_network_controls(&self) {
        let controls = self.0.network_controls.borrow();
        let Some(controls) = controls.as_ref() else {
            return;
        };
        let applied = controls.applied.borrow();
        let pin = controls.pin.text();
        let changed = controls.alias.text().trim() != applied.alias
            || controls.port.value() as u16 != applied.port
            || pin.as_str() != applied.receive_pin.as_deref().unwrap_or("")
            || self.0.settings.borrow().save_dir != applied.save_dir;
        controls
            .pending
            .set_visible(controls.running.get() && changed);
        controls.start.set_visible(!controls.running.get());
        controls.restart.set_visible(controls.running.get());
        controls.stop.set_visible(controls.running.get());
        for button in [&controls.start, &controls.restart, &controls.stop] {
            button.set_sensitive(!controls.busy.get());
        }
        if !controls.busy.get() {
            i18n::bind_property(
                &controls.status,
                "subtitle",
                if controls.running.get() {
                    "Online"
                } else {
                    "Offline"
                },
            );
        }
    }

    pub(super) fn server_state_changed(&self, running: bool, settings: Settings) {
        self.0.server_running.set(running);
        if let Some(controls) = self.0.network_controls.borrow().as_ref() {
            *controls.applied.borrow_mut() = settings;
            controls.running.set(running);
            controls.busy.set(false);
        }
        if !running {
            i18n::bind_property(&self.0.online, "label", "Offline");
            self.0.receive_view.cancel_acknowledged();
            self.0.canceling_receive.set(false);
        } else {
            i18n::bind_property(&self.0.online, "label", "Ready");
        }
        self.refresh_network_controls();
    }

    fn set_server_busy(&self, source: &str) {
        if let Some(controls) = self.0.network_controls.borrow().as_ref() {
            controls.busy.set(true);
            i18n::bind_property(&controls.status, "subtitle", source);
        }
        self.refresh_network_controls();
    }

    pub(super) fn restart_server(&self) {
        self.change_server(false);
    }

    fn change_server(&self, start: bool) {
        if self.0.sending.get()
            || self.0.incoming_pending.get() > 0
            || self.0.receive_view.has_active()
        {
            self.toast_text("Finish or cancel the current transfer before restarting the server.");
            return;
        }
        let mut draft = self.0.settings.borrow().clone();
        {
            let controls = self.0.network_controls.borrow();
            let Some(controls) = controls.as_ref() else {
                return;
            };
            if controls.busy.get() {
                return;
            }
            draft.alias = controls.alias.text().trim().to_owned();
            draft.port = controls.port.value() as u16;
            let pin = controls.pin.text().to_string();
            draft.receive_pin = if pin.is_empty() { None } else { Some(pin) };
        }
        if let Err(error) = draft.validate() {
            alert(&self.0.window, "Invalid settings", &error.to_string());
            return;
        }
        let previous = self.0.settings.replace(draft.clone());
        if !self.save_settings() {
            *self.0.settings.borrow_mut() = previous;
            return;
        }
        self.set_server_busy("Starting server…");
        if self.0.offline_fixture {
            if let Some(identity) = self.0.identity.borrow_mut().as_mut() {
                identity.alias.clone_from(&draft.alias);
                identity.port = draft.port;
            }
            self.0.alias_label.set_label(&draft.alias);
            self.0.alias_label.set_tooltip_text(Some(&draft.alias));
            i18n::bind_property(&self.0.online, "label", "Ready");
            self.web_receive_stopped();
            self.web_share_stopped();
            self.server_state_changed(true, draft);
        } else if self
            .0
            .commands
            .try_send(if start {
                Command::StartServer(draft)
            } else {
                Command::Reconfigure(draft)
            })
            .is_err()
        {
            if let Some(controls) = self.0.network_controls.borrow().as_ref() {
                controls.busy.set(false);
            }
            self.refresh_network_controls();
            self.toast_text("Server controls are unavailable.");
        }
    }

    fn stop_server(&self) {
        if self
            .0
            .network_controls
            .borrow()
            .as_ref()
            .is_none_or(|controls| controls.busy.get())
        {
            return;
        }
        self.set_server_busy("Stopping server…");
        if self.0.offline_fixture {
            self.web_receive_stopped();
            self.web_share_stopped();
            let applied = self
                .0
                .network_controls
                .borrow()
                .as_ref()
                .unwrap()
                .applied
                .borrow()
                .clone();
            self.server_state_changed(false, applied);
        } else if self.0.commands.try_send(Command::StopServer).is_err() {
            if let Some(controls) = self.0.network_controls.borrow().as_ref() {
                controls.busy.set(false);
            }
            self.refresh_network_controls();
            self.toast_text("Server controls are unavailable.");
        }
    }

    fn set_custom_color(&self, color: Color) -> bool {
        let value = color.hex();
        let previous = self.0.settings.borrow().custom_color.clone();
        if previous == value {
            return true;
        }
        self.0.settings.borrow_mut().custom_color = value;
        if !self.save_settings() {
            self.0.settings.borrow_mut().custom_color = previous;
            return false;
        }
        self.0.appearance.update(&self.0.settings.borrow());
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_color_is_inserted_without_shifting_mode_identity() {
        assert_eq!(
            visible_color_modes(&Settings::default(), true),
            [
                ColorMode::System,
                ColorMode::LocalSend,
                ColorMode::Oled,
                ColorMode::Yaru,
                ColorMode::Custom,
            ]
        );
        assert_eq!(
            visible_color_modes(&Settings::default(), false),
            [
                ColorMode::LocalSend,
                ColorMode::Oled,
                ColorMode::Yaru,
                ColorMode::Custom,
            ]
        );

        for (mode, without_system, with_system) in [
            (ColorMode::LocalSend, 0, 1),
            (ColorMode::Oled, 1, 2),
            (ColorMode::Yaru, 2, 3),
            (ColorMode::Custom, 3, 4),
        ] {
            let settings = Settings {
                color_mode: mode,
                oled: mode == ColorMode::Oled,
                ..Settings::default()
            };
            assert_eq!(color_mode_index(&settings, false), without_system);
            assert_eq!(color_mode_index(&settings, true), with_system);
        }
        let system = Settings {
            color_mode: ColorMode::System,
            ..Settings::default()
        };
        assert_eq!(color_mode_index(&system, true), 0);
        assert_eq!(color_mode_index(&system, false), 0);
        assert!(should_fallback_initial_system(true, false, &system));
        assert!(!should_fallback_initial_system(false, false, &system));
        assert!(!should_fallback_initial_system(true, true, &system));
        assert!(!should_fallback_initial_system(
            true,
            false,
            &Settings::default()
        ));
    }

    #[test]
    fn system_color_can_be_replaced_while_the_portal_is_temporarily_unavailable() {
        let mut settings = Settings {
            color_mode: ColorMode::System,
            ..Settings::default()
        };
        for available in [true, false] {
            let modes = visible_color_modes(&settings, available);
            assert_eq!(
                modes[color_mode_index(&settings, available) as usize],
                ColorMode::System
            );
            assert_eq!(modes[1], ColorMode::LocalSend);
        }

        // Selecting LocalSend while offline is a distinct row, and rebuilding
        // the list after that selection keeps its identity at the new index.
        settings.color_mode = visible_color_modes(&settings, false)[1];
        assert_eq!(settings.color_mode, ColorMode::LocalSend);
        assert_eq!(color_mode_index(&settings, false), 0);
        assert_eq!(
            visible_color_modes(&settings, false)[0],
            ColorMode::LocalSend
        );
        assert!(!visible_color_modes(&settings, false).contains(&ColorMode::System));

        // Portal recovery restores the extra option without changing the user's
        // selection back to System.
        assert_eq!(color_mode_index(&settings, true), 1);
        assert_eq!(
            visible_color_modes(&settings, true)[color_mode_index(&settings, true) as usize],
            ColorMode::LocalSend
        );
    }
}
