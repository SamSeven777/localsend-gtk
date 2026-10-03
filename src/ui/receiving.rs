use super::dialogs::{content_scroll, wrapped_label};
use super::*;

fn message_link(text: &str) -> Option<String> {
    let text = text.trim();
    if text.is_empty() || text.chars().any(char::is_whitespace) {
        return None;
    }
    glib::Uri::parse(text, glib::UriFlags::NONE).ok()?;
    Some(text.to_owned())
}

fn message_dialog(text: &str, sender_alias: &str) -> (adw::AlertDialog, Option<String>) {
    let link = message_link(text);
    let dialog = adw::AlertDialog::new(Some(sender_alias), None);
    i18n::bind_property(
        &dialog,
        "body",
        if link.is_some() {
            "sent you a link:"
        } else {
            "sent you a message:"
        },
    );
    let message = wrapped_label(text, "message-preview");
    message.set_selectable(true);
    dialog.set_extra_child(Some(&content_scroll(&message, 300)));
    add_responses(&dialog, &[("close", "Close"), ("copy", "Copy")]);
    if link.is_some() {
        add_response(&dialog, "open", "Open");
        dialog.set_response_appearance("open", adw::ResponseAppearance::Suggested);
    }
    dialog.set_close_response("close");
    // Enter and Escape dismiss the message; opening a link always requires an
    // explicit Open action, even while a native protocol offer is still pending.
    dialog.set_default_response(Some("close"));
    (dialog, link)
}

impl Ui {
    pub(super) fn show_received_message(&self, text: String, sender_alias: String) {
        let (dialog, link) = message_dialog(&text, &sender_alias);
        let ui = self.clone();
        dialog.connect_response(None, move |_, response| {
            ui.message_action(response, &text, link.as_deref());
        });
        self.show_window();
        dialog.present(Some(&self.0.window));
    }

    fn message_action(&self, response: &str, text: &str, link: Option<&str>) {
        match response {
            "copy" => {
                self.0.window.clipboard().set_text(text);
                self.toast_text("Copied to Clipboard");
            }
            "open" => {
                if let Some(link) = link {
                    let launcher = gtk::UriLauncher::new(link);
                    let ui = self.clone();
                    glib::spawn_future_local(async move {
                        if let Err(error) = launcher.launch_future(Some(&ui.0.window)).await {
                            if !error.matches(gtk::DialogError::Dismissed) {
                                ui.toast(&i18n::tr_format(
                                    "Could not open the link: {error}",
                                    &[("error", error.to_string())],
                                ));
                            }
                        }
                    });
                }
            }
            _ => {}
        }
    }

    fn incoming_message(&self, request: network::IncomingRequest, text: String) {
        let sender_alias = request.sender().alias.clone();
        let notification_id = format!("incoming-message-{}", uuid::Uuid::new_v4());
        let (dialog, link) = message_dialog(&text, &sender_alias);
        let cancellation = request
            .cancellation()
            .expect("Native request has cancellation");
        let expired = Rc::new(Cell::new(false));
        let decided = CancellationToken::new();
        let pending = Rc::new(RefCell::new(Some(request)));
        self.0
            .incoming_pending
            .set(self.0.incoming_pending.get() + 1);
        if !self.0.offline_fixture {
            if let Some(app) = self.0.window.application() {
                let notif_title = format!(
                    "{} {}",
                    sender_alias,
                    i18n::tr("sent you a message:").trim_end_matches(':')
                );
                let notif = gio::Notification::new(&notif_title);
                let preview = if text.chars().count() > 80 {
                    format!("{}...", text.chars().take(80).collect::<String>())
                } else {
                    text.clone()
                };
                notif.set_body(Some(&preview));
                notif.set_default_action("app.show-window");
                app.send_notification(Some(&notification_id), &notif);
            }
        }
        {
            let ui = self.clone();
            let pending = pending.clone();
            let decided = decided.clone();
            let cancellation = cancellation.clone();
            let expired = expired.clone();
            let notification_id = notification_id.clone();
            dialog.connect_response(None, move |_, response| {
                decided.cancel();
                ui.withdraw_incoming_notification(&notification_id);
                let request = pending.borrow_mut().take();
                if let Some(request) = request {
                    ui.0.incoming_pending
                        .set(ui.0.incoming_pending.get().saturating_sub(1));
                    if expired.get() || cancellation.is_cancelled() {
                        request.decline();
                    } else if request.accept_preview() {
                        ui.message_action(response, &text, link.as_deref());
                    }
                }
            });
        }
        // AlertDialog emits `response` from its default `closed` handler,
        // including window/Escape dismissal. A normal `closed` callback runs
        // first and would consume the request before Copy/Open can accept it.
        let weak = dialog.downgrade();
        glib::spawn_future_local(async move {
            tokio::select! {
                _ = decided.cancelled() => return,
                _ = cancellation.cancelled() => {},
                _ = glib::timeout_future(std::time::Duration::from_secs(60)) => {},
            }
            expired.set(true);
            if let Some(dialog) = weak.upgrade() {
                dialog.close();
            }
        });
        self.show_window();
        dialog.present(Some(&self.0.window));
    }

