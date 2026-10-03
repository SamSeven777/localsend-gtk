//! The official Selection → Edit/Add workflow, using native GTK widgets.
use super::*;

pub(super) type SelectionContinuation = (u64, Box<dyn FnOnce()>);

fn thumbnail(item: &Selection) -> gtk::Box {
    let frame = gtk::Box::new(gtk::Orientation::Vertical, 0);
    frame.add_css_class("file-thumbnail");
    frame.set_size_request(50, 50);
    frame.set_hexpand(false);
    frame.set_vexpand(false);
    frame.set_halign(gtk::Align::Start);
    frame.set_valign(gtk::Align::Center);
    frame.set_tooltip_text(Some(&format!(
        "{} · {}",
        item.name,
        transfer::size_label(item.size)
    )));
    let image = gtk::Image::from_icon_name(if matches!(item.source, Source::Text(_)) {
        "ls-subject-symbolic"
    } else {
        "ls-description-symbolic"
    });
    image.set_pixel_size(32);
    image.set_size_request(50, 50);
    frame.append(&image);
    if let Source::File(path) = &item.source {
        let (content_type, _) = gio::content_type_guess(Some(path), &[]);
        if content_type.starts_with("image/") {
            let file = gio::File::for_path(path);
            let weak = image.downgrade();
            glib::spawn_future_local(async move {
                if let Ok(stream) = file.read_future(glib::Priority::DEFAULT).await {
                    if let Ok(pixbuf) =
                        gdk_pixbuf::Pixbuf::from_stream_at_scale_future(&stream, 50, 50, true).await
                    {
                        if let Some(image) = weak.upgrade() {
                            image.set_paintable(Some(&gdk::Texture::for_pixbuf(&pixbuf)));
                        }
                    }
                    let _ = stream.close_future(glib::Priority::DEFAULT).await;
                }
            });
        }
    }
    frame
}

impl Ui {
    pub(super) fn refresh_selection(&self) {
        self.0
            .selection_generation
            .set(self.0.selection_generation.get().wrapping_add(1));
        while let Some(child) = self.0.files.first_child() {
            self.0.files.remove(&child);
        }
        let items = self.0.selection.borrow();
        let has_files = !items.is_empty();
        self.0.count.set_visible(has_files);
        self.0.clear.set_visible(has_files);
        self.0.file_strip.set_visible(has_files);
        self.0.selection_actions.set_visible(has_files);
        self.0.picker_choices.set_visible(!has_files);
        if has_files {
            self.0.selection_card.add_css_class("selection-card");
        } else {
            self.0.selection_card.remove_css_class("selection-card");
        }
        i18n::bind_format_property(
            &self.0.count,
            "label",
            "Files: {n}\nSize: {size}",
            &[
                ("n", items.len().to_string()),
                (
                    "size",
                    transfer::size_label(items.iter().map(|s| s.size).sum()),
                ),
            ],
        );
        for item in items.iter() {
            self.0.files.append(&thumbnail(item));
        }
        drop(items);
        self.refresh_send_controls();
    }

    pub(super) fn pending_selection_id(&self) -> Option<u64> {
        self.0
            .pending_selection
            .borrow()
            .as_ref()
            .map(|(id, _)| *id)
    }

    /// Standalone selection changes have no continuation. A linked operation
    /// must still own its request before it changes or resumes the selection.
    pub(super) fn selection_request_is_current(&self, request: Option<u64>) -> bool {
        request.is_none() || request == self.pending_selection_id()
    }

    pub(super) fn cancel_selection_request(&self, request: Option<u64>) {
        if request.is_some() && request == self.pending_selection_id() {
            self.0.pending_selection.borrow_mut().take();
        }
    }

    pub(super) fn resume_selection_request(&self, request: Option<u64>) {
        if request.is_none()
            || request != self.pending_selection_id()
            || self.0.selection.borrow().is_empty()
        {
            return;
        }
        let continuation = self.0.pending_selection.borrow_mut().take();
        if let Some((_, continuation)) = continuation {
            continuation();
        }
    }

