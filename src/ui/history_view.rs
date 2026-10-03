//! Received files and messages, with the same actions as LocalSend's history.
use super::*;
use crate::history::{HistoryEntry, HistoryKind};

pub(super) struct HistoryView {
    dialog: glib::WeakRef<adw::Dialog>,
    entries: glib::WeakRef<gtk::Box>,
    clear: glib::WeakRef<gtk::Button>,
}

fn entry_title(entry: &HistoryEntry) -> &str {
    match &entry.kind {
        HistoryKind::File { name, .. } => name,
        HistoryKind::Message { text, .. } | HistoryKind::Legacy { text } => text,
    }
}

fn entry_time(entry: &HistoryEntry) -> String {
    entry.timestamp_display().unwrap_or_default()
}

fn detail(body: &gtk::Box, caption: &str, value: &str) {
    let group = gtk::Box::new(gtk::Orientation::Vertical, 4);
    group.append(&translated_label(caption, "secondary"));
    let value = dialogs::wrapped_label(value, "body-text");
    value.set_width_chars(1);
    value.set_selectable(true);
    group.append(&value);
    body.append(&group);
}

impl Ui {
    pub(super) fn record_received_file(
        &self,
        name: String,
        path: PathBuf,
        size: u64,
        sender: String,
    ) {
        if !self.0.settings.borrow().save_to_history {
            return;
        }
        self.0
            .history
            .borrow_mut()
            .add_file(name, path, size, sender);
        self.persist_history();
        self.refresh_history();
    }

    pub(super) fn record_received_message(&self, text: String, sender: String) {
        if !self.0.settings.borrow().save_to_history {
            return;
        }
        self.0.history.borrow_mut().add_message(text, sender);
        self.persist_history();
        self.refresh_history();
    }

    fn persist_history(&self) -> bool {
        // An unreadable history is never replaced with an empty or partial one.
        // New receipts remain usable in memory until the saved file is repaired.
        if self.0.history_load_error.is_some() {
            return false;
        }
        if self.0.offline_fixture {
            return true;
        }
        match self
            .0
            .history
            .borrow()
            .save(Settings::directory().join("history.json"))
        {
            Ok(()) => true,
            Err(error) => {
                self.toast(&i18n::tr_format(
                    "Could not save history: {error}",
                    &[("error", error.to_string())],
                ));
                false
            }
        }
    }

    pub(super) fn show_history(&self) {
        if let Some(dialog) = self
            .0
            .history_view
            .borrow()
            .as_ref()
            .and_then(|view| view.dialog.upgrade())
        {
            dialog.present(Some(&self.0.window));
            return;
        }
        let dialog = adw::Dialog::builder()
            .content_width(600)
            .content_height(520)
            .build();
        i18n::bind_property(&dialog, "title", "History");
        let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
        content.append(&adw::HeaderBar::new());
        let body = gtk::Box::new(gtk::Orientation::Vertical, 20);
        margins(&body, 20);
        let actions = gtk::FlowBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .column_spacing(12)
            .row_spacing(8)
            .min_children_per_line(1)
            .max_children_per_line(2)
            .build();
        let folder = i18n::button("Open folder");
        let ui = self.clone();
        folder.connect_clicked(move |_| {
            let path = ui.0.settings.borrow().save_dir.clone();
            ui.open_history_path(path, false);
        });
        actions.insert(&folder, -1);
        let clear = i18n::button("Delete history");
        let ui = self.clone();
        clear.connect_clicked(move |_| ui.confirm_clear_history());
        actions.insert(&clear, -1);
        body.append(&actions);
        if let Some(error) = &self.0.history_load_error {
            let warning = dialogs::translated_wrapped_label(
                "History could not be loaded. New entries will be kept for this session only.",
                "secondary",
            );
            warning.set_width_chars(1);
            body.append(&warning);
            let diagnostic = dialogs::wrapped_label("", "secondary");
            diagnostic.set_width_chars(1);
            diagnostic.set_selectable(true);
            i18n::bind_format_property(
                &diagnostic,
                "label",
                "Could not load history: {error}",
                &[("error", error.clone())],
            );
            body.append(&diagnostic);
        }
        let entries = gtk::Box::new(gtk::Orientation::Vertical, 8);
        body.append(&entries);
        let scroll = dialogs::content_scroll(&body, 480);
        scroll.set_vexpand(true);
        content.append(&scroll);
        dialog.set_child(Some(&content));
        *self.0.history_view.borrow_mut() = Some(HistoryView {
            dialog: dialog.downgrade(),
            entries: entries.downgrade(),
            clear: clear.downgrade(),
        });
        // The view is weakly held and cleared when dismissed, so locale and
        // transfer updates never retain a closed dialog or rebuild stale rows.
        let weak = Rc::downgrade(&self.0);
        dialog.connect_closed(move |_| {
            if let Some(inner) = weak.upgrade() {
                inner.history_view.borrow_mut().take();
            }
        });
        self.refresh_history();
        dialog.present(Some(&self.0.window));
    }

