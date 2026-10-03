use super::*;
use crate::i18n::{self, tr, tr_format};
use crate::transfer::{FileOutcome, TransferProgress};

enum OutgoingStatus {
    Waiting,
    Sending(String),
    Canceling,
    Finished { sent: usize, total: usize },
    Error(transfer::SendError),
}

impl OutgoingStatus {
    fn text(&self) -> String {
        match self {
            Self::Waiting => tr("Waiting for response…"),
            Self::Sending(name) => tr_format("Sending {name}", &[("name", name.clone())]),
            Self::Canceling => tr("Canceling transfer…"),
            Self::Finished { sent, total } if sent == total => tr("Finished"),
            Self::Finished { sent, total } => tr_format(
                "Finished · {sent} of {total} files sent",
                &[("sent", sent.to_string()), ("total", total.to_string())],
            ),
            Self::Error(error) => match error {
                transfer::SendError::Cancelled => tr("Canceled by sender"),
                transfer::SendError::Declined => tr("Declined"),
                transfer::SendError::RecipientBusy => tr("Busy"),
                transfer::SendError::TooManyAttempts => tr("Too many attempts"),
                transfer::SendError::PinRequired => tr("The recipient requires a valid PIN."),
                transfer::SendError::Failed(message) => message.clone(),
            },
        }
    }
}

fn duration_label(duration: std::time::Duration) -> String {
    let seconds = duration.as_secs();
    if seconds >= 3600 {
        format!(
            "{}:{:02}:{:02}",
            seconds / 3600,
            seconds / 60 % 60,
            seconds % 60
        )
    } else {
        format!("{}:{:02}", seconds / 60, seconds % 60)
    }
}

pub(super) struct OutgoingTransfer {
    id: uuid::Uuid,
    alias: String,
    status: OutgoingStatus,
    fraction: f64,
    report: TransferProgress,
    active: bool,
    background: bool,
    cancellation: CancellationToken,
    selection_generation: u64,
    item_count: usize,
    updated: std::time::Instant,
    details: Option<OutgoingDetails>,
}

pub(super) struct OutgoingRow {
    pub container: gtk::Box,
    pub status: gtk::Label,
    pub progress: gtk::ProgressBar,
    pub cancel: gtk::Button,
}