    /// Device/link actions resume once after the user supplies a selection.
    pub(super) fn after_selection(&self, action: impl FnOnce() + 'static) {
        if self.0.selection.borrow().is_empty() {
            let id = self.0.selection_request_counter.get().wrapping_add(1);
            self.0.selection_request_counter.set(id);
            *self.0.pending_selection.borrow_mut() = Some((id, Box::new(action)));
            self.show_add_selection_for(Some(id));
        } else {
            // An immediate action supersedes any still-collecting older one.
            self.0.pending_selection.borrow_mut().take();
            action();
        }
    }

    pub(super) fn show_add_selection(&self) {
        self.cancel_selection_request(self.pending_selection_id());
        self.show_add_selection_for(None);
    }

    fn show_add_selection_for(&self, request: Option<u64>) {
        let dialog = translated_dialog(Some("Add to selection"), Some("What do you want to add?"));
        let choices = gtk::FlowBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .homogeneous(true)
            .min_children_per_line(2)
            .max_children_per_line(2)
            .column_spacing(10)
            .row_spacing(10)
            .build();
        let chosen = Rc::new(Cell::new(false));
        for (index, (title, icon)) in [
            ("File", "ls-description-symbolic"),
            ("Folder", "ls-folder-symbolic"),
            ("Text", "ls-subject-symbolic"),
            ("Paste", "ls-content-paste-symbolic"),
        ]
        .into_iter()
        .enumerate()
        {
            let button = gtk::Button::new();
            button.add_css_class("picker-button");
            i18n::bind_property(&button, "tooltip-text", title);
            i18n::bind_accessible_label(&button, title);
            button.set_child(Some(&button_content(icon, title, true)));
            let ui = self.clone();
            let dialog = dialog.clone();
            let chosen = chosen.clone();
            button.connect_clicked(move |_| {
                chosen.set(true);
                dialog.force_close();
                if !ui.selection_request_is_current(request) {
                    return;
                }
                match index {
                    0 => ui.pick_for(false, request),
                    1 => ui.pick_for(true, request),
                    2 => ui.text_dialog_for(request),
                    _ => ui.paste_for(request),
                }
            });
            choices.insert(&button, -1);
        }
        dialog.set_extra_child(Some(&choices));
        add_response(&dialog, "close", "Close");
        dialog.set_close_response("close");
        let ui = self.clone();
        dialog.connect_closed(move |_| {
            if !chosen.get() {
                ui.cancel_selection_request(request);
            }
        });
        dialog.present(Some(&self.0.window));
    }

    pub(super) fn show_selection_editor(&self) {
        let dialog = adw::Dialog::builder()
            .title("Selection")
            .content_width(600)
            .content_height(500)
            .build();
        i18n::bind_property(&dialog, "title", "Selection");
        let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
        content.append(&adw::HeaderBar::new());
        let body = gtk::Box::new(gtk::Orientation::Vertical, 12);
        margins(&body, 15);
        content.append(&scrolled(&body));
        dialog.set_child(Some(&content));
        self.populate_selection_editor(&body, &dialog);
        dialog.present(Some(&self.0.window));
    }

