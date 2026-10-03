//! Native receive-session progress, modelled after LocalSend's progress page.
//!
//! The protocol server currently admits one native upload at a time, but this
//! view keys native and browser transfers separately so their events never
//! consume each other's pending offers.
use super::*;
use crate::i18n::{self, tr, tr_format};
use std::{collections::VecDeque, time::Instant};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ReceiveFile {
    pub id: String,
    pub name: String,
    pub size: u64,
}

impl ReceiveFile {
    pub(super) fn new(id: impl Into<String>, name: impl Into<String>, size: u64) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            size,
        }
    }
}

#[derive(Clone)]
pub(super) struct ReceiveProgressView(Rc<ReceiveProgressInner>);

struct ReceiveProgressInner {
    window: glib::WeakRef<adw::ApplicationWindow>,
    root: gtk::Box,
    sessions_box: gtk::Box,
    cancel: gtk::Button,
    sessions: RefCell<HashMap<String, ReceiveSession>>,
    pending: RefCell<VecDeque<String>>,
    protocol_keys: RefCell<HashMap<String, String>>,
    retired: RefCell<VecDeque<String>>,
    next_key: Cell<u64>,
}

struct ReceiveSession {
    sender_alias: String,
    destination: Option<PathBuf>,
    expected_files: usize,
    bytes_received: u64,
    total_bytes: u64,
    files: HashMap<String, ReceiveFileState>,
    file_order: Vec<String>,
    started: Instant,
    status: ReceiveStatus,
    widgets: ReceiveSessionWidgets,
    details: Option<ReceiveDetails>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ReceiveStatus {
    Receiving,
    Canceling,
    Finished,
    CanceledBySender,
    CanceledByReceiver,
}

impl ReceiveStatus {
    fn is_active(self) -> bool {
        matches!(self, Self::Receiving | Self::Canceling)
    }