    pub(super) fn cancel_incoming(&self) {
        if self.0.canceling_receive.get() {
            return;
        }
        let targets = self.0.receive_view.active_session_keys();
        let pending = self.0.incoming_pending.get();
        let generation = self.0.incoming_generation.get();
        if targets.is_empty() {
            return;
        }
        let dialog = translated_dialog(
            Some("Cancel files transfer"),
            Some("Do you really want to cancel the files transfer?"),
        );
        add_responses(&dialog, &[("continue", "Continue"), ("cancel", "Cancel")]);
        dialog.set_close_response("continue");
        dialog.set_default_response(Some("continue"));
        dialog.set_response_appearance("cancel", adw::ResponseAppearance::Destructive);
        let ui = self.clone();
        dialog.connect_response(None, move |_, response| {
            if response != "cancel" {
                return;
            }
            if ui.0.receive_view.active_session_keys() == targets
                && ui.0.incoming_pending.get() == pending
                && ui.0.incoming_generation.get() == generation
            {
                // Consent may outlive this transfer. A replacement session
                // requires its own cancellation confirmation.
                ui.cancel_incoming_confirmed();
            } else {
                ui.toast_text("The incoming transfers changed. Review them before canceling.");
            }
        });
        dialog.present(Some(&self.0.window));
    }

    fn cancel_incoming_confirmed(&self) {
        if self.0.canceling_receive.replace(true) {
            return;
        }
        self.0.cancel_receive.set_sensitive(false);
        self.0.receive_view.mark_canceling();
        if self.0.offline_fixture {
            self.incoming_canceled();
        } else if self.0.commands.try_send(Command::CancelIncoming).is_err() {
            self.0.canceling_receive.set(false);
            self.0.receive_view.cancel_failed();
            self.toast_text("The receiver is not running.");
        }
    }

    pub(super) fn incoming_canceled(&self) {
        self.0.canceling_receive.set(false);
        self.0.receive_view.cancel_acknowledged();
        self.toast_text("Incoming transfers canceled. Completed files were kept.");
    }

    pub(super) fn incoming_offer(&self, request: network::IncomingRequest) {
        self.incoming_offer_with_timeout(request, std::time::Duration::from_secs(60));
    }

