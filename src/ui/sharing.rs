use super::dialogs::content_scroll;
use super::dialogs::translated_wrapped_label;
use super::*;
use crate::web_share::{PendingDownload, ShareEvent};

pub(super) struct ShareDialog {
    pub dialog: adw::AlertDialog,
    links: gtk::Box,
    status: gtk::Label,
    progress: gtk::ProgressBar,
    active: RefCell<HashMap<String, (String, u64, u64)>>,
    auto_accept: gtk::CheckButton,
}

impl Ui {
    pub(super) fn set_share_auto_accept(&self, enabled: bool) -> bool {
        let previous = self.0.settings.borrow().share_auto_accept;
        if previous == enabled {
            return true;
        }
        self.0.settings.borrow_mut().share_auto_accept = enabled;
        if !self.save_settings() {
            self.0.settings.borrow_mut().share_auto_accept = previous;
            return false;
        }
        if !self.0.offline_fixture {
            let _ = self
                .0
                .commands
                .try_send(Command::SetWebShareAutoAccept(enabled));
        }
        let control = self
            .0
            .share
            .borrow()
            .as_ref()
            .map(|share| share.auto_accept.clone());
        if let Some(control) = control {
            control.set_active(enabled);
        }
        let row = self.0.share_auto_row.borrow().clone();
        if let Some(row) = row {
            row.set_active(enabled);
        }
        true
    }

    pub(super) fn show_web_share(&self) {
        if self.0.share_stopping.get() {
            self.toast_text("The previous share is closing. Try again in a moment.");
            return;
        }
        if let Some(share) = self.0.share.borrow().as_ref() {
            share.dialog.present(Some(&self.0.window));
            return;
        }
        let items = self.0.selection.borrow().clone();
        if items.is_empty() {
            let ui = self.clone();
            self.after_selection(move || ui.show_web_share());
            return;
        }
        let total = items
            .iter()
            .fold(0_u64, |total, item| total.saturating_add(item.size));
        let dialog = translated_dialog(
            Some("Share via link"),
            Some("Open this link in your browser:"),
        );
        let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
        let summary = label("", "section-title");
        i18n::bind_format_property(
            &summary,
            "label",
            "Files: {n}\nSize: {size}",
            &[
                ("n", items.len().to_string()),
                ("size", transfer::size_label(total)),
            ],
        );
        content.append(&summary);
        let links = gtk::Box::new(gtk::Orientation::Vertical, 12);
        links.append(&translated_label("Starting server…", "body-text"));
        content.append(&links);
        content.append(&translated_label("Requests", "section-title"));
        let status = translated_wrapped_label("No requests yet.", "secondary");
        let progress = gtk::ProgressBar::new();
        progress.set_visible(false);
        content.append(&status);
        content.append(&progress);
        let auto_accept = gtk::CheckButton::with_label("Automatically accept requests");
        i18n::bind_property(&auto_accept, "label", "Automatically accept requests");
        if let Some(text) = auto_accept.child().and_downcast::<gtk::Label>() {
            text.set_wrap(true);
            text.set_wrap_mode(gtk::pango::WrapMode::WordChar);
            text.set_max_width_chars(28);
        }
        auto_accept.set_active(self.0.settings.borrow().share_auto_accept);
        {
            let ui = self.clone();
            auto_accept.connect_toggled(move |check| {
                if !ui.set_share_auto_accept(check.is_active()) {
                    let previous = ui.0.settings.borrow().share_auto_accept;
                    check.set_active(previous);
                }
            });
        }
        content.append(&auto_accept);
        content.append(&translated_wrapped_label(
            "Allow anyone with the link to download. Requests already waiting still need approval.",
            "secondary",
        ));
        content.append(&translated_wrapped_label(
            "Devices must be on the same network. Keep the link private. Browser transfers use unencrypted HTTP. Closing this dialog stops sharing.",
            "secondary",
        ));
        let scroll = content_scroll(&content, 420);
        dialog.set_extra_child(Some(&scroll));
        add_response(&dialog, "stop", "Close");
        dialog.set_close_response("stop");
        let ui = self.clone();
        dialog.connect_response(None, move |_, _| {
            if ui.0.share.borrow_mut().take().is_some() && !ui.0.offline_fixture {
                ui.0.share_stopping.set(true);
                if ui.0.commands.try_send(Command::StopWebShare).is_err() {
                    ui.0.share_stopping.set(false);
                }
            }
        });
        *self.0.share.borrow_mut() = Some(ShareDialog {
            dialog: dialog.clone(),
            links,
            status,
            progress,
            active: RefCell::new(HashMap::new()),
            auto_accept,
        });
        let auto_accept = self.0.settings.borrow().share_auto_accept;
        if !self.0.offline_fixture
            && self
                .0
                .commands
                .try_send(Command::StartWebShare { items, auto_accept })
                .is_err()
        {
            self.web_share_stopped();
            self.toast_text("The sharing service is not running.");
            return;
        }
        dialog.present(Some(&self.0.window));
    }