struct OutgoingDetails {
    dialog: adw::AlertDialog,
    status: gtk::Label,
    progress: gtk::ProgressBar,
    current_box: gtk::Box,
    current_size: gtk::Label,
    current_progress: gtk::ProgressBar,
    count: gtk::Label,
    size: gtk::Label,
    speed: gtk::Label,
    elapsed: gtk::Label,
    remaining: gtk::Label,
    files: Vec<OutgoingFileRow>,
    done_response: Cell<bool>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct FileRowState {
    bytes_sent: u64,
    size: u64,
    outcome: FileOutcome,
    locale: i18n::Locale,
}

struct OutgoingFileRow {
    size: gtk::Label,
    status: gtk::Label,
    progress: gtk::ProgressBar,
    rendered: Cell<Option<FileRowState>>,
}

impl OutgoingDetails {
    fn refresh(&self, transfer: &OutgoingTransfer) {
        let report = &transfer.report;
        self.status.set_label(&transfer.status.text());
        self.progress.set_fraction(transfer.fraction);
        let current = report
            .current_file
            .and_then(|index| report.files.get(index));
        self.current_box
            .set_visible(transfer.active && current.is_some());
        if let Some(file) = current {
            self.current_size.set_label(&tr_format(
                "Size: {sent} / {total}",
                &[
                    ("sent", transfer::size_label(file.bytes_sent)),
                    ("total", transfer::size_label(file.size)),
                ],
            ));
            self.current_progress.set_fraction(
                if file.size > 0 {
                    file.bytes_sent as f64 / file.size as f64
                } else if file.outcome == FileOutcome::Finished {
                    1.0
                } else {
                    0.0
                }
                .clamp(0.0, 1.0),
            );
        }
        self.count.set_label(&tr_format(
            "Files: {finished} / {total}",
            &[
                ("finished", report.finished_files.to_string()),
                ("total", report.accepted_files.to_string()),
            ],
        ));
        self.size.set_label(&tr_format(
            "Size: {sent} / {total}",
            &[
                ("sent", transfer::size_label(report.bytes_sent)),
                ("total", transfer::size_label(report.total_bytes)),
            ],
        ));
        self.speed.set_label(&tr_format(
            "Speed: {speed}/s",
            &[(
                "speed",
                transfer::size_label(report.bytes_per_second.max(0.0) as u64),
            )],
        ));
        self.elapsed.set_label(&tr_format(
            "Elapsed: {time}",
            &[("time", duration_label(report.elapsed))],
        ));
        self.remaining.set_label(&tr_format(
            "Remaining: {time}",
            &[(
                "time",
                report
                    .remaining
                    .map(duration_label)
                    .unwrap_or_else(|| "—".into()),
            )],
        ));
        let locale = i18n::current();
        for (row, file) in self.files.iter().zip(&report.files) {
            let state = FileRowState {
                bytes_sent: file.bytes_sent,
                size: file.size,
                outcome: file.outcome,
                locale,
            };
            if row.rendered.get() == Some(state) {
                continue;
            }
            row.rendered.set(Some(state));
            // The weak locale listener refreshes this semantic data on language changes.
            // Avoid registering thousands of bindings again for each progress sample.
            row.size.set_label(&tr_format(
                "Size: {sent} / {total}",
                &[
                    ("sent", transfer::size_label(file.bytes_sent)),
                    ("total", transfer::size_label(file.size)),
                ],
            ));
            row.status.set_label(&tr(match file.outcome {
                FileOutcome::Queued => "Queue",
                FileOutcome::Skipped => "Skipped",
                FileOutcome::Sending => "Sending",
                FileOutcome::Finished => "Done",
                FileOutcome::Failed => "Error",
                FileOutcome::Canceled => "Canceled by sender",
            }));
            row.progress
                .set_visible(file.outcome == FileOutcome::Sending);
            row.progress.set_fraction(
                if file.size > 0 {
                    file.bytes_sent as f64 / file.size as f64
                } else {
                    0.0
                }
                .clamp(0.0, 1.0),
            );
        }
    }
}

impl Ui {
    pub(super) fn send_options(&self) -> gtk::MenuButton {
        let menu = gtk::MenuButton::builder()
            .icon_name("ls-settings-symbolic")
            .build();
        i18n::bind_property(&menu, "tooltip-text", "Send mode");
        i18n::bind_accessible_label(&menu, "Send mode");
        let weak = Rc::downgrade(&self.0);
        i18n::on_locale_changed(move || {
            if let Some(inner) = weak.upgrade() {
                Ui(inner).refresh_send_controls();
                true
            } else {
                false
            }
        });
        menu.add_css_class("icon-button");
        let icon = gtk::Image::from_icon_name("ls-settings-symbolic");
        icon.set_pixel_size(24);
        menu.set_child(Some(&icon));
        let popover = gtk::Popover::new();
        let content = gtk::Box::new(gtk::Orientation::Vertical, 8);
        margins(&content, 12);
        content.append(&translated_label("Send mode", "section-title"));
        let single = gtk::CheckButton::with_label("Single recipient");
        let multiple = gtk::CheckButton::with_label("Multiple recipients");
        i18n::bind_property(&single, "label", "Single recipient");
        i18n::bind_property(&multiple, "label", "Multiple recipients");
        multiple.set_group(Some(&single));
        single.set_active(self.0.settings.borrow().send_mode == 0);
        multiple.set_active(self.0.settings.borrow().send_mode == 1);
        for (mode, button) in [(0, &single), (1, &multiple)] {
            let ui = self.clone();
            let popover = popover.clone();
            button.connect_toggled(move |button| {
                if !button.is_active() {
                    return;
                }
                ui.set_send_mode(mode);
                popover.popdown();
            });
        }
        content.append(&single);
        content.append(&multiple);
        let link = gtk::Button::new();
        link.set_child(Some(&button_content(
            "ls-language-symbolic",
            "Share via link",
            false,
        )));
        link.add_css_class("text-button");
        {
            let ui = self.clone();
            let popover = popover.clone();
            link.connect_clicked(move |_| {
                popover.popdown();
                ui.show_web_share();
            });
        }
        content.append(&link);
        let help = translated_label(
            "Tap each recipient to send. The selection is kept for other recipients.",
            "secondary",
        );
        help.set_wrap(true);
        help.set_max_width_chars(30);
        content.append(&help);
        popover.set_child(Some(&content));
        menu.set_popover(Some(&popover));
        menu
    }