    pub(super) fn incoming_offer_with_timeout(
        &self,
        request: network::IncomingRequest,
        timeout: std::time::Duration,
    ) {
        self.0
            .incoming_generation
            .set(self.0.incoming_generation.get().wrapping_add(1));
        if self.0.canceling_receive.get() {
            request.decline();
            return;
        }
        if let Some(text) = request.inline_message().map(str::to_owned) {
            self.incoming_message(request, text);
            return;
        }
        self.0
            .incoming_pending
            .set(self.0.incoming_pending.get() + 1);
        let total = request
            .files()
            .values()
            .fold(0_u64, |total, file| total.saturating_add(file.size));
        let dialog = adw::AlertDialog::new(Some(&request.sender().alias), None);
        // UUIDs also prevent notifications retained across application restarts
        // from accepting an unrelated, newer offer.
        let notification_id = format!("incoming-transfer-{}", uuid::Uuid::new_v4());
        self.0
            .incoming_dialogs
            .borrow_mut()
            .insert(notification_id.clone(), dialog.downgrade());
        if !self.0.offline_fixture {
            if let Some(app) = self.0.window.application() {
                let count = request.files().len();
                let notif_title = format!("{} · LocalSend", request.sender().alias);
                let size_str = transfer::size_label(total);
                let notif_body = i18n::tr_plural(
                    "wants to send you a file · {size}",
                    "wants to send you {n} files · {size}",
                    count as u64,
                    &[("size", size_str)],
                );
                let notif = gio::Notification::new(&notif_title);
                notif.set_body(Some(&notif_body));
                let target = notification_id.to_variant();
                notif.add_button_with_target_value(
                    &i18n::tr("Accept"),
                    "app.accept-transfer",
                    Some(&target),
                );
                notif.add_button_with_target_value(
                    &i18n::tr("Decline"),
                    "app.decline-transfer",
                    Some(&target),
                );
                notif.set_default_action("app.show-window");
                app.send_notification(Some(&notification_id), &notif);
            }
        }
        i18n::bind_plural_property(
            &dialog,
            "body",
            "wants to send you a file · {size}",
            "wants to send you {n} files · {size}",
            request.files().len() as u64,
            &[("size", transfer::size_label(total))],
        );
        let files = gtk::Box::new(gtk::Orientation::Vertical, 8);
        let mut offered: Vec<_> = request.files().values().collect();
        offered.sort_by(|a, b| a.file_name.cmp(&b.file_name));
        let mut choices = Vec::new();
        for file in offered {
            let check = gtk::CheckButton::new();
            check.set_active(true);
            let row = gtk::Box::new(gtk::Orientation::Vertical, 4);
            row.set_hexpand(true);
            let title = label(&file.file_name, "body-text");
            title.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
            title.set_hexpand(true);
            title.set_tooltip_text(Some(&file.file_name));
            row.append(&title);
            row.append(&label(&transfer::size_label(file.size), "secondary"));
            check.set_child(Some(&row));
            files.append(&check);
            choices.push((file.id.clone(), check));
            if let Some(text) = file
                .preview
                .as_ref()
                .filter(|_| file.file_type.starts_with("text/"))
            {
                let preview = wrapped_label(text, "message-preview");
                preview.set_selectable(true);
                margins(&preview, 12);
                files.append(&preview);
            }
        }
        let scroll = content_scroll(&files, 280);
        dialog.set_extra_child(Some(&scroll));
        add_responses(&dialog, &[("decline", "Decline"), ("accept", "Accept")]);
        dialog.set_response_appearance("accept", adw::ResponseAppearance::Suggested);
        dialog.set_close_response("decline");
        let checks = Rc::new(
            choices
                .iter()
                .map(|(_, check)| check.downgrade())
                .collect::<Vec<_>>(),
        );
        for (_, check) in &choices {
            let checks = checks.clone();
            let weak = dialog.downgrade();
            check.connect_toggled(move |_| {
                if let Some(dialog) = weak.upgrade() {
                    dialog.set_response_enabled(
                        "accept",
                        checks
                            .iter()
                            .filter_map(|check| check.upgrade())
                            .any(|check| check.is_active()),
                    );
                }
            });
        }
        let cancellation = request.cancellation().unwrap_or_default();
        let expired = Rc::new(Cell::new(false));
        let decided = CancellationToken::new();
        let pending = Rc::new(RefCell::new(Some(request)));
        {
            let ui = self.clone();
            let pending = pending.clone();
            let notification_id = notification_id.clone();
            let decided = decided.clone();
            let cancellation = cancellation.clone();
            let expired = expired.clone();
            dialog.connect_response(None, move |_, response| {
                decided.cancel();
                ui.withdraw_incoming_notification(&notification_id);
                let request = pending.borrow_mut().take();
                let Some(request) = request else {
                    return;
                };
                ui.0.incoming_pending
                    .set(ui.0.incoming_pending.get().saturating_sub(1));
                let ids = choices
                    .iter()
                    .filter(|(_, check)| check.is_active())
                    .map(|(id, _)| id.clone())
                    .collect::<Vec<_>>();
                if response == "accept"
                    && !ids.is_empty()
                    && !expired.get()
                    && !cancellation.is_cancelled()
                {
                    ui.0.stack.set_visible_child_name("receive");
                    let files = ids
                        .iter()
                        .filter_map(|id| request.files().get(id))
                        .map(|file| {
                            receive_progress::ReceiveFile::new(
                                file.id.to_string(),
                                file.file_name.clone(),
                                file.size,
                            )
                        })
                        .collect();
                    ui.0.receive_view.begin_offer(
                        request.progress_identity(),
                        request.sender().alias.clone(),
                        Some(ui.0.settings.borrow().save_dir.clone()),
                        files,
                    );
                    if ids.len() == request.files().len() {
                        request.accept();
                    } else {
                        request.accept_files(ids);
                    }
                } else {
                    request.decline();
                }
            });
        }
        // Handle every decision through `response`, including the configured
        // close response. Handling `closed` separately would decline the offer
        // before libadwaita emits the user's Accept response.
        let weak = dialog.downgrade();
        glib::spawn_future_local(async move {
            tokio::select! {
                biased;
                _ = decided.cancelled() => return,
                _ = cancellation.cancelled() => {},
                _ = glib::timeout_future(timeout) => {},
            }
            expired.set(true);
            if let Some(dialog) = weak.upgrade() {
                dialog.close();
            }
        });
        self.show_window();
        dialog.present(Some(&self.0.window));
    }

    pub(super) fn apply_network_settings(&self) {
        self.restart_server();
    }
}