    pub(super) fn refresh_history(&self) {
        let (body, clear) = {
            let view = self.0.history_view.borrow();
            let Some(view) = view.as_ref() else {
                return;
            };
            let (Some(body), Some(clear)) = (view.entries.upgrade(), view.clear.upgrade()) else {
                return;
            };
            (body, clear)
        };
        while let Some(child) = body.first_child() {
            body.remove(&child);
        }
        let entries = self.0.history.borrow().entries().to_vec();
        clear.set_sensitive(!entries.is_empty() && self.0.history_load_error.is_none());
        if entries.is_empty() {
            let empty = dialogs::translated_wrapped_label("The history is empty.", "section-title");
            empty.set_halign(gtk::Align::Center);
            margins(&empty, 30);
            body.append(&empty);
            return;
        }
        for entry in entries.iter().rev() {
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 4);
            row.add_css_class("device-row");
            let open = gtk::Button::new();
            open.set_widget_name(&entry.id);
            open.set_hexpand(true);
            open.add_css_class("flat");
            open.add_css_class("history-entry");
            let content = gtk::Box::new(gtk::Orientation::Horizontal, 12);
            let icon = gtk::Image::from_icon_name(match &entry.kind {
                HistoryKind::File { .. } => "ls-description-symbolic",
                HistoryKind::Message { .. } => "ls-subject-symbolic",
                HistoryKind::Legacy { .. } => "ls-history-symbolic",
            });
            icon.set_pixel_size(28);
            content.append(&icon);
            let labels = gtk::Box::new(gtk::Orientation::Vertical, 4);
            labels.set_hexpand(true);
            let title = label(entry_title(entry), "body-text");
            title.add_css_class("history-title");
            title.set_width_chars(1);
            title.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
            title.set_tooltip_text(Some(entry_title(entry)));
            labels.append(&title);
            let metadata = match &entry.kind {
                HistoryKind::File { size, sender, .. } => format!(
                    "{} · {} · {}",
                    entry_time(entry),
                    transfer::size_label(*size),
                    sender
                ),
                HistoryKind::Message { text, sender } => format!(
                    "{} · {} · {}",
                    entry_time(entry),
                    transfer::size_label(text.len() as u64),
                    sender
                ),
                HistoryKind::Legacy { .. } => String::new(),
            };
            if !metadata.is_empty() {
                let subtitle = label(&metadata, "secondary");
                subtitle.add_css_class("history-metadata");
                subtitle.set_width_chars(1);
                subtitle.set_ellipsize(gtk::pango::EllipsizeMode::End);
                labels.append(&subtitle);
            }
            content.append(&labels);
            open.set_child(Some(&content));
            let ui = self.clone();
            let selected = entry.clone();
            open.connect_clicked(move |_| ui.open_history_entry(&selected));
            if let HistoryKind::File { path, .. } = &entry.kind {
                let drag_path = path.clone();
                let drag_source = gtk::DragSource::new();
                drag_source.set_actions(gdk::DragAction::COPY);
                drag_source.connect_prepare(move |_, _, _| {
                    if drag_path.exists() {
                        let file = gio::File::for_path(&drag_path);
                        let file_list = gdk::FileList::from_array(&[file]);
                        Some(gdk::ContentProvider::for_value(&file_list.to_value()))
                    } else {
                        None
                    }
                });
                open.add_controller(drag_source);
            }
            row.append(&open);
            let menu = gtk::MenuButton::builder()
                .icon_name("view-more-symbolic")
                .valign(gtk::Align::Center)
                .build();
            menu.set_widget_name(&format!("history-options-{}", entry.id));
            menu.add_css_class("flat");
            i18n::bind_accessible_label(&menu, "Information");
            let popover = gtk::Popover::new();
            let options = gtk::Box::new(gtk::Orientation::Vertical, 4);
            for action in [
                "Open file",
                "Show in folder",
                "Information",
                "Delete from history",
            ] {
                if matches!(action, "Open file" | "Show in folder")
                    && !matches!(entry.kind, HistoryKind::File { .. })
                {
                    continue;
                }
                let button = i18n::button(action);
                button.add_css_class("flat");
                if action == "Delete from history" {
                    button.set_sensitive(self.0.history_load_error.is_none());
                }
                let ui = self.clone();
                let selected = entry.clone();
                let weak = popover.downgrade();
                button.connect_clicked(move |_| {
                    if let Some(popover) = weak.upgrade() {
                        popover.popdown();
                    }
                    match action {
                        "Open file" => ui.open_history_entry(&selected),
                        "Show in folder" => {
                            if let HistoryKind::File { path, .. } = &selected.kind {
                                ui.open_history_path(path.clone(), true);
                            }
                        }
                        "Information" => ui.show_history_info(&selected),
                        _ => ui.remove_history_entry(&selected.id),
                    }
                });
                options.append(&button);
            }
            popover.set_child(Some(&options));
            menu.set_popover(Some(&popover));
            row.append(&menu);
            body.append(&row);
        }
    }

    fn open_history_entry(&self, entry: &HistoryEntry) {
        match &entry.kind {
            HistoryKind::File { path, .. } => self.open_history_path(path.clone(), false),
            HistoryKind::Message { text, sender } => {
                self.show_received_message(text.clone(), sender.clone())
            }
            HistoryKind::Legacy { .. } => self.show_history_info(entry),
        }
    }

    fn open_history_path(&self, path: PathBuf, reveal: bool) {
        let launcher = gtk::FileLauncher::new(Some(&gio::File::for_path(path)));
        let ui = self.clone();
        glib::spawn_future_local(async move {
            let result = if reveal {
                launcher
                    .open_containing_folder_future(Some(&ui.0.window))
                    .await
            } else {
                launcher.launch_future(Some(&ui.0.window)).await
            };
            if let Err(error) = result {
                if !error.matches(gtk::DialogError::Dismissed) {
                    alert(
                        &ui.0.window,
                        if reveal {
                            "Could not open folder"
                        } else {
                            "Could not open file"
                        },
                        &error.to_string(),
                    );
                }
            }
        });
    }

    fn show_history_info(&self, entry: &HistoryEntry) {
        let dialog = translated_dialog(Some("File information"), None);
        let body = gtk::Box::new(gtk::Orientation::Vertical, 12);
        match &entry.kind {
            HistoryKind::File {
                name,
                path,
                size,
                sender,
            } => {
                detail(&body, "File name:", name);
                detail(&body, "Path:", &path.to_string_lossy());
                detail(&body, "Size:", &transfer::size_label(*size));
                detail(&body, "Sender:", sender);
                detail(&body, "Time:", &entry_time(entry));
            }
            HistoryKind::Message { text, sender } => {
                detail(&body, "Size:", &transfer::size_label(text.len() as u64));
                detail(&body, "Sender:", sender);
                detail(&body, "Time:", &entry_time(entry));
                let message = dialogs::wrapped_label(text, "message-preview");
                message.set_selectable(true);
                message.set_width_chars(1);
                body.append(&message);
            }
            HistoryKind::Legacy { text } => {
                let text = dialogs::wrapped_label(text, "body-text");
                text.set_width_chars(1);
                text.set_selectable(true);
                body.append(&text);
            }
        }
        dialog.set_extra_child(Some(&dialogs::content_scroll(&body, 400)));
        add_response(&dialog, "close", "Close");
        dialog.set_close_response("close");
        dialog.present(Some(&self.0.window));
    }

    fn remove_history_entry(&self, id: &str) {
        if self.0.history_load_error.is_some() {
            return;
        }
        let previous = self.0.history.borrow().clone();
        if self.0.history.borrow_mut().remove(id) && !self.persist_history() {
            *self.0.history.borrow_mut() = previous;
        }
        self.refresh_history();
    }

    fn confirm_clear_history(&self) {
        if self.0.history_load_error.is_some() {
            return;
        }
        let dialog = translated_dialog(
            Some("Clear history"),
            Some("Do you really want to delete the entire history?"),
        );
        add_responses(&dialog, &[("cancel", "Cancel"), ("delete", "Delete")]);
        dialog.set_close_response("cancel");
        dialog.set_default_response(Some("cancel"));
        dialog.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
        let ui = self.clone();
        dialog.connect_response(None, move |_, response| {
            if response != "delete" {
                return;
            }
            let previous = ui.0.history.borrow().clone();
            ui.0.history.borrow_mut().clear();
            if !ui.persist_history() {
                *ui.0.history.borrow_mut() = previous;
            }
            ui.refresh_history();
        });
        dialog.present(Some(&self.0.window));
    }
}