    pub(super) fn set_send_mode(&self, mode: u32) {
        self.0.settings.borrow_mut().send_mode = mode;
        self.save_settings();
        self.refresh_send_controls();
    }

    pub(super) fn outgoing_device_status(&self, fingerprint: &str) -> gtk::Box {
        if let Some(row) = self.0.outgoing_rows.borrow().get(fingerprint) {
            return row.container.clone();
        }
        let container = gtk::Box::new(gtk::Orientation::Vertical, 6);
        container.set_margin_start(12);
        container.set_margin_end(12);
        container.set_margin_bottom(8);
        container.set_visible(false);
        let line = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let status = label("", "secondary");
        status.set_wrap(true);
        status.set_wrap_mode(gtk::pango::WrapMode::WordChar);
        status.set_max_width_chars(42);
        status.set_width_chars(1);
        status.set_lines(2);
        status.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
        status.set_hexpand(true);
        line.append(&status);
        let cancel = gtk::Button::with_label("Cancel");
        i18n::bind_property(&cancel, "label", "Cancel");
        cancel.add_css_class("text-button");
        let ui = self.clone();
        let key = fingerprint.to_owned();
        cancel.connect_clicked(move |_| ui.cancel_outgoing(&key));
        line.append(&cancel);
        container.append(&line);
        let progress = gtk::ProgressBar::new();
        container.append(&progress);
        self.0.outgoing_rows.borrow_mut().insert(
            fingerprint.to_owned(),
            OutgoingRow {
                container: container.clone(),
                status,
                progress,
                cancel,
            },
        );
        container
    }

    pub(super) fn refresh_send_controls(&self) {
        let outgoing = self.0.outgoing.borrow();
        let rows = self.0.outgoing_rows.borrow();
        let active: Vec<_> = outgoing
            .values()
            .filter(|transfer| transfer.active)
            .collect();
        self.0.sending.set(!active.is_empty());
        for (key, row) in rows.iter() {
            let transfer = outgoing.get(key);
            row.container
                .set_visible(transfer.is_some_and(|transfer| transfer.background));
            if let Some(transfer) = transfer {
                row.status.set_label(&transfer.status.text());
                row.progress.set_fraction(transfer.fraction);
                row.progress
                    .set_visible(transfer.active || transfer.fraction > 0.0);
                row.cancel.set_visible(transfer.active);
                row.cancel
                    .set_sensitive(!transfer.cancellation.is_cancelled());
            }
        }
        for transfer in outgoing.values() {
            if let Some(details) = &transfer.details {
                details.refresh(transfer);
                if transfer.active {
                    details
                        .dialog
                        .set_response_enabled("cancel", !transfer.cancellation.is_cancelled());
                } else if !details.done_response.replace(true) {
                    if details.dialog.has_response("cancel") {
                        details.dialog.remove_response("cancel");
                    }
                    i18n::bind_response(&details.dialog, "close", "Done");
                    details.dialog.set_default_response(Some("close"));
                }
            }
        }
        self.0.cancel_send.set_visible(!outgoing.is_empty());
        let cancel_source = if active.is_empty() {
            "Done"
        } else if active.len() > 1 {
            "Cancel all"
        } else {
            "Cancel"
        };
        if self.0.cancel_send.label().as_deref() != Some(tr(cancel_source).as_str()) {
            i18n::bind_property(&self.0.cancel_send, "label", cancel_source);
        }
        self.0.cancel_send.set_sensitive(
            active.is_empty()
                || active
                    .iter()
                    .any(|transfer| !transfer.cancellation.is_cancelled()),
        );
        if !active.is_empty() {
            self.0.progress_box.set_visible(true);
            self.0.progress.set_fraction(
                active.iter().map(|transfer| transfer.fraction).sum::<f64>() / active.len() as f64,
            );
            self.0.progress_title.set_label(&if active.len() == 1 {
                tr_format(
                    "{alias} · {status}",
                    &[
                        ("alias", active[0].alias.clone()),
                        ("status", active[0].status.text()),
                    ],
                )
            } else {
                tr_format(
                    "Sending files · {count} recipients",
                    &[("count", active.len().to_string())],
                )
            });
        } else if let Some(last) = outgoing.values().max_by_key(|transfer| transfer.updated) {
            self.0.progress.set_fraction(last.fraction);
            self.0.progress_title.set_label(&tr_format(
                "{alias} · {status}",
                &[
                    ("alias", last.alias.clone()),
                    ("status", last.status.text()),
                ],
            ));
        }
    }