    fn text(self) -> String {
        tr(match self {
            Self::Receiving => "Receiving files…",
            Self::Canceling => "Canceling incoming transfers…",
            Self::Finished => "Finished",
            Self::CanceledBySender => "Canceled by sender",
            Self::CanceledByReceiver => "Canceled by receiver",
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ReceiveFileStatus {
    Queued,
    Receiving,
    Finished,
    Canceled,
}

struct ReceiveFileState {
    name: String,
    size: u64,
    bytes_received: u64,
    path: Option<PathBuf>,
    status: ReceiveFileStatus,
}

struct ReceiveSessionWidgets {
    container: gtk::Box,
    sender: gtk::Label,
    status: gtk::Label,
    progress: gtk::ProgressBar,
    metrics: gtk::Label,
    details: gtk::Button,
    done: gtk::Button,
}

struct ReceiveDetails {
    dialog: adw::AlertDialog,
    status: gtk::Label,
    progress: gtk::ProgressBar,
    count: gtk::Label,
    size: gtk::Label,
    speed: gtk::Label,
    elapsed: gtk::Label,
    remaining: gtk::Label,
    file_list: gtk::Box,
    file_rows: HashMap<String, ReceiveFileRow>,
}

struct ReceiveFileRow {
    size: gtk::Label,
    status: gtk::Label,
    progress: gtk::ProgressBar,
    open: gtk::Button,
    path: Rc<RefCell<Option<PathBuf>>>,
}

impl ReceiveProgressView {
    pub(super) fn new(window: &adw::ApplicationWindow) -> Self {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 10);
        root.add_css_class("selection-card");
        root.set_visible(false);

        let header = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let title = translated_label("Receiving files", "section-title");
        title.set_hexpand(true);
        header.append(&title);
        let cancel = i18n::button("Cancel receiving");
        cancel.add_css_class("text-button");
        header.append(&cancel);
        root.append(&header);

        let sessions_box = gtk::Box::new(gtk::Orientation::Vertical, 10);
        root.append(&sessions_box);

        let view = Self(Rc::new(ReceiveProgressInner {
            window: window.downgrade(),
            root,
            sessions_box,
            cancel,
            sessions: RefCell::new(HashMap::new()),
            pending: RefCell::new(VecDeque::new()),
            protocol_keys: RefCell::new(HashMap::new()),
            retired: RefCell::new(VecDeque::new()),
            next_key: Cell::new(0),
        }));
        let weak = Rc::downgrade(&view.0);
        i18n::on_locale_changed(move || {
            if let Some(inner) = weak.upgrade() {
                ReceiveProgressView(inner).refresh_all();
                true
            } else {
                false
            }
        });
        view
    }

    pub(super) fn widget(&self) -> gtk::Box {
        self.0.root.clone()
    }

    pub(super) fn cancel_button(&self) -> gtk::Button {
        self.0.cancel.clone()
    }

    /// Seeds the full accepted selection before its first upload request. The
    /// server does not reveal its protocol session id during prepare-upload, so
    /// the next matching progress/receipt event binds this provisional entry.
    pub(super) fn begin_offer(
        &self,
        (offer_id, needs_protocol_binding): (String, bool),
        sender_alias: impl Into<String>,
        destination: Option<PathBuf>,
        files: Vec<ReceiveFile>,
    ) {
        if files.is_empty() {
            return;
        }
        let number = self.0.next_key.get().wrapping_add(1);
        self.0.next_key.set(number);
        let key = format!("pending:{number}");
        self.insert_session(key.clone(), sender_alias.into(), destination, files);
        self.0
            .protocol_keys
            .borrow_mut()
            .insert(offer_id, key.clone());
        if needs_protocol_binding {
            // Native receiving admits only one live offer. Browser sessions
            // already have their exact wire id and never enter this queue.
            self.0.pending.borrow_mut().push_back(key);
        }
        self.refresh_all();
    }

    // These values deliberately mirror one protocol event: session and current
    // file counters must remain separate for a multi-file transfer.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn progress(
        &self,
        session_id: &str,
        file_id: &str,
        file_name: String,
        sender_alias: String,
        bytes_received: u64,
        total_bytes: u64,
        file_bytes_received: u64,
        file_size: u64,
        file_count: usize,
    ) {
        let Some(key) = self.resolve_session(session_id, Some(file_id), &sender_alias, file_count)
        else {
            return;
        };
        if let Some(session) = self.0.sessions.borrow_mut().get_mut(&key) {
            // Progress queued before a cancel/terminal event must never revive
            // a session the user already finished handling.
            if !session.status.is_active() {
                return;
            }
            session.expected_files = session.expected_files.max(file_count).max(1);
            session.sender_alias = sender_alias;
            session.bytes_received = bytes_received.min(total_bytes);
            session.total_bytes = total_bytes;
            ensure_file(
                session,
                file_id,
                file_name,
                file_size,
                ReceiveFileStatus::Receiving,
            );
            if let Some(file) = session.files.get_mut(file_id) {
                file.size = file_size;
                file.bytes_received = file_bytes_received.min(file_size);
                file.status = ReceiveFileStatus::Receiving;
            }
        }
        self.refresh_all();
    }

    pub(super) fn file_received(
        &self,
        session_id: &str,
        file_id: &str,
        file_name: String,
        path: PathBuf,
        size: u64,
        sender_alias: String,
    ) {
        let Some(key) = self.resolve_session(session_id, Some(file_id), &sender_alias, 1) else {
            return;
        };
        if let Some(session) = self.0.sessions.borrow_mut().get_mut(&key) {
            session.sender_alias = sender_alias;
            if session.destination.is_none() {
                session.destination = path.parent().map(PathBuf::from);
            }
            ensure_file(
                session,
                file_id,
                file_name,
                size,
                ReceiveFileStatus::Finished,
            );
            if let Some(file) = session.files.get_mut(file_id) {
                file.size = size;
                file.bytes_received = size;
                file.path = Some(path);
                file.status = ReceiveFileStatus::Finished;
            }
            let known_received = session.files.values().fold(0_u64, |total, file| {
                total.saturating_add(file.bytes_received)
            });
            let known_total = session
                .files
                .values()
                .fold(0_u64, |total, file| total.saturating_add(file.size));
            session.bytes_received = session.bytes_received.max(known_received);
            session.total_bytes = session.total_bytes.max(known_total);
        }
        self.refresh_all();
    }

    /// Applies the protocol's ambiguous terminal event. A fully received file
    /// set is success; an incomplete set is sender cancellation unless the user
    /// had already requested local cancellation.
    pub(super) fn session_done(&self, session_id: &str) {
        let key = self.resolve_terminal_session(session_id);
        if let Some(key) = key {
            if let Some(session) = self.0.sessions.borrow_mut().get_mut(&key) {
                if !session.status.is_active() {
                    return;
                }
                let finished = finished_files(session);
                let terminal = if session.expected_files > 0 && finished >= session.expected_files {
                    ReceiveStatus::Finished
                } else if session.status == ReceiveStatus::Canceling {
                    ReceiveStatus::CanceledByReceiver
                } else {
                    ReceiveStatus::CanceledBySender
                };
                session.status = terminal;
                mark_unfinished_files(session, ReceiveFileStatus::Canceled);
            }
        } else {
            // Cancellation can be reported before any payload event binds its
            // wire id. Late queued receipts must not create a fresh active card.
            self.remember_retired(session_id.to_owned());
        }
        self.refresh_all();
    }

    pub(super) fn mark_canceling(&self) {
        for session in self.0.sessions.borrow_mut().values_mut() {
            if session.status == ReceiveStatus::Receiving {
                session.status = ReceiveStatus::Canceling;
            }
        }
        self.refresh_all();
    }

    pub(super) fn cancel_acknowledged(&self) {
        for session in self.0.sessions.borrow_mut().values_mut() {
            if session.status.is_active() {
                session.status = ReceiveStatus::CanceledByReceiver;
                mark_unfinished_files(session, ReceiveFileStatus::Canceled);
            }
        }
        self.refresh_all();
    }

    pub(super) fn cancel_failed(&self) {
        for session in self.0.sessions.borrow_mut().values_mut() {
            if session.status == ReceiveStatus::Canceling {
                session.status = ReceiveStatus::Receiving;
            }
        }
        self.refresh_all();
    }

    pub(super) fn has_active(&self) -> bool {
        self.0
            .sessions
            .borrow()
            .values()
            .any(|session| session.status.is_active())
    }

    pub(super) fn active_session_keys(&self) -> Vec<String> {
        let mut keys = self
            .0
            .sessions
            .borrow()
            .iter()
            .filter(|(_, session)| session.status.is_active())
            .map(|(key, _)| key.clone())
            .collect::<Vec<_>>();
        keys.sort();
        keys
    }

    fn insert_session(
        &self,
        key: String,
        sender_alias: String,
        destination: Option<PathBuf>,
        files: Vec<ReceiveFile>,
    ) {
        let container = gtk::Box::new(gtk::Orientation::Vertical, 6);
        container.add_css_class("selection-item");
        let sender = label(&sender_alias, "body-text");
        sender.set_wrap(true);
        sender.set_wrap_mode(gtk::pango::WrapMode::WordChar);
        sender.set_width_chars(1);
        sender.set_lines(2);
        sender.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
        sender.set_tooltip_text(Some(&sender_alias));
        container.append(&sender);
        let status = label("", "secondary");
        status.set_wrap(true);
        status.set_wrap_mode(gtk::pango::WrapMode::WordChar);
        status.set_width_chars(1);
        status.set_lines(2);
        status.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
        container.append(&status);
        let progress = gtk::ProgressBar::new();
        container.append(&progress);
        let metrics = label("", "secondary");
        container.append(&metrics);
        let actions = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        actions.set_halign(gtk::Align::End);
        let details = i18n::button("Show details");
        details.add_css_class("text-button");
        let done = i18n::button("Done");
        done.add_css_class("text-button");
        done.set_visible(false);
        actions.append(&details);
        actions.append(&done);
        container.append(&actions);
        self.0.sessions_box.append(&container);

        let weak = Rc::downgrade(&self.0);
        let detail_key = key.clone();
        details.connect_clicked(move |_| {
            if let Some(inner) = weak.upgrade() {
                ReceiveProgressView(inner).show_details(&detail_key);
            }
        });
        let weak = Rc::downgrade(&self.0);
        let done_key = key.clone();
        done.connect_clicked(move |_| {
            if let Some(inner) = weak.upgrade() {
                ReceiveProgressView(inner).dismiss(&done_key);
            }
        });

        let mut states = HashMap::new();
        let mut order = Vec::with_capacity(files.len());
        for file in files {
            order.push(file.id.clone());
            states.insert(
                file.id,
                ReceiveFileState {
                    name: file.name,
                    size: file.size,
                    bytes_received: 0,
                    path: None,
                    status: ReceiveFileStatus::Queued,
                },
            );
        }
        let expected_files = states.len();
        let total_bytes = states
            .values()
            .fold(0_u64, |total, file| total.saturating_add(file.size));
        self.0.sessions.borrow_mut().insert(
            key,
            ReceiveSession {
                sender_alias,
                destination,
                expected_files,
                bytes_received: 0,
                total_bytes,
                files: states,
                file_order: order,
                started: Instant::now(),
                status: ReceiveStatus::Receiving,
                widgets: ReceiveSessionWidgets {
                    container,
                    sender,
                    status,
                    progress,
                    metrics,
                    details,
                    done,
                },
                details: None,
            },
        );
    }

    fn resolve_session(
        &self,
        session_id: &str,
        file_id: Option<&str>,
        sender_alias: &str,
        file_count: usize,
    ) -> Option<String> {
        if self.0.retired.borrow().iter().any(|id| id == session_id) {
            return None;
        }
        if let Some(key) = self.0.protocol_keys.borrow().get(session_id).cloned() {
            return Some(key);
        }
        let pending_key = {
            let pending = self.0.pending.borrow();
            let sessions = self.0.sessions.borrow();
            pending
                .iter()
                .find(|key| {
                    sessions.get(*key).is_some_and(|candidate| {
                        candidate.status.is_active()
                            && file_id.is_some_and(|id| candidate.files.contains_key(id))
                    })
                })
                .cloned()
        };
        let key = if let Some(key) = pending_key {
            self.0
                .pending
                .borrow_mut()
                .retain(|pending| pending != &key);
            key
        } else {
            let key = format!("session:{session_id}");
            self.insert_session(key.clone(), sender_alias.to_owned(), None, Vec::new());
            if let Some(session) = self.0.sessions.borrow_mut().get_mut(&key) {
                session.expected_files = file_count.max(1);
            }
            key
        };
        self.0
            .protocol_keys
            .borrow_mut()
            .insert(session_id.to_owned(), key.clone());
        Some(key)
    }

    fn remember_retired(&self, session_id: String) {
        let mut retired = self.0.retired.borrow_mut();
        if !retired.contains(&session_id) {
            retired.push_back(session_id);
            if retired.len() > 128 {
                retired.pop_front();
            }
        }
    }

    fn resolve_terminal_session(&self, session_id: &str) -> Option<String> {
        // Declined browser offers and inline messages also emit SessionDone.
        // Only an exact known identity can complete an accepted file offer.
        self.0.protocol_keys.borrow().get(session_id).cloned()
    }

    fn refresh_all(&self) {
        let keys = self.0.sessions.borrow().keys().cloned().collect::<Vec<_>>();
        for key in keys {
            self.refresh_session(&key);
        }
        let sessions = self.0.sessions.borrow();
        let any = !sessions.is_empty();
        let active = sessions.values().any(|session| session.status.is_active());
        let canceling = sessions
            .values()
            .any(|session| session.status == ReceiveStatus::Canceling);
        drop(sessions);
        self.0.root.set_visible(any);
        self.0.cancel.set_visible(active);
        self.0.cancel.set_sensitive(active && !canceling);
    }

    fn refresh_session(&self, key: &str) {
        let mut sessions = self.0.sessions.borrow_mut();
        let Some(session) = sessions.get_mut(key) else {
            return;
        };
        session.widgets.sender.set_label(&session.sender_alias);
        session
            .widgets
            .sender
            .set_tooltip_text(Some(&session.sender_alias));
        let current = session
            .file_order
            .iter()
            .filter_map(|id| session.files.get(id))
            .find(|file| file.status == ReceiveFileStatus::Receiving);
        let status = if session.status == ReceiveStatus::Receiving {
            current.map_or_else(
                || session.status.text(),
                |file| {
                    tr_format(
                        "Receiving {name} · {received} / {total}",
                        &[
                            ("name", file.name.clone()),
                            ("received", transfer::size_label(file.bytes_received)),
                            ("total", transfer::size_label(file.size)),
                        ],
                    )
                },
            )
        } else {
            session.status.text()
        };
        session.widgets.status.set_label(&status);
        let fraction = session_fraction(session);
        session.widgets.progress.set_fraction(fraction);
        session
            .widgets
            .progress
            .set_visible(session.status.is_active() || fraction > 0.0);
        let finished = finished_files(session);
        session.widgets.metrics.set_label(&tr_format(
            "Files: {curr} / {n}",
            &[
                ("curr", finished.to_string()),
                (
                    "n",
                    session.expected_files.max(session.files.len()).to_string(),
                ),
            ],
        ));
        session
            .widgets
            .done
            .set_visible(!session.status.is_active());
        session
            .widgets
            .details
            .set_visible(!session.files.is_empty());
        refresh_details(session, &self.0.window);
    }

    fn show_details(&self, key: &str) {
        let existing = self.0.sessions.borrow().get(key).and_then(|session| {
            session
                .details
                .as_ref()
                .map(|details| details.dialog.clone())
        });
        if let Some(dialog) = existing {
            if let Some(window) = self.0.window.upgrade() {
                dialog.present(Some(&window));
            }
            return;
        }
        let (sender_alias, destination, active) = {
            let sessions = self.0.sessions.borrow();
            let Some(session) = sessions.get(key) else {
                return;
            };
            (
                session.sender_alias.clone(),
                session.destination.clone(),
                session.status.is_active(),
            )
        };
        let dialog = translated_dialog(Some("Receiving files"), None);
        // Sender aliases are user content and intentionally bypass localization.
        dialog.set_body(&sender_alias);
        let content = gtk::Box::new(gtk::Orientation::Vertical, 10);
        if let Some(destination) = destination {
            let destination_box = gtk::Box::new(gtk::Orientation::Vertical, 2);
            destination_box.append(&translated_label("Save received files to", "secondary"));
            let path = label(&destination.to_string_lossy(), "body-text");
            path.set_selectable(true);
            path.set_wrap(true);
            path.set_wrap_mode(gtk::pango::WrapMode::WordChar);
            path.set_width_chars(1);
            path.set_max_width_chars(42);
            destination_box.append(&path);
            content.append(&destination_box);
        }
        let status = label("", "body-text");
        status.set_wrap(true);
        status.set_wrap_mode(gtk::pango::WrapMode::WordChar);
        status.set_width_chars(1);
        status.set_lines(2);
        status.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
        content.append(&status);
        let progress = gtk::ProgressBar::new();
        content.append(&progress);
        content.append(&translated_label("Total", "section-title"));
        let count = label("", "secondary");
        let size = label("", "secondary");
        let speed = label("", "secondary");
        let elapsed = label("", "secondary");
        let remaining = label("", "secondary");
        for metric in [&count, &size, &speed, &elapsed, &remaining] {
            content.append(metric);
        }
        let file_list = gtk::Box::new(gtk::Orientation::Vertical, 10);
        content.append(&file_list);
        dialog.set_extra_child(Some(&super::dialogs::content_scroll(&content, 320)));
        if active {
            add_responses(&dialog, &[("cancel", "Cancel"), ("close", "Close")]);
        } else {
            add_response(&dialog, "done", "Done");
            dialog.set_default_response(Some("done"));
        }
        dialog.set_close_response(if active { "close" } else { "done" });
        let weak = Rc::downgrade(&self.0);
        let response_key = key.to_owned();
        dialog.connect_response(None, move |_, response| {
            let Some(inner) = weak.upgrade() else {
                return;
            };
            let view = ReceiveProgressView(inner);
            if let Some(session) = view.0.sessions.borrow_mut().get_mut(&response_key) {
                session.details = None;
            }
            match response {
                "cancel" => view.0.cancel.emit_clicked(),
                "done" => view.dismiss(&response_key),
                _ => {}
            }
        });
        if let Some(session) = self.0.sessions.borrow_mut().get_mut(key) {
            session.details = Some(ReceiveDetails {
                dialog: dialog.clone(),
                status,
                progress,
                count,
                size,
                speed,
                elapsed,
                remaining,
                file_list,
                file_rows: HashMap::new(),
            });
        }
        self.refresh_session(key);
        if let Some(window) = self.0.window.upgrade() {
            dialog.present(Some(&window));
        }
    }

    fn dismiss(&self, key: &str) {
        let Some(session) = self.0.sessions.borrow_mut().remove(key) else {
            return;
        };
        if session.status.is_active() {
            self.0.sessions.borrow_mut().insert(key.to_owned(), session);
            return;
        }
        self.0.pending.borrow_mut().retain(|pending| pending != key);
        let retired = self
            .0
            .protocol_keys
            .borrow()
            .iter()
            .filter(|(_, mapped)| mapped.as_str() == key)
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>();
        self.0
            .protocol_keys
            .borrow_mut()
            .retain(|_, mapped| mapped != key);
        for session_id in retired {
            self.remember_retired(session_id);
        }
        self.0.sessions_box.remove(&session.widgets.container);
        if let Some(details) = session.details {
            details.dialog.force_close();
        }
        self.refresh_all();
    }
}

fn ensure_file(
    session: &mut ReceiveSession,
    file_id: &str,
    name: String,
    size: u64,
    status: ReceiveFileStatus,
) {
    if let Some(file) = session.files.get_mut(file_id) {
        // The name supplied by the protocol is user content. Keep it verbatim.
        file.name = name;
        file.size = size;
        return;
    }
    session.file_order.push(file_id.to_owned());
    session.files.insert(
        file_id.to_owned(),
        ReceiveFileState {
            name,
            size,
            bytes_received: 0,
            path: None,
            status,
        },
    );
}

fn finished_files(session: &ReceiveSession) -> usize {
    session
        .files
        .values()
        .filter(|file| file.status == ReceiveFileStatus::Finished)
        .count()
}

fn mark_unfinished_files(session: &mut ReceiveSession, status: ReceiveFileStatus) {
    for file in session.files.values_mut() {
        if file.status != ReceiveFileStatus::Finished {
            file.status = status;
        }
    }
}

fn session_totals(session: &ReceiveSession) -> (u64, u64) {
    (session.bytes_received, session.total_bytes)
}

fn session_fraction(session: &ReceiveSession) -> f64 {
    if session.status == ReceiveStatus::Finished {
        return 1.0;
    }
    let (received, total) = session_totals(session);
    if total == 0 {
        0.0
    } else {
        (received as f64 / total as f64).clamp(0.0, 1.0)
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

fn refresh_details(session: &mut ReceiveSession, window: &glib::WeakRef<adw::ApplicationWindow>) {
    let status_text = session.status.text();
    let fraction = session_fraction(session);
    let finished = finished_files(session);
    let expected = session.expected_files.max(session.files.len());
    let (received, total) = session_totals(session);
    let elapsed = session.started.elapsed();
    let bytes_per_second = if elapsed.as_secs_f64() > 0.0 {
        received as f64 / elapsed.as_secs_f64()
    } else {
        0.0
    };
    let remaining = if session.files.len() >= expected && bytes_per_second > 0.0 {
        Some(std::time::Duration::from_secs_f64(
            total.saturating_sub(received) as f64 / bytes_per_second,
        ))
    } else {
        None
    };
    let Some(details) = session.details.as_mut() else {
        return;
    };
    details.status.set_label(&status_text);
    details.progress.set_fraction(fraction);
    details.count.set_label(&tr_format(
        "Files: {curr} / {n}",
        &[("curr", finished.to_string()), ("n", expected.to_string())],
    ));
    details.size.set_label(&tr_format(
        "Size: {curr} / {n}",
        &[
            ("curr", transfer::size_label(received)),
            ("n", transfer::size_label(total)),
        ],
    ));
    details.speed.set_label(&tr_format(
        "Speed: {speed}/s",
        &[("speed", transfer::size_label(bytes_per_second as u64))],
    ));
    details.elapsed.set_label(&tr_format(
        "Elapsed: {time}",
        &[("time", duration_label(elapsed))],
    ));
    details.remaining.set_label(&tr_format(
        "Remaining: {time}",
        &[(
            "time",
            remaining.map(duration_label).unwrap_or_else(|| "—".into()),
        )],
    ));
    for file_id in &session.file_order {
        let Some(file) = session.files.get(file_id) else {
            continue;
        };
        if !details.file_rows.contains_key(file_id) {
            let row = receive_file_row(file, window.clone());
            details.file_list.append(&row.0);
            details.file_rows.insert(file_id.clone(), row.1);
        }
        if let Some(row) = details.file_rows.get(file_id) {
            row.size.set_label(&tr_format(
                "Size: {curr} / {n}",
                &[
                    ("curr", transfer::size_label(file.bytes_received)),
                    ("n", transfer::size_label(file.size)),
                ],
            ));
            row.status.set_label(&tr(match file.status {
                ReceiveFileStatus::Queued => "Queue",
                ReceiveFileStatus::Receiving => "Receiving files…",
                ReceiveFileStatus::Finished => "Done",
                ReceiveFileStatus::Canceled
                    if session.status == ReceiveStatus::CanceledByReceiver =>
                {
                    "Canceled by receiver"
                }
                ReceiveFileStatus::Canceled => "Canceled by sender",
            }));
            row.progress
                .set_visible(file.status == ReceiveFileStatus::Receiving);
            row.progress.set_fraction(if file.size == 0 {
                0.0
            } else {
                (file.bytes_received as f64 / file.size as f64).clamp(0.0, 1.0)
            });
            *row.path.borrow_mut() = file.path.clone();
            row.open.set_sensitive(file.path.is_some());
            row.open
                .set_visible(file.status == ReceiveFileStatus::Finished);
        }
    }
    if !session.status.is_active() {
        if details.dialog.has_response("cancel") {
            details.dialog.remove_response("cancel");
        }
        if details.dialog.has_response("close") {
            details.dialog.remove_response("close");
        }
        if !details.dialog.has_response("done") {
            add_response(&details.dialog, "done", "Done");
            details.dialog.set_close_response("done");
            details.dialog.set_default_response(Some("done"));
        }
    }
}

fn receive_file_row(
    file: &ReceiveFileState,
    window: glib::WeakRef<adw::ApplicationWindow>,
) -> (gtk::Box, ReceiveFileRow) {
    let row = gtk::Box::new(gtk::Orientation::Vertical, 4);
    let heading = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let name = label(&file.name, "body-text");
    name.set_hexpand(true);
    name.set_wrap(true);
    name.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    name.set_width_chars(1);
    name.set_lines(2);
    name.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
    name.set_tooltip_text(Some(&file.name));
    heading.append(&name);
    let open = gtk::Button::builder()
        .icon_name("document-open-symbolic")
        .build();
    open.add_css_class("icon-button");
    open.set_sensitive(false);
    open.set_visible(false);
    i18n::bind_property(&open, "tooltip-text", "Open file");
    i18n::bind_accessible_label(&open, "Open file");
    heading.append(&open);
    row.append(&heading);
    let size = label("", "secondary");
    let status = label("", "secondary");
    let progress = gtk::ProgressBar::new();
    row.append(&size);
    row.append(&status);
    row.append(&progress);
    let path = Rc::new(RefCell::new(file.path.clone()));
    let launch_path = path.clone();
    open.connect_clicked(move |_| {
        let Some(path) = launch_path.borrow().clone() else {
            return;
        };
        let launcher = gtk::FileLauncher::new(Some(&gio::File::for_path(path)));
        let window = window.clone();
        glib::spawn_future_local(async move {
            let parent = window.upgrade();
            if let Err(error) = launcher.launch_future(parent.as_ref()).await {
                if !error.matches(gtk::DialogError::Dismissed) {
                    if let Some(parent) = parent {
                        alert(&parent, "Could not open file", &error.to_string());
                    }
                }
            }
        });
    });
    let drag_source = gtk::DragSource::new();
    drag_source.set_actions(gdk::DragAction::COPY);
    let drag_path = path.clone();
    drag_source.connect_prepare(move |_, _, _| {
        let path = drag_path.borrow().clone()?;
        if path.exists() {
            let file = gio::File::for_path(&path);
            let file_list = gdk::FileList::from_array(&[file]);
            Some(gdk::ContentProvider::for_value(&file_list.to_value()))
        } else {
            None
        }
    });
    row.add_controller(drag_source);
    (
        row,
        ReceiveFileRow {
            size,
            status,
            progress,
            open,
            path,
        },
    )
}