    fn populate_selection_editor(&self, body: &gtk::Box, dialog: &adw::Dialog) {
        while let Some(child) = body.first_child() {
            body.remove(&child);
        }
        let summary = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        let count = label(&self.0.count.label(), "body-text");
        self.0
            .count
            .bind_property("label", &count, "label")
            .sync_create()
            .build();
        count.set_hexpand(true);
        summary.append(&count);
        let clear = i18n::button("Delete all");
        clear.add_css_class("suggested-action");
        clear.set_valign(gtk::Align::Center);
        {
            let ui = self.clone();
            let weak = dialog.downgrade();
            clear.connect_clicked(move |_| {
                ui.0.selection.borrow_mut().clear();
                ui.refresh_selection();
                if let Some(dialog) = weak.upgrade() {
                    dialog.close();
                }
            });
        }
        summary.append(&clear);
        body.append(&summary);
        let generation = self.0.selection_generation.get();
        for (index, item) in self.0.selection.borrow().iter().enumerate() {
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            row.add_css_class("selection-item");
            row.append(&thumbnail(item));
            let details = gtk::Box::new(gtk::Orientation::Vertical, 4);
            details.set_hexpand(true);
            details.set_valign(gtk::Align::Center);
            let text = match &item.source {
                Source::Text(text) => format!("\"{}\"", text.replace('\n', " ")),
                Source::File(_) => item.name.clone(),
            };
            let name = label(&text, "body-text");
            name.set_ellipsize(gtk::pango::EllipsizeMode::End);
            name.set_tooltip_text(Some(&text));
            details.append(&name);
            details.append(&label(&transfer::size_label(item.size), "secondary"));
            row.append(&details);
            match &item.source {
                Source::Text(text) => {
                    let edit = icon_button("document-edit-symbolic", "Edit message");
                    let text = text.clone();
                    let ui = self.clone();
                    let weak = dialog.downgrade();
                    edit.connect_clicked(move |_| {
                        if let Some(dialog) = weak.upgrade() {
                            dialog.force_close();
                        }
                        ui.edit_selected_message(index, generation, &text);
                    });
                    row.append(&edit);
                }
                Source::File(path) => {
                    let open = icon_button("document-open-symbolic", "Open file");
                    let launcher = gtk::FileLauncher::new(Some(&gio::File::for_path(path)));
                    let ui = self.clone();
                    open.connect_clicked(move |_| {
                        let launcher = launcher.clone();
                        let ui = ui.clone();
                        glib::spawn_future_local(async move {
                            if let Err(error) = launcher.launch_future(Some(&ui.0.window)).await {
                                if !error.matches(gtk::DialogError::Dismissed) {
                                    alert(&ui.0.window, "Could not open file", &error.to_string());
                                }
                            }
                        });
                    });
                    row.append(&open);
                }
            }
            let remove = icon_button("user-trash-symbolic", "Remove");
            i18n::bind_format_property(
                &remove,
                "tooltip-text",
                "Remove {name}",
                &[("name", item.name.clone())],
            );
            let ui = self.clone();
            let weak_dialog = dialog.downgrade();
            let weak_body = body.downgrade();
            remove.connect_clicked(move |_| {
                if ui.0.selection_generation.get() == generation {
                    ui.0.selection.borrow_mut().remove(index);
                    ui.refresh_selection();
                }
                if let (Some(dialog), Some(body)) = (weak_dialog.upgrade(), weak_body.upgrade()) {
                    if ui.0.selection.borrow().is_empty() {
                        dialog.close();
                    } else {
                        ui.populate_selection_editor(&body, &dialog);
                    }
                }
            });
            row.append(&remove);
            body.append(&row);
        }
    }

    fn edit_selected_message(&self, index: usize, generation: u64, value: &str) {
        let dialog = translated_dialog(Some("Type message"), None);
        let text = gtk::TextView::new();
        text.set_wrap_mode(gtk::WrapMode::WordChar);
        text.buffer().set_text(value);
        margins(&text, 12);
        let scroll = scrolled(&text);
        scroll.set_min_content_height(180);
        dialog.set_extra_child(Some(&scroll));
        add_responses(&dialog, &[("cancel", "Cancel"), ("confirm", "Confirm")]);
        dialog.set_response_appearance("confirm", adw::ResponseAppearance::Suggested);
        dialog.set_close_response("cancel");
        dialog.set_response_enabled("confirm", !value.trim().is_empty());
        let weak_dialog = dialog.downgrade();
        text.buffer().connect_changed(move |buffer| {
            if let Some(dialog) = weak_dialog.upgrade() {
                let value = buffer.text(&buffer.start_iter(), &buffer.end_iter(), false);
                dialog.set_response_enabled("confirm", !value.trim().is_empty());
            }
        });
        let ui = self.clone();
        dialog.connect_response(None, move |_, response| {
            if response == "confirm" && ui.0.selection_generation.get() == generation {
                let buffer = text.buffer();
                let value = buffer
                    .text(&buffer.start_iter(), &buffer.end_iter(), false)
                    .to_string();
                if !value.trim().is_empty() {
                    ui.0.selection.borrow_mut()[index] = Selection::text(value);
                    ui.refresh_selection();
                }
            }
            if !ui.0.selection.borrow().is_empty() {
                ui.show_selection_editor();
            }
        });
        dialog.present(Some(&self.0.window));
    }
}