    pub(super) fn send(&self, mut peer: DeviceInfo) {
        peer.fingerprint = crate::settings::fingerprint_key(&peer.fingerprint);
        if self.0.settings.borrow().favorites.iter().any(|favorite| {
            crate::settings::same_fingerprint(&favorite.fingerprint, &peer.fingerprint)
                && favorite.protocol != peer.protocol
        }) {
            self.toast_text(
                "This connection does not match the encryption saved for that favorite.",
            );
            return;
        }
        let active = self
            .0
            .outgoing
            .borrow()
            .get(&peer.fingerprint)
            .is_some_and(|transfer| transfer.active);
        if active {
            self.show_outgoing_details(&peer.fingerprint);
            return;
        }
        if self.0.selection.borrow().is_empty() {
            let ui = self.clone();
            self.after_selection(move || ui.send(peer));
            return;
        }
        if self.0.identity.borrow().is_none() && !self.0.offline_fixture {
            self.toast_text("Wait for LocalSend to finish starting.");
            return;
        }
        self.start_outgoing(peer);
    }

    pub(super) fn cancel_outgoing(&self, fingerprint: &str) {
        let target = self
            .0
            .outgoing
            .borrow()
            .get(fingerprint)
            .filter(|transfer| transfer.active && !transfer.cancellation.is_cancelled())
            .map(|transfer| (fingerprint.to_owned(), transfer.id));
        if let Some(target) = target {
            self.confirm_cancel_outgoing(vec![target], None);
        }
    }

    pub(super) fn cancel_all_outgoing(&self) {
        let targets = self
            .0
            .outgoing
            .borrow()
            .iter()
            .filter(|(_, transfer)| transfer.active && !transfer.cancellation.is_cancelled())
            .map(|(key, transfer)| (key.clone(), transfer.id))
            .collect::<Vec<_>>();
        if targets.is_empty() {
            if !self.0.sending.get() {
                self.0.progress_box.set_visible(false);
            }
            return;
        }
        self.confirm_cancel_outgoing(targets, None);
    }

    fn confirm_cancel_outgoing(
        &self,
        targets: Vec<(String, uuid::Uuid)>,
        return_to_details: Option<(String, uuid::Uuid)>,
    ) {
        let dialog = translated_dialog(
            Some("Cancel files transfer"),
            Some("Do you really want to cancel the files transfer?"),
        );
        add_responses(&dialog, &[("continue", "Continue"), ("cancel", "Cancel")]);
        dialog.set_default_response(Some("continue"));
        dialog.set_close_response("continue");
        dialog.set_response_appearance("cancel", adw::ResponseAppearance::Destructive);
        let ui = self.clone();
        dialog.connect_response(None, move |_, response| {
            if response == "cancel" {
                {
                    let mut outgoing = ui.0.outgoing.borrow_mut();
                    for (key, id) in &targets {
                        // The offer may have finished or been replaced while its
                        // confirmation was open. Never cancel a later session.
                        if let Some(transfer) = outgoing
                            .get_mut(key)
                            .filter(|transfer| transfer.id == *id && transfer.active)
                        {
                            transfer.cancellation.cancel();
                            transfer.status = OutgoingStatus::Canceling;
                        }
                    }
                }
                ui.refresh_send_controls();
            } else if let Some((key, id)) = &return_to_details {
                let current =
                    ui.0.outgoing
                        .borrow()
                        .get(key)
                        .is_some_and(|transfer| transfer.id == *id);
                if current {
                    ui.show_outgoing_details(key);
                }
            }
        });
        dialog.present(Some(&self.0.window));
    }