    pub(super) fn web_share_ready(&self, links: Vec<String>) {
        let share = self.0.share.borrow();
        let Some(share) = share.as_ref() else {
            let _ = self.0.commands.try_send(Command::StopWebShare);
            return;
        };
        while let Some(child) = share.links.first_child() {
            share.links.remove(&child);
        }
        i18n::bind_property(
            &share.dialog,
            "body",
            if links.len() == 1 {
                "Open this link in your browser:"
            } else {
                "Open one of these links in your browser:"
            },
        );
        share.links.append(&self.browser_links(links));
        i18n::bind_property(&share.status, "label", "No requests yet.");
    }

    pub(super) fn web_share_stopped(&self) {
        self.0.share_stopping.set(false);
        let share = self.0.share.borrow_mut().take();
        if let Some(share) = share {
            share.dialog.force_close();
        }
    }

    pub(super) fn web_share_event(&self, event: ShareEvent) {
        if let ShareEvent::DownloadRequest(request) = event {
            self.approve_browser_download(request);
            return;
        }
        let share = self.0.share.borrow();
        let Some(share) = share.as_ref() else {
            return;
        };
        let mut active = share.active.borrow_mut();
        match event {
            ShareEvent::Started {
                id,
                file_name,
                total_bytes,
            } => {
                active.insert(id, (file_name, 0, total_bytes));
            }
            ShareEvent::Progress {
                id,
                bytes_sent,
                total_bytes,
            } => {
                if let Some(entry) = active.get_mut(&id) {
                    entry.1 = bytes_sent;
                    entry.2 = total_bytes;
                }
            }
            ShareEvent::Finished { id, file_name, .. } => {
                active.remove(&id);
                i18n::bind_format_property(
                    &share.status,
                    "label",
                    "Served {name}. Check the browser's downloads for completion.",
                    &[("name", file_name)],
                );
            }
            ShareEvent::Failed {
                id,
                file_name,
                error,
            } => {
                active.remove(&id);
                i18n::bind_format_property(
                    &share.status,
                    "label",
                    "{name}: {error}",
                    &[("name", file_name), ("error", error)],
                );
            }
            ShareEvent::DownloadRequest(_) => unreachable!(),
        }
        share.progress.set_visible(!active.is_empty());
        if !active.is_empty() {
            let (sent, total) =
                active
                    .values()
                    .fold((0_u64, 0_u64), |(sent, total), (_, bytes, size)| {
                        (sent.saturating_add(*bytes), total.saturating_add(*size))
                    });
            share.progress.set_fraction(if total == 0 {
                0.0
            } else {
                sent as f64 / total as f64
            });
            i18n::bind_plural_property(
                &share.status,
                "label",
                "{n} active download · {sent} / {total}",
                "{n} active downloads · {sent} / {total}",
                active.len() as u64,
                &[
                    ("sent", transfer::size_label(sent)),
                    ("total", transfer::size_label(total)),
                ],
            );
        }
    }

    fn approve_browser_download(&self, request: PendingDownload) {
        if self.0.share.borrow().is_none() {
            request.decline();
            return;
        }
        let total = request
            .files()
            .iter()
            .fold(0_u64, |total, file| total.saturating_add(file.size));
        let dialog = translated_dialog(Some("Download request"), None);
        i18n::bind_plural_property(
            &dialog,
            "body",
            "{name} wants to download a file · {size}",
            "{name} wants to download {n} files · {size}",
            request.files().len() as u64,
            &[
                ("name", request.ip().to_string()),
                ("size", transfer::size_label(total)),
            ],
        );
        let list = gtk::Box::new(gtk::Orientation::Vertical, 8);
        for file in request.files() {
            let name = label(&file.name, "body-text");
            name.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
            name.set_hexpand(true);
            name.set_tooltip_text(Some(&file.name));
            list.append(&name);
            list.append(&label(&transfer::size_label(file.size), "secondary"));
        }
        let scroll = content_scroll(&list, 240);
        dialog.set_extra_child(Some(&scroll));
        add_responses(&dialog, &[("decline", "Decline"), ("allow", "Accept")]);
        dialog.set_response_appearance("allow", adw::ResponseAppearance::Suggested);
        dialog.set_close_response("decline");
        let cancellation = request.cancellation();
        let decided = CancellationToken::new();
        let signal = decided.clone();
        let pending = RefCell::new(Some(request));
        self.0
            .incoming_pending
            .set(self.0.incoming_pending.get() + 1);
        let ui = self.clone();
        dialog.connect_response(None, move |_, response| {
            signal.cancel();
            if let Some(request) = pending.borrow_mut().take() {
                ui.0.incoming_pending
                    .set(ui.0.incoming_pending.get().saturating_sub(1));
                if response == "allow" {
                    if let Some(share) = ui.0.share.borrow().as_ref() {
                        i18n::bind_format_property(
                            &share.status,
                            "label",
                            "{name} · Accepted",
                            &[("name", request.ip().to_string())],
                        );
                    }
                    request.accept();
                } else {
                    request.decline();
                }
            }
        });
        let weak = dialog.downgrade();
        glib::spawn_future_local(async move {
            tokio::select! {
                _ = cancellation.cancelled() => if let Some(dialog) = weak.upgrade() { dialog.close(); },
                _ = decided.cancelled() => {},
            }
        });
        self.show_window();
        dialog.present(Some(&self.0.window));
    }
}