    fn show_outgoing_details(&self, fingerprint: &str) {
        let existing = self
            .0
            .outgoing
            .borrow()
            .get(fingerprint)
            .and_then(|transfer| {
                transfer
                    .details
                    .as_ref()
                    .map(|details| details.dialog.clone())
            });
        if let Some(dialog) = existing {
            dialog.present(Some(&self.0.window));
            return;
        }
        let Some((id, alias, report)) = self
            .0
            .outgoing
            .borrow()
            .get(fingerprint)
            .map(|transfer| (transfer.id, transfer.alias.clone(), transfer.report.clone()))
        else {
            return;
        };
        let dialog = translated_dialog(Some("Sending files"), None);
        dialog.set_body(&alias);
        let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
        let status = label("", "body-text");
        status.set_wrap(true);
        status.set_wrap_mode(gtk::pango::WrapMode::WordChar);
        status.set_max_width_chars(42);
        status.set_width_chars(1);
        status.set_lines(2);
        status.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
        let progress = gtk::ProgressBar::new();
        content.append(&status);
        let current_box = gtk::Box::new(gtk::Orientation::Vertical, 4);
        let current_size = label("", "secondary");
        let current_progress = gtk::ProgressBar::new();
        current_box.append(&current_size);
        current_box.append(&current_progress);
        content.append(&current_box);
        content.append(&translated_label("Total", "section-title"));
        content.append(&progress);
        let metrics = gtk::Box::new(gtk::Orientation::Vertical, 4);
        let count = label("", "secondary");
        let size = label("", "secondary");
        let speed = label("", "secondary");
        let elapsed = label("", "secondary");
        let remaining = label("", "secondary");
        for metric in [&count, &size, &speed, &elapsed, &remaining] {
            metrics.append(metric);
        }
        content.append(&metrics);
        let file_list = gtk::Box::new(gtk::Orientation::Vertical, 12);
        let mut files = Vec::new();
        for file in &report.files {
            let row = gtk::Box::new(gtk::Orientation::Vertical, 4);
            let name = label(&file.name, "body-text");
            name.set_wrap(true);
            name.set_wrap_mode(gtk::pango::WrapMode::WordChar);
            name.set_max_width_chars(42);
            name.set_width_chars(1);
            name.set_lines(2);
            name.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
            name.set_tooltip_text(Some(&file.name));
            row.append(&name);
            let size = label("", "secondary");
            row.append(&size);
            let status = label("", "secondary");
            row.append(&status);
            let progress = gtk::ProgressBar::new();
            row.append(&progress);
            file_list.append(&row);
            files.push(OutgoingFileRow {
                size,
                status,
                progress,
                rendered: Cell::new(None),
            });
        }
        content.append(&file_list);
        dialog.set_extra_child(Some(&super::dialogs::content_scroll(&content, 400)));
        add_responses(&dialog, &[("cancel", "Cancel"), ("close", "Close")]);
        dialog.set_close_response("close");
        let ui = self.clone();
        let key = fingerprint.to_owned();
        dialog.connect_response(None, move |_, response| {
            let (current, finished) = {
                let mut outgoing = ui.0.outgoing.borrow_mut();
                if let Some(transfer) = outgoing.get_mut(&key).filter(|transfer| transfer.id == id)
                {
                    transfer.details = None;
                    (true, !transfer.active)
                } else {
                    (false, false)
                }
            };
            if current && !finished && response == "cancel" {
                ui.confirm_cancel_outgoing(vec![(key.clone(), id)], Some((key.clone(), id)));
            } else if finished && !ui.0.sending.get() {
                ui.0.progress_box.set_visible(false);
                ui.0.stack.set_visible_child_name("send");
            }
        });
        if let Some(transfer) = self
            .0
            .outgoing
            .borrow_mut()
            .get_mut(fingerprint)
            .filter(|transfer| transfer.id == id)
        {
            transfer.details = Some(OutgoingDetails {
                dialog: dialog.clone(),
                status,
                progress,
                current_box,
                current_size,
                current_progress,
                count,
                size,
                speed,
                elapsed,
                remaining,
                files,
                done_response: Cell::new(false),
            });
        }
        self.refresh_send_controls();
        dialog.present(Some(&self.0.window));
    }
    pub(super) fn request_pin(&self, alias: &str, request: transfer::PinRequest) {
        if request.cancellation.is_cancelled() {
            return;
        }
        let dialog = translated_dialog(Some("Enter PIN"), None);
        if request.invalid {
            i18n::bind_format_property(
                &dialog,
                "body",
                "{alias}\nInvalid PIN",
                &[("alias", alias.to_owned())],
            );
        } else {
            dialog.set_body(alias);
        }
        let pin = gtk::PasswordEntry::builder()
            .show_peek_icon(true)
            .activates_default(true)
            .build();
        i18n::bind_accessible_label(&pin, "PIN");
        dialog.set_extra_child(Some(&pin));
        add_responses(&dialog, &[("cancel", "Cancel"), ("confirm", "Confirm")]);
        dialog.set_response_appearance("confirm", adw::ResponseAppearance::Suggested);
        dialog.set_default_response(Some("confirm"));
        dialog.set_close_response("cancel");
        dialog.set_response_enabled("confirm", false);
        let weak = dialog.downgrade();
        pin.connect_changed(move |entry| {
            if let Some(dialog) = weak.upgrade() {
                dialog.set_response_enabled("confirm", !entry.text().is_empty());
            }
        });
        let cancellation = request.cancellation;
        let answer = RefCell::new(Some(request.answer));
        let decided = CancellationToken::new();
        let signal = decided.clone();
        let field = pin.clone();
        dialog.connect_response(None, move |_, response| {
            signal.cancel();
            if let Some(answer) = answer.borrow_mut().take() {
                let value = field.text().to_string();
                // Official LocalSend PINs may contain letters and punctuation.
                let _ = answer.send((response == "confirm" && !value.is_empty()).then_some(value));
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
        pin.grab_focus();
    }
    fn begin_outgoing(
        &self,
        peer: &DeviceInfo,
        background: bool,
    ) -> Option<(uuid::Uuid, CancellationToken)> {
        let key = crate::settings::fingerprint_key(&peer.fingerprint);
        let mut outgoing = self.0.outgoing.borrow_mut();
        if outgoing.get(&key).is_some_and(|transfer| transfer.active) {
            return None;
        }
        let any_active = outgoing.values().any(|transfer| transfer.active);
        if any_active
            && (!background
                || outgoing
                    .values()
                    .any(|transfer| transfer.active && !transfer.background))
        {
            return None;
        }
        let parent = if any_active {
            self.0.outgoing_cancel.borrow().clone()?
        } else {
            let parent = CancellationToken::new();
            *self.0.outgoing_cancel.borrow_mut() = Some(parent.clone());
            parent
        };
        if parent.is_cancelled() {
            return None;
        }
        let cancellation = parent.child_token();
        let id = uuid::Uuid::new_v4();
        let previous = outgoing.insert(
            key,
            OutgoingTransfer {
                id,
                alias: peer.alias.clone(),
                status: OutgoingStatus::Waiting,
                fraction: 0.0,
                report: TransferProgress::queued(&self.0.selection.borrow()),
                active: true,
                background,
                cancellation: cancellation.clone(),
                selection_generation: self.0.selection_generation.get(),
                item_count: self.0.selection.borrow().len(),
                updated: std::time::Instant::now(),
                details: None,
            },
        );
        drop(outgoing);
        if let Some(details) = previous.and_then(|transfer| transfer.details) {
            details.dialog.force_close();
        }
        self.refresh_send_controls();
        Some((id, cancellation))
    }

    fn outgoing_report(&self, key: &str, id: uuid::Uuid, report: TransferProgress) {
        {
            let mut outgoing = self.0.outgoing.borrow_mut();
            let Some(transfer) = outgoing
                .get_mut(key)
                .filter(|transfer| transfer.id == id && transfer.active)
            else {
                return;
            };
            if transfer.cancellation.is_cancelled() && !report.complete {
                return;
            }
            if !transfer.cancellation.is_cancelled() {
                if let Some(file) = report
                    .current_file
                    .and_then(|index| report.files.get(index))
                {
                    transfer.status = OutgoingStatus::Sending(file.name.clone());
                }
            }
            transfer.fraction = report.fraction();
            transfer.report = report;
            transfer.updated = std::time::Instant::now();
        }
        self.refresh_send_controls();
    }

    fn finish_outgoing(
        &self,
        key: &str,
        id: uuid::Uuid,
        result: Result<usize, transfer::SendError>,
    ) {
        let (message, clear_selection, background) = {
            let mut outgoing = self.0.outgoing.borrow_mut();
            let Some(transfer) = outgoing
                .get_mut(key)
                .filter(|transfer| transfer.id == id && transfer.active)
            else {
                return;
            };
            transfer.active = false;
            transfer.updated = std::time::Instant::now();
            if !transfer.report.complete {
                // Certificate/startup errors or a failed task can precede the reporter.
                for file in &mut transfer.report.files {
                    file.outcome = match file.outcome {
                        FileOutcome::Queued | FileOutcome::Sending
                            if matches!(result, Err(transfer::SendError::Cancelled)) =>
                        {
                            FileOutcome::Canceled
                        }
                        FileOutcome::Queued => FileOutcome::Skipped,
                        FileOutcome::Sending => FileOutcome::Failed,
                        outcome => outcome,
                    };
                }
                transfer.report.complete = true;
                transfer.report.remaining = None;
            }
            transfer.status = match &result {
                Ok(sent) => OutgoingStatus::Finished {
                    sent: *sent,
                    total: transfer.item_count,
                },
                Err(error) => OutgoingStatus::Error(error.clone()),
            };
            if result.is_ok() {
                transfer.fraction = 1.0;
            }
            let clear = !transfer.background
                && !transfer.cancellation.is_cancelled()
                && result
                    .as_ref()
                    .is_ok_and(|sent| *sent == transfer.item_count)
                && self.0.selection_generation.get() == transfer.selection_generation;
            (transfer.status.text(), clear, transfer.background)
        };
        if clear_selection {
            self.0.selection.borrow_mut().clear();
            self.refresh_selection();
        }
        if !self
            .0
            .outgoing
            .borrow()
            .values()
            .any(|transfer| transfer.active)
        {
            self.0.outgoing_cancel.borrow_mut().take();
        }
        self.refresh_send_controls();
        if !background {
            self.toast(&message);
        }
    }

    fn start_outgoing(&self, peer: DeviceInfo) {
        let background = self.0.settings.borrow().send_mode == 1;
        let items = self.0.selection.borrow().clone();
        if items.is_empty() {
            return;
        }
        let Some((id, cancellation)) = self.begin_outgoing(&peer, background) else {
            self.toast_text("Finish or cancel the current transfer first.");
            return;
        };
        if !background {
            self.show_outgoing_details(&peer.fingerprint);
        }
        // The Wayland fixture exercises the same state transitions without LAN traffic.
        if self.0.offline_fixture {
            return;
        }
        let Some(identity) = self.0.identity.borrow().clone() else {
            self.finish_outgoing(
                &peer.fingerprint,
                id,
                Err(transfer::SendError::Failed(tr("The sender is not ready."))),
            );
            return;
        };
        let ui = self.clone();
        glib::spawn_future_local(async move {
            let key = peer.fingerprint.clone();
            let alias = peer.alias.clone();
            let (progress, mut updates) =
                tokio::sync::watch::channel(TransferProgress::queued(&items));
            let (requests, prompts) = async_channel::bounded(1);
            let task = tokio::spawn(async move {
                let certificate = network::certificate().map_err(transfer::SendError::Failed)?;
                transfer::send_interactive(
                    identity,
                    certificate,
                    peer,
                    items,
                    transfer::PinRequests {
                        initial_pin: None,
                        requests,
                    },
                    progress,
                    cancellation,
                )
                .await
            });
            loop {
                tokio::select! {
                    progress = updates.changed() => match progress {
                        Ok(()) => {
                            let report = updates.borrow_and_update().clone();
                            ui.outgoing_report(&key, id, report);
                        },
                        Err(_) => break,
                    },
                    request = prompts.recv(), if !prompts.is_closed() => {
                        if let Ok(request) = request { ui.request_pin(&alias, request); }
                    },
                }
            }
            let result = task
                .await
                .unwrap_or_else(|error| Err(transfer::SendError::Failed(error.to_string())));
            ui.finish_outgoing(&key, id, result);
        });
    }

    #[cfg(test)]
    pub(super) fn fixture_start_outgoing(
        &self,
        peer: DeviceInfo,
        background: bool,
    ) -> Option<CancellationToken> {
        self.begin_outgoing(&peer, background)
            .map(|(_, token)| token)
    }

    #[cfg(test)]
    pub(super) fn fixture_outgoing_progress(&self, fingerprint: &str, fraction: f64) {
        let report = self.0.outgoing.borrow().get(fingerprint).map(|transfer| {
            let mut report = transfer.report.clone();
            report.total_bytes = 1000;
            report.bytes_sent = (fraction.clamp(0.0, 1.0) * 1000.0) as u64;
            report.current_file = (!report.files.is_empty()).then_some(0);
            report
        });
        if let Some(report) = report {
            self.fixture_outgoing_report(fingerprint, report);
        }
    }

    #[cfg(test)]
    pub(super) fn fixture_outgoing_report(&self, fingerprint: &str, report: TransferProgress) {
        let id = self
            .0
            .outgoing
            .borrow()
            .get(fingerprint)
            .map(|transfer| transfer.id);
        if let Some(id) = id {
            self.outgoing_report(fingerprint, id, report);
        }
    }

    #[cfg(test)]
    pub(super) fn fixture_finish_outgoing(
        &self,
        fingerprint: &str,
        result: Result<usize, transfer::SendError>,
    ) {
        let current = self
            .0
            .outgoing
            .borrow()
            .get(fingerprint)
            .map(|transfer| (transfer.id, transfer.report.clone()));
        if let Some((id, mut report)) = current {
            if !report.complete {
                report.complete = true;
                for (index, file) in report.files.iter_mut().enumerate() {
                    file.outcome = match &result {
                        Ok(sent) if index < *sent => {
                            file.bytes_sent = file.size;
                            FileOutcome::Finished
                        }
                        Ok(_) => FileOutcome::Skipped,
                        Err(transfer::SendError::Cancelled) => FileOutcome::Canceled,
                        Err(_) => FileOutcome::Failed,
                    };
                }
                report.bytes_sent = report.files.iter().map(|file| file.bytes_sent).sum();
                report.finished_files = report
                    .files
                    .iter()
                    .filter(|file| file.outcome == FileOutcome::Finished)
                    .count();
                if result.is_ok() {
                    report.accepted_files = report.finished_files;
                    report.total_bytes = report.bytes_sent;
                    report.remaining = Some(std::time::Duration::ZERO);
                } else {
                    report.remaining = None;
                }
                self.outgoing_report(fingerprint, id, report);
            }
            self.finish_outgoing(fingerprint, id, result);
        }
    }
}
