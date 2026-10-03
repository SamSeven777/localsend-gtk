use crate::{
    i18n,
    network::{self, Command, Event},
    settings::Settings,
    transfer::{self, Selection, Source},
};
use adw::prelude::*;
use gtk4::{self as gtk, gdk, glib};
use localsend_rs::{
    protocol::{DeviceInfo, DeviceType},
    server::ServerEvent,
};
use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    path::PathBuf,
    rc::Rc,
};
use tokio_util::sync::CancellationToken;

mod desktop_controls;
mod devices;
mod dialogs;
mod history_view;
mod receive_progress;
mod receiving;
mod selection_view;
mod sending;
mod settings_view;
mod sharing;
mod web;

#[cfg(test)]
mod tests {
    use super::*;
    fn find_theme(widget: &gtk::Widget) -> Option<adw::ComboRow> {
        if let Some(row) = widget.downcast_ref::<adw::ComboRow>() {
            return Some(row.clone());
        }
        let mut child = widget.first_child();
        while let Some(item) = child {
            if let Some(row) = find_theme(&item) {
                return Some(row);
            }
            child = item.next_sibling();
        }
        None
    }
    fn settle() {
        glib::MainContext::default()
            .block_on(glib::timeout_future(std::time::Duration::from_millis(350)));
    }
    fn find_switch(widget: &gtk::Widget, title: &str) -> Option<adw::SwitchRow> {
        if let Some(row) = widget.downcast_ref::<adw::SwitchRow>() {
            if row.title() == title {
                return Some(row.clone());
            }
        }
        let mut child = widget.first_child();
        while let Some(item) = child {
            if let Some(row) = find_switch(&item, title) {
                return Some(row);
            }
            child = item.next_sibling();
        }
        None
    }
    fn find_color(widget: &gtk::Widget) -> Option<adw::ComboRow> {
        find_combo(widget, "Color")
    }
    fn find_combo(widget: &gtk::Widget, title: &str) -> Option<adw::ComboRow> {
        if let Some(row) = widget.downcast_ref::<adw::ComboRow>() {
            if row.title() == title {
                return Some(row.clone());
            }
        }
        let mut child = widget.first_child();
        while let Some(item) = child {
            if let Some(row) = find_combo(&item, title) {
                return Some(row);
            }
            child = item.next_sibling();
        }
        None
    }
    fn checkboxes(widget: &gtk::Widget, found: &mut Vec<gtk::CheckButton>) {
        if let Some(check) = widget.downcast_ref::<gtk::CheckButton>() {
            found.push(check.clone());
        }
        let mut child = widget.first_child();
        while let Some(item) = child {
            checkboxes(&item, found);
            child = item.next_sibling();
        }
    }
    fn labels(widget: &gtk::Widget, found: &mut Vec<String>) {
        if let Some(label) = widget.downcast_ref::<gtk::Label>() {
            found.push(label.label().to_string());
        }
        let mut child = widget.first_child();
        while let Some(item) = child {
            labels(&item, found);
            child = item.next_sibling();
        }
    }
    fn progress_bars(widget: &gtk::Widget, found: &mut Vec<gtk::ProgressBar>) {
        if let Some(progress) = widget.downcast_ref::<gtk::ProgressBar>() {
            found.push(progress.clone());
        }
        let mut child = widget.first_child();
        while let Some(item) = child {
            progress_bars(&item, found);
            child = item.next_sibling();
        }
    }
    fn find_entry(widget: &gtk::Widget, title: &str) -> Option<adw::EntryRow> {
        if let Some(row) = widget.downcast_ref::<adw::EntryRow>() {
            if row.title() == title {
                return Some(row.clone());
            }
        }
        let mut child = widget.first_child();
        while let Some(item) = child {
            if let Some(row) = find_entry(&item, title) {
                return Some(row);
            }
            child = item.next_sibling();
        }
        None
    }
    fn find_tooltip(widget: &gtk::Widget, tooltip: &str) -> Option<gtk::Widget> {
        if widget.tooltip_text().as_deref() == Some(tooltip) {
            return Some(widget.clone());
        }
        let mut child = widget.first_child();
        while let Some(item) = child {
            if let Some(found) = find_tooltip(&item, tooltip) {
                return Some(found);
            }
            child = item.next_sibling();
        }
        None
    }
    fn find_text_view(widget: &gtk::Widget) -> Option<gtk::TextView> {
        if let Some(text) = widget.downcast_ref::<gtk::TextView>() {
            return Some(text.clone());
        }
        let mut child = widget.first_child();
        while let Some(item) = child {
            if let Some(text) = find_text_view(&item) {
                return Some(text);
            }
            child = item.next_sibling();
        }
        None
    }
    fn respond(ui: &Ui, response: &str) {
        // Let the native dialog finish presentation before simulating a click.
        settle();
        let dialog =
            ui.0.window
                .visible_dialog()
                .unwrap()
                .downcast::<adw::AlertDialog>()
                .unwrap();
        let label = match response {
            "add" | "confirm" => "Confirm",
            "continue" => "Continue",
            "cancel" => "Cancel",
            "copy" => "Copy",
            "close" => "Close",
            "done" => "Done",
            "delete" => "Delete",
            _ => panic!("Unknown fixture response: {response}"),
        };
        find_button(dialog.upcast_ref(), label)
            .unwrap()
            .emit_clicked();
        settle();
        assert!(
            ui.0.window
                .visible_dialog()
                .is_none_or(|visible| visible != dialog.clone().upcast::<adw::Dialog>()),
            "Response {response} must dismiss its dialog"
        );
    }
    fn find_button(widget: &gtk::Widget, label: &str) -> Option<gtk::Button> {
        if let Some(button) = widget.downcast_ref::<gtk::Button>() {
            if button.label().as_deref() == Some(label) {
                return Some(button.clone());
            }
        }
        let mut child = widget.first_child();
        while let Some(item) = child {
            if let Some(button) = find_button(&item, label) {
                return Some(button);
            }
            child = item.next_sibling();
        }
        None
    }
    fn find_named(widget: &gtk::Widget, name: &str) -> Option<gtk::Widget> {
        if widget.widget_name() == name {
            return Some(widget.clone());
        }
        let mut child = widget.first_child();
        while let Some(item) = child {
            if let Some(found) = find_named(&item, name) {
                return Some(found);
            }
            child = item.next_sibling();
        }
        None
    }
    fn show_settings_group(ui: &Ui, name: &str) {
        settle();
        let scroll =
            ui.0.stack
                .child_by_name("settings")
                .unwrap()
                .downcast::<gtk::ScrolledWindow>()
                .unwrap();
        let group = find_named(scroll.upcast_ref(), &format!("settings-{name}")).unwrap();
        let bounds = group.compute_bounds(&scroll).unwrap();
        let adjustment = scroll.vadjustment();
        adjustment.set_value(
            (adjustment.value() + f64::from(bounds.y()) - 16.0)
                .clamp(0.0, (adjustment.upper() - adjustment.page_size()).max(0.0)),
        );
        settle();
    }
    fn assert_receive_action_uncovered(ui: &Ui) {
        let link = find_tooltip(
            ui.0.window.upcast_ref(),
            "Receive files from another device's web browser",
        )
        .unwrap();
        let bounds = link.compute_bounds(&ui.0.window).unwrap();
        let picked =
            ui.0.window
                .pick(
                    (bounds.x() + bounds.width() / 2.0).into(),
                    (bounds.y() + bounds.height() / 2.0).into(),
                    gtk::PickFlags::DEFAULT,
                )
                .unwrap();
        assert!(
            picked == link || picked.is_ancestor(&link),
            "A transient notification must not intercept the receive action"
        );
    }
    fn incoming_fixture(
        peer: DeviceInfo,
    ) -> (
        network::IncomingRequest,
        tokio::sync::oneshot::Receiver<Vec<localsend_rs::protocol::FileId>>,
    ) {
        use localsend_rs::protocol::{FileId, FileMetadata};
        let files = ["A photograph.jpg", "B notes.txt"]
            .into_iter()
            .map(|name| {
                let id = FileId::new();
                (
                    id.clone(),
                    FileMetadata {
                        id,
                        file_name: name.into(),
                        size: 1024,
                        file_type: "application/octet-stream".into(),
                        sha256: None,
                        preview: None,
                        metadata: None,
                    },
                )
            })
            .collect();
        let (request, answer) = crate::web_receive::PendingRequest::fixture(peer, files);
        (network::IncomingRequest::Browser(request), answer)
    }
    fn snapshot(ui: &Ui, name: &str) {
        eprintln!("Rendering {name}");
        if !name.ends_with("-toast") {
            for toast in ui.0.fixture_toasts.borrow_mut().drain(..) {
                toast.dismiss();
            }
        }
        settle();
        let paintable = gtk::WidgetPaintable::new(Some(&ui.0.window));
        // A just-closed popover can invalidate the frame between the timeout
        // and this snapshot. Wait for a real rendered frame, never a blank image.
        let mut node = None;
        for _ in 0..5 {
            let snapshot = gtk::Snapshot::new();
            paintable.snapshot(
                &snapshot,
                ui.0.window.width() as f64,
                ui.0.window.height() as f64,
            );
            node = snapshot.to_node();
            if node.is_some() {
                break;
            }
            ui.0.window.queue_draw();
            settle();
        }
        let node = node.expect("Rendered window after waiting for a frame");
        let texture = ui.0.window.renderer().unwrap().render_texture(&node, None);
        let path = std::env::var_os("LOCALSEND_SCREENSHOT_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("target/screenshots"));
        std::fs::create_dir_all(&path).unwrap();
        texture
            .save_to_png(path.join(format!("{name}.png")))
            .unwrap();
    }

    fn incoming_dialog_buttons(ui: &Ui, peer: &DeviceInfo) {
        let settings = gtk::Settings::default().unwrap();
        let animations = settings.property::<bool>("gtk-enable-animations");
        for animate in [false, true] {
            settings.set_property("gtk-enable-animations", animate);
            for action in ["Accept", "Decline", "Dismiss"] {
                let (request, mut answer) = incoming_fixture(peer.clone());
                let session = request.progress_identity().0;
                ui.incoming_offer(request);
                settle();
                let dialog = ui.0.window.visible_dialog().unwrap();
                // Exercise the actual button/close path: emitting `response`
                // directly skips libadwaita's closed/default-handler ordering.
                if action == "Dismiss" {
                    dialog.close();
                } else {
                    find_button(dialog.upcast_ref(), action)
                        .expect("Visible consent button")
                        .emit_clicked();
                }
                settle();
                let expected = if action == "Accept" { 2 } else { 0 };
                assert_eq!(
                    answer.try_recv().unwrap().len(),
                    expected,
                    "{action} with animations={animate}"
                );
                assert_eq!(ui.0.incoming_pending.get(), 0);
                assert!(ui.0.incoming_dialogs.borrow().is_empty());
                assert!(ui.0.window.visible_dialog().is_none());
                if action == "Accept" {
                    ui.0.receive_view.session_done(&session);
                    while let Some(done) =
                        find_button(ui.0.receive_view.widget().upcast_ref(), "Done")
                    {
                        done.emit_clicked();
                    }
                }
            }
        }
        settings.set_property("gtk-enable-animations", animations);
    }

    fn receive_logo_animation(ui: &Ui) {
        let logo = find_named(ui.0.window.upcast_ref(), "receive-logo").unwrap();
        let pixels = || {
            let snapshot = gtk::Snapshot::new();
            gtk::WidgetPaintable::new(Some(&logo)).snapshot(
                &snapshot,
                logo.width() as f64,
                logo.height() as f64,
            );
            let texture =
                ui.0.window
                    .renderer()
                    .unwrap()
                    .render_texture(snapshot.to_node().unwrap(), None);
            let stride = texture.width() as usize * 4;
            let mut bytes = vec![0; stride * texture.height() as usize];
            texture.download(&mut bytes, stride);
            bytes
        };
        let settings = gtk::Settings::default().unwrap();
        let system_animations = settings.property::<bool>("gtk-enable-animations");
        settings.set_property("gtk-enable-animations", true);
        let identity = DeviceInfo::new(
            ui.0.settings.borrow().alias.clone(),
            53317,
            localsend_rs::Protocol::Https,
        );
        ui.network_event(Event::Ready(identity));
        settle();
        let first = pixels();
        settle();
        assert!(first != pixels(), "Online receive logo must visibly rotate");
        ui.0.settings.borrow_mut().animations = false;
        settle();
        let still = pixels();
        settle();
        assert!(
            still == pixels(),
            "App animation preference must pause the logo"
        );
        ui.0.settings.borrow_mut().animations = true;
        settings.set_property("gtk-enable-animations", false);
        settle();
        let still = pixels();
        settle();
        assert!(
            still == pixels(),
            "System reduced motion must pause the logo"
        );
        settings.set_property("gtk-enable-animations", true);
        ui.server_state_changed(false, ui.0.settings.borrow().clone());
        settle();
        let still = pixels();
        settle();
        assert!(still == pixels(), "Stopped server must pause the logo");
        ui.server_state_changed(true, ui.0.settings.borrow().clone());
        settle();
        let restarted = pixels();
        settle();
        assert!(
            restarted != pixels(),
            "Restarting the server must resume rotation"
        );
        settings.set_property("gtk-enable-animations", system_animations);
        ui.0.server_running.set(false); // Keep the remaining offline snapshots deterministic.
        ui.0.identity.borrow_mut().take();
    }

    fn native_dialog_transfers(ui: &Ui, peer: &DeviceInfo) {
        use localsend_rs::{protocol::PrepareUploadResponse, server::LocalSendServer, Protocol};

        let runtime = tokio::runtime::Runtime::new().unwrap();
        let destination = tempfile::tempdir().unwrap();
        let (mut server, mut events) = runtime.block_on(async {
            LocalSendServer::builder()
                .alias("GTK consent regression")
                .port(0)
                .protocol(Protocol::Https)
                .save_dir(destination.path())
                .auto_accept(false)
                .build()
                .await
                .unwrap()
        });
        let base = format!("https://127.0.0.1:{}/api/localsend/v2", server.port());
        let client = reqwest::Client::builder()
            .no_proxy()
            .danger_accept_invalid_certs(true)
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .unwrap();
        let settings = gtk::Settings::default().unwrap();
        let animations = settings.property::<bool>("gtk-enable-animations");
        for animate in [false, true] {
            settings.set_property("gtk-enable-animations", animate);
            for action in ["Accept", "Copy", "Close"] {
                let file_name = format!("consent-{animate}.txt");
                let text = "Native HTTPS consent regression";
                let inline = action != "Accept";
                let payload = serde_json::json!({
                    "info": peer,
                    "files": {"test-file": {
                        "id": "test-file", "fileName": file_name,
                        "fileType": "text/plain", "size": text.len(),
                        "preview": if inline { Some(text) } else { None },
                    }}
                });
                let prepare = client.post(format!("{base}/prepare-upload")).json(&payload);
                let response = runtime.spawn(async move { prepare.send().await.unwrap() });
                let request = runtime.block_on(async {
                    loop {
                        let event =
                            tokio::time::timeout(std::time::Duration::from_secs(5), events.recv())
                                .await
                                .unwrap()
                                .unwrap();
                        if let ServerEvent::TransferRequest(request) = event {
                            break network::IncomingRequest::native_fixture(request);
                        }
                    }
                });
                let offer = request.progress_identity().0;
                ui.incoming_offer(request);
                settle();
                let dialog = ui.0.window.visible_dialog().unwrap();
                find_button(dialog.upcast_ref(), action)
                    .unwrap()
                    .emit_clicked();
                settle();
                let response = runtime.block_on(response).unwrap();
                assert_eq!(
                    response.status().as_u16(),
                    if inline { 204 } else { 200 },
                    "Native {action} with animations={animate}"
                );
                if !inline {
                    runtime.block_on(async {
                        let accepted: PrepareUploadResponse = response.json().await.unwrap();
                        let (file_id, token) = accepted.files.iter().next().unwrap();
                        let uploaded = client
                            .post(format!("{base}/upload"))
                            .query(&[
                                ("sessionId", accepted.session_id.to_string()),
                                ("fileId", file_id.to_string()),
                                ("token", token.to_string()),
                            ])
                            .body(text)
                            .send()
                            .await
                            .unwrap();
                        assert_eq!(uploaded.status(), reqwest::StatusCode::OK);
                    });
                    assert_eq!(
                        std::fs::read_to_string(destination.path().join(&file_name)).unwrap(),
                        text
                    );
                    ui.0.receive_view.session_done(&offer);
                    while let Some(done) =
                        find_button(ui.0.receive_view.widget().upcast_ref(), "Done")
                    {
                        done.emit_clicked();
                    }
                }
                assert_eq!(ui.0.incoming_pending.get(), 0);
                assert!(ui.0.incoming_dialogs.borrow().is_empty());
            }
        }
        settings.set_property("gtk-enable-animations", animations);
        runtime.block_on(server.stop());
    }

    fn incoming_notification_actions(ui: &Ui, peer: &DeviceInfo) {
        let app = ui.0.window.application().unwrap();
        let (first, mut first_answer) = incoming_fixture(peer.clone());
        let session = first.progress_identity().0;
        ui.incoming_offer_with_timeout(first, std::time::Duration::from_secs(2));
        settle();
        let first_id =
            ui.0.incoming_dialogs
                .borrow()
                .keys()
                .next()
                .unwrap()
                .clone();
        let first_dialog = ui.0.incoming_dialogs.borrow()[&first_id].upgrade().unwrap();
        let mut checks = Vec::new();
        checkboxes(first_dialog.upcast_ref(), &mut checks);
        for check in &checks {
            check.set_active(false);
        }
        app.activate_action("accept-transfer", Some(&first_id.to_variant()));
        assert!(first_answer.try_recv().is_err());
        assert_eq!(ui.0.incoming_pending.get(), 1);
        checks[0].set_active(true);

        let (second, mut second_answer) = incoming_fixture(peer.clone());
        ui.incoming_offer(second);
        settle();
        let second_id =
            ui.0.incoming_dialogs
                .borrow()
                .keys()
                .find(|id| *id != &first_id)
                .unwrap()
                .clone();
        // Invoke the same parameterized application action as a desktop
        // notification. It must accept only the selected file from this offer.
        app.activate_action("accept-transfer", Some(&first_id.to_variant()));
        assert_eq!(first_answer.try_recv().unwrap().len(), 1);
        assert!(second_answer.try_recv().is_err());
        assert_eq!(ui.0.incoming_pending.get(), 1);
        // Wait longer than the full original timeout, independently of how
        // much time GTK presentation consumed before the response.
        glib::MainContext::default()
            .block_on(glib::timeout_future(std::time::Duration::from_millis(2100)));
        // A consumed notification and its old deadline cannot decide or clear
        // the other offer, even if the desktop delivers a stale action again.
        app.activate_action("accept-transfer", Some(&first_id.to_variant()));
        assert!(second_answer.try_recv().is_err());
        assert!(ui.0.incoming_dialogs.borrow().contains_key(&second_id));
        app.activate_action("decline-transfer", Some(&second_id.to_variant()));
        assert!(second_answer.try_recv().unwrap().is_empty());
        settle();
        assert_eq!(ui.0.incoming_pending.get(), 0);
        assert!(ui.0.incoming_dialogs.borrow().is_empty());
        ui.0.receive_view.session_done(&session);
        while let Some(done) = find_button(ui.0.receive_view.widget().upcast_ref(), "Done") {
            done.emit_clicked();
        }

        let (expired, mut expired_answer) = incoming_fixture(peer.clone());
        ui.incoming_offer_with_timeout(expired, std::time::Duration::from_millis(50));
        let expired_id =
            ui.0.incoming_dialogs
                .borrow()
                .keys()
                .next()
                .unwrap()
                .clone();
        let (newer, mut newer_answer) = incoming_fixture(peer.clone());
        ui.incoming_offer(newer);
        settle();
        assert!(expired_answer.try_recv().unwrap().is_empty());
        assert_eq!(ui.0.incoming_pending.get(), 1);
        assert_eq!(ui.0.incoming_dialogs.borrow().len(), 1);
        app.activate_action("accept-transfer", Some(&expired_id.to_variant()));
        assert!(newer_answer.try_recv().is_err());
        let newer_id =
            ui.0.incoming_dialogs
                .borrow()
                .keys()
                .next()
                .unwrap()
                .clone();
        app.activate_action("decline-transfer", Some(&newer_id.to_variant()));
        assert!(newer_answer.try_recv().unwrap().is_empty());
        settle();

        let (cancelled, mut cancelled_answer) = incoming_fixture(peer.clone());
        let cancellation = cancelled.cancellation().unwrap();
        ui.incoming_offer(cancelled);
        settle();
        let id =
            ui.0.incoming_dialogs
                .borrow()
                .keys()
                .next()
                .unwrap()
                .clone();
        cancellation.cancel();
        // No main-context iteration between withdrawal and click: the action
        // must check cancellation itself, before the watcher closes the dialog.
        app.activate_action("accept-transfer", Some(&id.to_variant()));
        assert!(cancelled_answer.try_recv().unwrap().is_empty());
        assert!(!ui.0.receive_view.has_active());
        settle();
        assert_eq!(ui.0.incoming_pending.get(), 0);
        assert!(ui.0.incoming_dialogs.borrow().is_empty());
        assert!(ui.0.window.visible_dialog().is_none());
    }
    #[test]
    #[ignore = "requires a Wayland compositor; see scripts/test-wayland.sh"]
    fn wayland_views_and_selection() {
        adw::init().unwrap();
        // An already open count label must reselect grammar on a locale switch.
        {
            i18n::set_locale(i18n::Locale::En);
            let count_label = gtk::Label::new(None);
            for count in [1, 2, 21] {
                i18n::bind_plural_property(
                    &count_label,
                    "label",
                    "{n} file",
                    "{n} files",
                    count,
                    &[],
                );
                let english = if count == 1 { "file" } else { "files" };
                assert_eq!(count_label.label(), format!("{count} {english}"));
                i18n::set_locale(i18n::Locale::Ru);
                let russian = if count == 2 { "файла" } else { "файл" };
                assert_eq!(count_label.label(), format!("{count} {russian}"));
                i18n::set_locale(i18n::Locale::ZhCn);
                assert_eq!(count_label.label(), format!("{count} 个文件"));
                i18n::set_locale(i18n::Locale::En);
                assert_eq!(count_label.label(), format!("{count} {english}"));
            }
            i18n::bind_property(&count_label, "label", "Finished");
            i18n::set_locale(i18n::Locale::Ru);
            assert_eq!(count_label.label(), i18n::tr("Finished"));
            i18n::set_locale(i18n::Locale::En);
            assert_eq!(count_label.label(), "Finished");
        }
        if std::env::var("ADW_DEBUG_HIGH_CONTRAST").as_deref() == Ok("1") {
            assert!(adw::StyleManager::default().is_high_contrast());
        }
        install_styles();
        let app = adw::Application::builder()
            .application_id("org.localsend.GtkVisualTest")
            .build();
        app.register(gio::Cancellable::NONE).unwrap();
        let ui = build(
            &app,
            Settings {
                alias: "Kind Apple".into(),
                theme: 1,
                language: i18n::Locale::En,
                save_dir: PathBuf::from("/home/user/Downloads"),
                ..Settings::default()
            },
            true,
        );
        settle();
        ui.request_hidden_start();
        assert!(!ui.0.window.is_visible());
        assert!(ui.0.background_hold.borrow().is_some());
        ui.fixture_tray_failure();
        assert!(!ui.0.window.is_visible());
        assert!(ui.0.background_hold.borrow().is_some());
        ui.show_window();
        ui.fixture_tray_failure();
        assert!(ui.0.window.is_visible());
        assert!(ui.0.background_hold.borrow().is_none());
        let font =
            ui.0.window
                .pango_context()
                .load_font(&gtk::pango::FontDescription::from_string("sans-serif 14"))
                .unwrap();
        assert!(font.describe().family().is_some());
        assert_eq!(ui.0.stack.visible_child_name().as_deref(), Some("receive"));
        receive_logo_animation(&ui);
        snapshot(&ui, "receive-light");
        ui.0.stack.set_visible_child_name("send");
        snapshot(&ui, "send-empty");
        ui.add_text("Hello from LocalSend".into());
        assert_eq!(ui.0.selection.borrow().len(), 1);
        assert!(ui.0.clear.is_visible());
        let mut peer = DeviceInfo::new(
            "Bright Orange".into(),
            53317,
            localsend_rs::protocol::Protocol::Https,
        );
        peer.ip = Some("192.168.1.24".into());
        peer.fingerprint = "a".repeat(64);
        peer.device_type = Some(DeviceType::Mobile);
        peer.device_model = Some("Android".into());
        incoming_dialog_buttons(&ui, &peer);
        native_dialog_transfers(&ui, &peer);
        incoming_notification_actions(&ui, &peer);
        ui.0.stack.set_visible_child_name("send");
        ui.add_peer(peer.clone());
        ui.add_peer(peer.clone());
        let mut uppercase_peer = peer.clone();
        uppercase_peer.fingerprint = peer.fingerprint.to_ascii_uppercase();
        ui.add_peer(uppercase_peer.clone());
        assert_eq!(ui.0.peers.borrow().len(), 1);
        snapshot(&ui, "send-selection");
        assert!(!ui.0.picker_choices.is_visible());
        assert!(ui.0.selection_actions.is_visible());
        ui.show_selection_editor();
        snapshot(&ui, "selection-editor");
        find_tooltip(ui.0.window.upcast_ref(), "Edit message")
            .unwrap()
            .downcast::<gtk::Button>()
            .unwrap()
            .emit_clicked();
        settle();
        find_text_view(ui.0.window.upcast_ref())
            .unwrap()
            .buffer()
            .set_text("Edited message");
        snapshot(&ui, "selection-edit-message");
        respond(&ui, "confirm");
        assert!(
            matches!(&ui.0.selection.borrow()[0].source, Source::Text(value) if value == "Edited message")
        );
        ui.0.window.visible_dialog().unwrap().force_close();
        settle();
        ui.show_add_selection();
        snapshot(&ui, "selection-add");
        ui.0.window.visible_dialog().unwrap().force_close();
        settle();
        ui.0.settings
            .borrow_mut()
            .favorites
            .push(crate::settings::FavoriteDevice::from_peer(&peer, "My phone").unwrap());
        ui.add_peer(peer.clone());
        let mut downgrade = peer.clone();
        downgrade.protocol = localsend_rs::protocol::Protocol::Http;
        ui.add_peer(downgrade.clone());
        assert_eq!(
            ui.0.peers
                .borrow()
                .get(&peer.fingerprint)
                .unwrap()
                .0
                .protocol,
            localsend_rs::protocol::Protocol::Https
        );
        ui.set_send_mode(1);
        ui.send(downgrade);
        assert!(ui.0.outgoing.borrow().is_empty());
        ui.set_send_mode(0);
        let mut other = peer.clone();
        other.fingerprint = "b".repeat(64);
        other.alias = "Calm Peach".into();
        other.ip = Some("192.168.1.25".into());
        ui.add_peer(other.clone());
        ui.set_send_mode(1);
        let token_a = ui.fixture_start_outgoing(peer.clone(), true).unwrap();
        let token_b = ui.fixture_start_outgoing(other.clone(), true).unwrap();
        assert!(ui.fixture_start_outgoing(peer.clone(), true).is_none());
        assert!(ui.fixture_start_outgoing(uppercase_peer, true).is_none());
        assert!(ui.fixture_start_outgoing(other.clone(), false).is_none());
        ui.fixture_outgoing_progress(&peer.fingerprint, 0.35);
        ui.fixture_outgoing_progress(&other.fingerprint, 0.7);
        snapshot(&ui, "send-multiple");
        assert!(ui.0.sending.get());
        ui.cancel_outgoing(&peer.fingerprint);
        assert!(!token_a.is_cancelled() && !token_b.is_cancelled());
        snapshot(&ui, "send-cancel-confirmation");
        respond(&ui, "continue");
        assert!(!token_a.is_cancelled() && !token_b.is_cancelled());
        ui.cancel_outgoing(&peer.fingerprint);
        respond(&ui, "cancel");
        assert!(token_a.is_cancelled() && !token_b.is_cancelled());
        ui.fixture_finish_outgoing(&peer.fingerprint, Err(transfer::SendError::Cancelled));
        assert!(ui.0.sending.get());
        ui.fixture_finish_outgoing(&other.fingerprint, Ok(1));
        assert!(!ui.0.sending.get());
        assert_eq!(ui.0.selection.borrow().len(), 1);
        ui.set_send_mode(0);
        ui.send(peer.clone());
        snapshot(&ui, "sending-waiting");
        assert_eq!(
            ui.0.window
                .visible_dialog()
                .unwrap()
                .downcast::<adw::AlertDialog>()
                .unwrap()
                .heading()
                .as_deref(),
            Some("Sending files")
        );
        ui.fixture_finish_outgoing(&peer.fingerprint, Ok(1));
        snapshot(&ui, "sending-finished");
        respond(&ui, "done");
        assert!(ui.0.selection.borrow().is_empty());
        ui.add_text("Hello from LocalSend".into());
        ui.fixture_start_outgoing(peer.clone(), false).unwrap();
        ui.add_text("Keep this newer selection".into());
        ui.fixture_finish_outgoing(&peer.fingerprint, Ok(1));
        assert_eq!(ui.0.selection.borrow().len(), 2);
        ui.0.selection.borrow_mut().pop();
        ui.refresh_selection();
        ui.0.cancel_send.emit_clicked();
        assert!(!ui.0.progress_box.is_visible());
        ui.show_device_details(peer.clone());
        snapshot(&ui, "device-details");
        ui.0.window.visible_dialog().unwrap().force_close();
        settle();
        ui.show_favorites();
        snapshot(&ui, "favorites");
        ui.0.window.visible_dialog().unwrap().force_close();
        settle();
        ui.show_manual_address();
        snapshot(&ui, "manual-address");
        ui.0.window.visible_dialog().unwrap().force_close();
        settle();
        ui.show_web_receive();
        ui.web_receive_ready(vec![
            "http://192.168.1.42:53318/0123456789abcdef0123456789abcdef/".into(),
        ]);
        snapshot(&ui, "receive-link");
        ui.web_receive_stopped();
        assert!(ui.0.web.borrow().is_none());
        settle();
        let (request, mut answer) = incoming_fixture(peer.clone());
        let ids = request
            .files()
            .values()
            .find(|file| file.file_name.starts_with('B'))
            .unwrap()
            .id
            .clone();
        ui.incoming_offer(request);
        settle();
        let dialog =
            ui.0.window
                .visible_dialog()
                .unwrap()
                .downcast::<adw::AlertDialog>()
                .unwrap();
        let mut checks = Vec::new();
        checkboxes(dialog.upcast_ref(), &mut checks);
        assert_eq!(checks.len(), 2);
        for check in &checks {
            check.set_active(false);
        }
        assert!(!dialog.is_response_enabled("accept"));
        checks[1].set_active(true);
        assert!(dialog.is_response_enabled("accept"));
        snapshot(&ui, "receive-consent");
        dialog.emit_by_name::<()>("response", &[&"accept"]);
        assert_eq!(answer.try_recv().unwrap(), vec![ids]);
        assert_eq!(ui.0.incoming_pending.get(), 0);
        dialog.force_close();
        settle();
        let (request, mut answer) = incoming_fixture(peer.clone());
        let cancellation = request.cancellation().unwrap();
        ui.incoming_offer(request);
        settle();
        cancellation.cancel();
        settle();
        assert!(ui.0.window.visible_dialog().is_none());
        assert!(answer.try_recv().unwrap().is_empty());
        assert_eq!(ui.0.incoming_pending.get(), 0);
        ui.0.receive_view.cancel_acknowledged();
        find_button(ui.0.receive_view.widget().upcast_ref(), "Done")
            .unwrap()
            .emit_clicked();
        assert!(!ui.0.receive_view.widget().is_visible());
        ui.show_web_share();
        ui.web_share_ready(vec![
            "http://192.168.1.42:53319/0123456789abcdef0123456789abcdef/".into(),
        ]);
        snapshot(&ui, "share-link");
        let share_dialog = ui.0.window.visible_dialog().unwrap();
        let mut share_checks = Vec::new();
        checkboxes(share_dialog.upcast_ref(), &mut share_checks);
        let auto_accept = share_checks
            .iter()
            .find(|check| check.label().as_deref() == Some("Automatically accept requests"))
            .unwrap();
        auto_accept.set_active(true);
        assert!(ui.0.settings.borrow().share_auto_accept);
        let auto_row =
            find_switch(ui.0.window.upcast_ref(), "Share via link: auto accept").unwrap();
        assert!(auto_row.is_active());
        auto_row.set_active(false);
        assert!(!auto_accept.is_active());
        assert!(!ui.0.settings.borrow().share_auto_accept);
        let fixture_files = vec![crate::web_share::SharedFile {
            id: "one".into(),
            name: "Message.txt".into(),
            size: 20,
        }];
        let (request, mut answer) = crate::web_share::PendingDownload::fixture(
            "192.168.1.24".parse().unwrap(),
            fixture_files.clone(),
        );
        ui.web_share_event(crate::web_share::ShareEvent::DownloadRequest(request));
        assert_eq!(ui.0.incoming_pending.get(), 1);
        snapshot(&ui, "share-approval");
        let dialog =
            ui.0.window
                .visible_dialog()
                .unwrap()
                .downcast::<adw::AlertDialog>()
                .unwrap();
        dialog.emit_by_name::<()>("response", &[&"allow"]);
        assert!(answer.try_recv().unwrap());
        assert_eq!(ui.0.incoming_pending.get(), 0);
        dialog.force_close();
        settle();
        let (request, mut answer) = crate::web_share::PendingDownload::fixture(
            "192.168.1.24".parse().unwrap(),
            fixture_files,
        );
        let cancel = request.cancellation();
        ui.web_share_event(crate::web_share::ShareEvent::DownloadRequest(request));
        settle();
        cancel.cancel();
        settle();
        assert!(!answer.try_recv().unwrap());
        assert_eq!(ui.0.incoming_pending.get(), 0);
        ui.web_share_stopped();
        settle();
        ui.0.stack.set_visible_child_name("receive");
        let detail_session = localsend_rs::protocol::SessionId::new();
        let detail_first = localsend_rs::protocol::FileId::new();
        let detail_second = localsend_rs::protocol::FileId::new();
        ui.0.receive_view.begin_offer(
            (uuid::Uuid::new_v4().to_string(), true),
            peer.alias.clone(),
            Some(PathBuf::from("/home/user/Downloads")),
            vec![
                receive_progress::ReceiveFile::new(
                    detail_first.to_string(),
                    "A photograph.jpg",
                    1_024,
                ),
                receive_progress::ReceiveFile::new(detail_second.to_string(), "B notes.txt", 2_048),
            ],
        );
        ui.network_event(Event::Server(ServerEvent::FileReceiveProgress {
            session_id: detail_session.clone(),
            file_id: detail_first.clone(),
            file_name: "A photograph.jpg".into(),
            sender_alias: peer.alias.clone(),
            bytes_received: 512,
            total_bytes: 3_072,
            file_bytes_received: 512,
            file_size: 1_024,
            file_count: 2,
        }));
        find_button(ui.0.receive_view.widget().upcast_ref(), "Show details")
            .unwrap()
            .emit_clicked();
        ui.0.window.set_default_size(360, 540);
        settle();
        ui.0.window.set_default_size(360, 540);
        settle();
        let details = ui.0.window.visible_dialog().unwrap();
        let mut detail_labels = Vec::new();
        labels(details.upcast_ref(), &mut detail_labels);
        assert!(detail_labels.iter().any(|text| text == "Files: 0 / 2"));
        assert!(detail_labels
            .iter()
            .any(|text| text == "Size: 512 B / 3.1 KB"));
        assert!(detail_labels
            .iter()
            .any(|text| text == "Size: 512 B / 1.0 KB"));
        assert!(detail_labels
            .iter()
            .any(|text| text == "Size: 0 B / 2.0 KB"));
        let mut bars = Vec::new();
        progress_bars(details.upcast_ref(), &mut bars);
        assert!(bars
            .iter()
            .any(|bar| (bar.fraction() - (512.0 / 3_072.0)).abs() < 0.001));
        assert!(bars.iter().any(|bar| (bar.fraction() - 0.5).abs() < 0.001));
        snapshot(&ui, "receiving-details-small");
        assert!(
            ui.0.window.width() <= 360,
            "Details must fit a 360 px window"
        );
        assert!(
            ui.0.window.height() <= 540,
            "Details must fit a 540 px window"
        );
        respond(&ui, "close");
        ui.0.receive_view.file_received(
            detail_session.as_str(),
            detail_first.as_str(),
            "A photograph.jpg".into(),
            PathBuf::from("/home/user/Downloads/A photograph.jpg"),
            1_024,
            peer.alias.clone(),
        );
        ui.network_event(Event::Server(ServerEvent::FileReceiveProgress {
            session_id: detail_session.clone(),
            file_id: detail_second.clone(),
            file_name: "B notes.txt".into(),
            sender_alias: peer.alias.clone(),
            bytes_received: 3_072,
            total_bytes: 3_072,
            file_bytes_received: 2_048,
            file_size: 2_048,
            file_count: 2,
        }));
        ui.0.receive_view.file_received(
            detail_session.as_str(),
            detail_second.as_str(),
            "B notes.txt".into(),
            PathBuf::from("/home/user/Downloads/B notes.txt"),
            2_048,
            peer.alias.clone(),
        );
        // A completion that wins the wire race with local cancellation remains
        // successful when its terminal event arrives.
        ui.0.receive_view.mark_canceling();
        ui.network_event(Event::Server(ServerEvent::SessionDone {
            session_id: detail_session,
        }));
        assert!(!ui.0.receive_view.has_active());
        let mut terminal_labels = Vec::new();
        labels(
            ui.0.receive_view.widget().upcast_ref(),
            &mut terminal_labels,
        );
        assert!(terminal_labels.iter().any(|text| text == "Finished"));
        snapshot(&ui, "receiving-finished-small");
        assert!(
            ui.0.window.width() <= 360 && ui.0.window.height() <= 540,
            "Finished receive feedback must fit a 360×540 window"
        );
        find_button(ui.0.receive_view.widget().upcast_ref(), "Done")
            .unwrap()
            .emit_clicked();
        assert!(!ui.0.receive_view.widget().is_visible());
        // An unrelated browser rejection or inline-message terminal must not
        // cancel a native offer accepted before its first upload arrives.
        let offered = uuid::Uuid::new_v4().to_string();
        let pending_file = localsend_rs::protocol::FileId::new();
        ui.0.receive_view.begin_offer(
            (offered.clone(), true),
            "Shared alias",
            None,
            vec![receive_progress::ReceiveFile::new(
                pending_file.to_string(),
                "pending.txt",
                16,
            )],
        );
        let pending_keys = ui.0.receive_view.active_session_keys();
        ui.0.receive_view
            .session_done("unrelated-browser-rejection");
        assert_eq!(ui.0.receive_view.active_session_keys(), pending_keys);
        // A different native/automatic session using the same alias must not
        // consume that accepted offer merely because its sender name matches.
        ui.0.receive_view.progress(
            "unrelated-session",
            "unrelated-file",
            "other.txt".into(),
            "Shared alias".into(),
            1,
            1,
            1,
            1,
            1,
        );
        assert_eq!(ui.0.receive_view.active_session_keys().len(), 2);
        ui.0.receive_view.session_done("unrelated-session");
        assert_eq!(ui.0.receive_view.active_session_keys(), pending_keys);
        // The tracked identity still handles cancellation before any bytes.
        ui.0.receive_view.session_done(&offered);
        assert!(!ui.0.receive_view.has_active());
        while let Some(done) = find_button(ui.0.receive_view.widget().upcast_ref(), "Done") {
            done.emit_clicked();
        }
        let original = uuid::Uuid::new_v4().to_string();
        ui.0.receive_view.begin_offer(
            (original.clone(), false),
            "Original browser",
            None,
            vec![receive_progress::ReceiveFile::new(
                "original-file",
                "one.txt",
                1,
            )],
        );
        ui.0.cancel_receive.emit_clicked();
        ui.0.receive_view.session_done(&original);
        let replacement = uuid::Uuid::new_v4().to_string();
        ui.0.receive_view.begin_offer(
            (replacement.clone(), false),
            "Replacement browser",
            None,
            vec![receive_progress::ReceiveFile::new(
                "replacement-file",
                "two.txt",
                1,
            )],
        );
        respond(&ui, "cancel");
        assert!(
            ui.0.receive_view.has_active(),
            "Stale cancel must preserve a replacement receive"
        );
        // Replacing a pending offer leaves the count unchanged. Its new
        // identity must still invalidate a confirmation for the prior offer.
        let (request_x, mut answer_x) = incoming_fixture(peer.clone());
        let withdraw_x = request_x.cancellation().unwrap();
        ui.incoming_offer(request_x);
        settle();
        assert_eq!(ui.0.incoming_pending.get(), 1);
        ui.0.cancel_receive.emit_clicked();
        settle();
        let old_confirmation =
            ui.0.window
                .visible_dialog()
                .unwrap()
                .downcast::<adw::AlertDialog>()
                .unwrap();
        withdraw_x.cancel();
        settle();
        assert!(answer_x.try_recv().unwrap().is_empty());
        let (request_y, mut answer_y) = incoming_fixture(peer.clone());
        ui.incoming_offer(request_y);
        settle();
        assert_eq!(ui.0.incoming_pending.get(), 1);
        let pending_y =
            ui.0.window
                .visible_dialog()
                .unwrap()
                .downcast::<adw::AlertDialog>()
                .unwrap();
        old_confirmation.emit_by_name::<()>("response", &[&"cancel"]);
        old_confirmation.force_close();
        assert!(
            ui.0.receive_view.has_active(),
            "A replacement pending offer must invalidate the old cancellation"
        );
        assert!(
            answer_y.try_recv().is_err(),
            "Replacement offer must remain undecided"
        );
        pending_y.emit_by_name::<()>("response", &[&"decline"]);
        pending_y.force_close();
        settle();
        assert!(answer_y.try_recv().unwrap().is_empty());
        assert_eq!(ui.0.incoming_pending.get(), 0);
        ui.0.receive_view.session_done(&replacement);
        while let Some(done) = find_button(ui.0.receive_view.widget().upcast_ref(), "Done") {
            done.emit_clicked();
        }
        assert!(!ui.0.receive_view.widget().is_visible());
        ui.0.receive_view.file_received(
            &replacement,
            "replacement-file",
            "two.txt".into(),
            PathBuf::from("/tmp/two.txt"),
            1,
            "Replacement browser".into(),
        );
        assert!(
            !ui.0.receive_view.widget().is_visible(),
            "Late receipt must not revive a dismissed card"
        );
        ui.0.receive_view
            .session_done("cancelled-before-first-progress");
        ui.0.receive_view.progress(
            "cancelled-before-first-progress",
            "late-file",
            "late.txt".into(),
            "Late sender".into(),
            1,
            1,
            1,
            1,
            1,
        );
        assert!(
            !ui.0.receive_view.has_active(),
            "Late progress must not revive an unbound cancelled session"
        );
        ui.0.window.set_default_size(1000, 700);
        ui.0.stack.set_visible_child_name("send");
        settle();
        let session_id = localsend_rs::protocol::SessionId::new();
        ui.network_event(Event::Server(ServerEvent::FileReceiveProgress {
            session_id: session_id.clone(),
            file_id: localsend_rs::protocol::FileId::new(),
            file_name: "Holiday.jpg".into(),
            sender_alias: peer.alias.clone(),
            bytes_received: 512,
            total_bytes: 1024,
            file_bytes_received: 512,
            file_size: 1024,
            file_count: 1,
        }));
        snapshot(&ui, "receiving-progress");
        ui.0.cancel_receive.emit_clicked();
        respond(&ui, "continue");
        assert!(ui.0.receive_view.widget().is_visible());
        assert!(ui.0.receive_view.has_active());
        ui.0.cancel_receive.emit_clicked();
        respond(&ui, "cancel");
        assert!(!ui.0.receive_view.has_active());
        assert!(ui.0.receive_view.widget().is_visible());
        ui.network_event(Event::Server(ServerEvent::SessionDone { session_id }));
        assert!(ui.0.receive_view.widget().is_visible());
        find_button(ui.0.receive_view.widget().upcast_ref(), "Done")
            .unwrap()
            .emit_clicked();
        assert!(!ui.0.receive_view.widget().is_visible());
        // Only received items enter history, and completed files retain their
        // actual destination for details, opening and revealing in the folder.
        assert!(ui.0.history.borrow().entries().is_empty());
        let downloads = tempfile::tempdir().unwrap();
        let saved_path = downloads.path().join("Holiday.jpg");
        std::fs::write(&saved_path, b"received bytes").unwrap();
        let history_session = localsend_rs::protocol::SessionId::new();
        ui.network_event(Event::Server(ServerEvent::FileReceived {
            session_id: history_session.clone(),
            file_id: localsend_rs::protocol::FileId::new(),
            file_name: "Holiday.jpg".into(),
            path: saved_path.clone(),
            size: 14,
            sender_alias: peer.alias.clone(),
            message_text: None,
        }));
        ui.network_event(Event::Server(ServerEvent::SessionDone {
            session_id: history_session,
        }));
        ui.network_event(Event::TextReceived {
            text: "Send".into(),
            sender_alias: "Settings".into(),
            preview_handled: true,
        });
        let ids: Vec<_> =
            ui.0.history
                .borrow()
                .entries()
                .iter()
                .map(|entry| entry.id.clone())
                .collect();
        assert_eq!(ids.len(), 2);
        let save_history = find_switch(ui.0.window.upcast_ref(), "Save to history").unwrap();
        save_history.set_active(false);
        ui.network_event(Event::TextReceived {
            text: "Not recorded".into(),
            sender_alias: peer.alias.clone(),
            preview_handled: true,
        });
        assert_eq!(ui.0.history.borrow().entries().len(), 2);
        save_history.set_active(true);
        ui.show_history();
        snapshot(&ui, "receive-history");
        let history_dialog = ui.0.window.visible_dialog().unwrap();
        find_named(history_dialog.upcast_ref(), &ids[1])
            .unwrap()
            .downcast::<gtk::Button>()
            .unwrap()
            .emit_clicked();
        let message =
            ui.0.window
                .visible_dialog()
                .unwrap()
                .downcast::<adw::AlertDialog>()
                .unwrap();
        assert_eq!(message.heading().as_deref(), Some("Settings"));
        respond(&ui, "copy");
        assert_eq!(
            glib::MainContext::default()
                .block_on(ui.0.window.clipboard().read_text_future())
                .unwrap()
                .as_deref(),
            Some("Send")
        );
        let options = find_named(
            history_dialog.upcast_ref(),
            &format!("history-options-{}", ids[0]),
        )
        .unwrap()
        .downcast::<gtk::MenuButton>()
        .unwrap();
        options.popup();
        settle();
        find_button(options.popover().unwrap().upcast_ref(), "Information")
            .unwrap()
            .emit_clicked();
        snapshot(&ui, "receive-history-file-information");
        let info =
            ui.0.window
                .visible_dialog()
                .unwrap()
                .downcast::<adw::AlertDialog>()
                .unwrap();
        assert_eq!(info.heading().as_deref(), Some("File information"));
        respond(&ui, "close");
        i18n::set_locale(i18n::Locale::ZhCn);
        snapshot(&ui, "receive-history-chinese");
        assert_eq!(
            find_named(history_dialog.upcast_ref(), &ids[1])
                .unwrap()
                .downcast::<gtk::Button>()
                .unwrap()
                .child()
                .unwrap()
                .last_child()
                .unwrap()
                .first_child()
                .unwrap()
                .downcast::<gtk::Label>()
                .unwrap()
                .label()
                .as_str(),
            "Send"
        );
        i18n::set_locale(i18n::Locale::En);
        ui.0.window.set_default_size(360, 540);
        settle();
        ui.0.window.set_default_size(360, 540);
        snapshot(&ui, "receive-history-small");
        assert!(ui.0.window.width() <= 360 && ui.0.window.height() <= 540);
        options.popup();
        settle();
        find_button(options.popover().unwrap().upcast_ref(), "Information")
            .unwrap()
            .emit_clicked();
        snapshot(&ui, "receive-history-information-small");
        assert!(ui.0.window.width() <= 360 && ui.0.window.height() <= 540);
        respond(&ui, "close");
        ui.0.window.set_default_size(1000, 700);
        settle();
        options.popup();
        settle();
        find_button(
            options.popover().unwrap().upcast_ref(),
            "Delete from history",
        )
        .unwrap()
        .emit_clicked();
        assert_eq!(ui.0.history.borrow().entries().len(), 1);
        assert!(saved_path.exists());
        find_button(history_dialog.upcast_ref(), "Delete history")
            .unwrap()
            .emit_clicked();
        respond(&ui, "cancel");
        assert_eq!(ui.0.history.borrow().entries().len(), 1);
        find_button(history_dialog.upcast_ref(), "Delete history")
            .unwrap()
            .emit_clicked();
        snapshot(&ui, "receive-history-clear-confirmation");
        respond(&ui, "delete");
        assert!(ui.0.history.borrow().entries().is_empty());
        assert!(saved_path.exists());
        snapshot(&ui, "receive-history-empty");
        history_dialog.close();
        settle();
        ui.0.clear.emit_clicked();
        assert!(ui.0.selection.borrow().is_empty());
        assert!(!ui.0.clear.is_visible());
        ui.0.stack.set_visible_child_name("settings");
        snapshot(&ui, "settings-light");
        find_theme(ui.0.window.upcast_ref())
            .unwrap()
            .set_selected(2);
        assert_eq!(ui.0.settings.borrow().theme, 2);
        snapshot(&ui, "settings-dark");
        find_color(ui.0.window.upcast_ref())
            .unwrap()
            .set_selected(1);
        assert!(ui.0.settings.borrow().oled);
        ui.0.stack.set_visible_child_name("receive");
        snapshot(&ui, "receive-oled");
        let color_row = find_color(ui.0.window.upcast_ref()).unwrap();
        color_row.set_selected(2);
        ui.0.stack.set_visible_child_name("settings");
        snapshot(&ui, "settings-yaru-dark");
        color_row.set_selected(3);
        let custom = find_entry(ui.0.window.upcast_ref(), "Custom color (#RRGGBB)").unwrap();
        let original = ui.0.settings.borrow().custom_color.clone();
        custom.set_text("not a color");
        custom.emit_by_name::<()>("apply", &[]);
        assert_eq!(ui.0.settings.borrow().custom_color, original);
        assert!(custom.has_css_class("error"));
        custom.set_text("#6750a4");
        custom.emit_by_name::<()>("apply", &[]);
        assert_eq!(ui.0.settings.borrow().custom_color, "#6750A4");
        assert!(!custom.has_css_class("error"));
        snapshot(&ui, "settings-custom-dark");
        find_theme(ui.0.window.upcast_ref())
            .unwrap()
            .set_selected(1);
        snapshot(&ui, "settings-custom-light");
        find_theme(ui.0.window.upcast_ref())
            .unwrap()
            .set_selected(2);
        find_color(ui.0.window.upcast_ref())
            .unwrap()
            .set_selected(0);
        let tray = find_switch(ui.0.window.upcast_ref(), "Show tray icon").unwrap();
        tray.set_active(true);
        let close_to_tray = find_switch(ui.0.window.upcast_ref(), "Minimize to tray").unwrap();
        close_to_tray.set_active(true);
        assert!(!ui.hide_in_tray());
        ui.hide_to_background();
        assert!(!ui.0.window.is_visible());
        assert!(ui.0.background_hold.borrow().is_some());
        ui.show_window();
        let auto = find_switch(ui.0.window.upcast_ref(), "Launch at startup").unwrap();
        auto.set_active(true);
        let minimized = find_switch(ui.0.window.upcast_ref(), "Launch minimized").unwrap();
        tray.set_active(false);
        assert!(close_to_tray.is_sensitive());
        assert!(minimized.is_sensitive());
        minimized.set_active(true);
        assert!(ui.0.settings.borrow().start_minimized);
        assert!(ui.0.settings.borrow().autostart);
        minimized.set_active(false);
        auto.set_active(false);
        assert!(!minimized.is_sensitive());
        assert!(close_to_tray.is_sensitive());
        {
            let mut settings = ui.0.settings.borrow_mut();
            settings.autostart = true;
            settings.start_minimized = true;
        }
        let (sandbox_auto, sandbox_minimized) = ui.startup_rows(false);
        for row in [&sandbox_auto, &sandbox_minimized] {
            assert!(!row.is_active());
            assert!(!row.is_sensitive());
            assert_eq!(
                row.subtitle().as_deref(),
                Some("Launch at startup is not available in Flatpak yet")
            );
            // Programmatic activation must not persist a false enabled state.
            row.set_active(true);
            assert!(!row.is_active());
        }
        assert!(!ui.0.settings.borrow().autostart);
        assert!(!ui.0.settings.borrow().start_minimized);
        *ui.0.background_hold.borrow_mut() = Some(app.hold());
        ui.0.window.set_visible(false);
        ui.0.window.present();
        settle();
        assert!(ui.0.background_hold.borrow().is_none());
        ui.0.hidden_start.set(true);
        gtk::prelude::WidgetExt::activate_action(&ui.0.window, "win.show", None).unwrap();
        assert!(!ui.0.hidden_start.get());
        ui.0.stack.set_visible_child_name("receive");
        snapshot(&ui, "receive-dark");
        ui.0.window.set_default_size(750, 700);
        snapshot(&ui, "receive-compact");
        ui.0.window.set_default_size(390, 700);
        snapshot(&ui, "receive-narrow");
        assert_receive_action_uncovered(&ui);
        assert!(ui.0.window.width() < 700, "Narrow layout must be reachable");
        ui.0.stack.set_visible_child_name("send");
        snapshot(&ui, "send-narrow");
        assert!(
            ui.0.window.width() < 700,
            "Send toolbar must fit narrow layouts"
        );
        ui.show_manual_address();
        snapshot(&ui, "manual-address-narrow");
        ui.0.window.visible_dialog().unwrap().force_close();
        settle();
        ui.show_web_receive();
        ui.web_receive_ready(vec![
            "http://192.168.1.42:53318/0123456789abcdef0123456789abcdef/".into(),
        ]);
        snapshot(&ui, "receive-link-narrow");
        ui.web_receive_stopped();
        settle();
        ui.add_text("Hello from LocalSend".into());
        ui.show_web_share();
        ui.web_share_ready(vec![
            "http://192.168.1.42:53319/0123456789abcdef0123456789abcdef/".into(),
        ]);
        snapshot(&ui, "share-link-narrow");
        ui.web_share_stopped();
        settle();
        ui.0.window.set_default_size(360, 540);
        ui.0.stack.set_visible_child_name("receive");
        ui.toast_text("Ready to receive. Your files stay on this device.");
        snapshot(&ui, "receive-small-toast");
        assert!(
            ui.0.window.width() <= 360,
            "Receive must fit a 360 px window"
        );
        assert!(
            ui.0.window.height() <= 540,
            "Receive must fit a short window"
        );
        assert_receive_action_uncovered(&ui);
        ui.0.alias_label
            .set_label("A very long device name with 中文 and more words for a small display");
        snapshot(&ui, "receive-long-name-small");
        assert!(ui.0.window.width() <= 360 && ui.0.window.height() <= 540);
        ui.0.stack.set_visible_child_name("send");
        snapshot(&ui, "send-small");
        ui.0.stack.set_visible_child_name("settings");
        snapshot(&ui, "settings-small");
        assert!(ui.0.window.width() <= 360 && ui.0.window.height() <= 540);
        ui.0.stack.set_visible_child_name("send");
        ui.show_manual_address();
        snapshot(&ui, "manual-address-small");
        assert!(ui.0.window.width() <= 360 && ui.0.window.height() <= 540);
        ui.0.window.visible_dialog().unwrap().force_close();
        settle();
        ui.show_web_receive();
        ui.web_receive_ready(vec![
            "http://192.168.1.42:53318/0123456789abcdef0123456789abcdef/".into(),
        ]);
        snapshot(&ui, "receive-link-small");
        assert!(ui.0.window.width() <= 360 && ui.0.window.height() <= 540);
        ui.web_receive_stopped();
        settle();
        ui.show_web_share();
        ui.web_share_ready(vec![
            "http://192.168.1.42:53319/0123456789abcdef0123456789abcdef/".into(),
        ]);
        snapshot(&ui, "share-link-small");
        assert!(ui.0.window.width() <= 360 && ui.0.window.height() <= 540);
        ui.web_share_stopped();
        settle();
        let (request, mut answer) = incoming_fixture(peer.clone());
        ui.incoming_offer(request);
        snapshot(&ui, "receive-consent-small");
        assert!(ui.0.window.width() <= 360 && ui.0.window.height() <= 540);
        ui.0.window.visible_dialog().unwrap().force_close();
        settle();
        assert!(answer.try_recv().unwrap().is_empty());
        ui.0.clear.emit_clicked();
        assert!(ui.0.picker_choices.is_visible());
        let resumed = Rc::new(Cell::new(0));
        let counter = resumed.clone();
        ui.after_selection(move || counter.set(counter.get() + 1));
        settle();
        ui.0.window.visible_dialog().unwrap().force_close();
        settle();
        ui.add_text("Does not resume a canceled device action".into());
        assert_eq!(resumed.get(), 0);
        ui.0.clear.emit_clicked();
        let counter = resumed.clone();
        ui.after_selection(move || counter.set(counter.get() + 1));
        settle();
        find_tooltip(ui.0.window.upcast_ref(), "Text")
            .unwrap()
            .downcast::<gtk::Button>()
            .unwrap()
            .emit_clicked();
        settle();
        find_text_view(ui.0.window.upcast_ref())
            .unwrap()
            .buffer()
            .set_text("Resume after selection");
        respond(&ui, "add");
        assert_eq!(resumed.get(), 1);
        ui.add_text("A later selection".into());
        assert_eq!(resumed.get(), 1);
        ui.show_selection_editor();
        snapshot(&ui, "selection-editor-small");
        assert!(ui.0.window.width() <= 360 && ui.0.window.height() <= 540);
        ui.0.window.visible_dialog().unwrap().force_close();
        settle();
        let (answer, mut received) = tokio::sync::oneshot::channel();
        ui.request_pin(
            "Bright Orange",
            transfer::PinRequest {
                invalid: false,
                answer,
                cancellation: CancellationToken::new(),
            },
        );
        snapshot(&ui, "pin-required-small");
        let dialog =
            ui.0.window
                .visible_dialog()
                .unwrap()
                .downcast::<adw::AlertDialog>()
                .unwrap();
        assert!(!dialog.is_response_enabled("confirm"));
        dialog
            .extra_child()
            .unwrap()
            .downcast::<gtk::PasswordEntry>()
            .unwrap()
            .set_text("A1b+Z6");
        respond(&ui, "confirm");
        assert_eq!(received.try_recv().unwrap().as_deref(), Some("A1b+Z6"));
        let (answer, mut received) = tokio::sync::oneshot::channel();
        let cancellation = CancellationToken::new();
        ui.request_pin(
            "Bright Orange",
            transfer::PinRequest {
                invalid: true,
                answer,
                cancellation: cancellation.clone(),
            },
        );
        settle();
        cancellation.cancel();
        settle();
        assert!(ui.0.window.visible_dialog().is_none());
        assert_eq!(received.try_recv().unwrap(), None);
        ui.show_received_message("Hello from LocalSend".into(), "Bright Orange".into());
        snapshot(&ui, "received-message-small");
        respond(&ui, "copy");
        let copied = glib::MainContext::default()
            .block_on(ui.0.window.clipboard().read_text_future())
            .unwrap()
            .unwrap();
        assert_eq!(copied.as_str(), "Hello from LocalSend");
        ui.show_received_message("https://localsend.org".into(), "Bright Orange".into());
        snapshot(&ui, "received-link-small");
        assert!(ui
            .0
            .window
            .visible_dialog()
            .unwrap()
            .downcast::<adw::AlertDialog>()
            .unwrap()
            .has_response("open"));
        respond(&ui, "close");
        ui.network_event(Event::TextReceived {
            text: "Already displayed".into(),
            sender_alias: "Bright Orange".into(),
            preview_handled: true,
        });
        assert!(ui.0.window.visible_dialog().is_none());
        ui.0.clear.emit_clicked();
        let old_calls = Rc::new(Cell::new(0));
        let old_count = old_calls.clone();
        ui.after_selection(move || old_count.set(old_count.get() + 1));
        settle();
        let old_request = ui.pending_selection_id();
        let old_dialog = ui.0.window.visible_dialog().unwrap();
        let new_calls = Rc::new(Cell::new(0));
        let new_count = new_calls.clone();
        ui.after_selection(move || new_count.set(new_count.get() + 1));
        settle();
        let new_request = ui.pending_selection_id();
        old_dialog.force_close();
        settle();
        ui.cancel_selection_request(old_request);
        ui.add_text_for("Stale file picker result".into(), old_request);
        assert!(ui.0.selection.borrow().is_empty());
        assert_eq!(ui.pending_selection_id(), new_request);
        ui.add_text_for("Current file picker result".into(), new_request);
        assert_eq!(old_calls.get(), 0);
        assert_eq!(new_calls.get(), 1);
        assert!(ui.pending_selection_id().is_none());
        ui.0.window.visible_dialog().unwrap().force_close();
        settle();
        // Live localization preserves user content, selection and in-flight sends.
        ui.0.window.set_default_size(1000, 700);
        ui.0.stack.set_visible_child_name("settings");
        show_settings_group(&ui, "network");
        let alias = find_entry(ui.0.window.upcast_ref(), "Device name").unwrap();
        let saved_alias = ui.0.settings.borrow().alias.clone();
        alias.set_text("New local name");
        assert_eq!(
            ui.0.settings.borrow().alias,
            saved_alias,
            "Network drafts wait for Restart"
        );
        snapshot(&ui, "settings-network-pending");
        find_button(ui.0.window.upcast_ref(), "Restart")
            .unwrap()
            .emit_clicked();
        assert_eq!(ui.0.settings.borrow().alias, "New local name");
        assert_eq!(ui.0.alias_label.label().as_str(), "New local name");
        find_button(ui.0.window.upcast_ref(), "Stop")
            .unwrap()
            .emit_clicked();
        assert_eq!(ui.0.online.label().as_str(), "Offline");
        let start = find_button(ui.0.window.upcast_ref(), "Start").unwrap();
        assert!(start.is_visible() && start.is_sensitive());
        snapshot(&ui, "settings-network-stopped");
        start.emit_clicked();
        assert!(!start.is_visible());
        snapshot(&ui, "settings-network-started");
        show_settings_group(&ui, "receive");
        snapshot(&ui, "settings-receive");
        show_settings_group(&ui, "send");
        snapshot(&ui, "settings-send");
        show_settings_group(&ui, "general");
        let language = find_combo(ui.0.window.upcast_ref(), "Language").unwrap();
        let theme = find_theme(ui.0.window.upcast_ref()).unwrap();
        let color = find_color(ui.0.window.upcast_ref()).unwrap();
        theme.set_selected(1);
        color.set_selected(3);
        ui.0.clear.emit_clicked();
        ui.add_text("Send".into());
        ui.0.selection.borrow_mut().push(Selection {
            name: "Holiday.jpg".into(),
            size: 10_000_000,
            source: Source::Text("A photograph fixture".into()),
        });
        ui.refresh_selection();
        let generation = ui.0.selection_generation.get();
        let token = ui.fixture_start_outgoing(peer.clone(), true).unwrap();
        ui.0.stack.set_visible_child_name("settings");
        language.set_selected(2);
        settle();
        assert_eq!(i18n::current(), i18n::Locale::ZhCn);
        assert_eq!(ui.0.settings.borrow().language, i18n::Locale::ZhCn);
        assert_eq!(
            (theme.selected(), color.selected(), language.selected()),
            (1, 3, 2)
        );
        assert_eq!(ui.0.settings.borrow().theme, 1);
        assert_eq!(ui.0.selection_generation.get(), generation);
        assert!(
            matches!(&ui.0.selection.borrow()[0].source, Source::Text(value) if value == "Send")
        );
        assert!(!token.is_cancelled());
        snapshot(&ui, "settings-chinese");
        ui.0.stack.set_visible_child_name("send");
        snapshot(&ui, "send-chinese");
        ui.show_received_message("Send".into(), "Settings".into());
        let message =
            ui.0.window
                .visible_dialog()
                .unwrap()
                .downcast::<adw::AlertDialog>()
                .unwrap();
        assert_eq!(message.heading().as_deref(), Some("Settings"));
        assert_eq!(message.response_label("copy").as_str(), "复制");
        language.set_selected(3);
        settle();
        assert_eq!(message.heading().as_deref(), Some("Settings"));
        assert_eq!(message.response_label("copy").as_str(), "複製");
        assert!(!token.is_cancelled());
        snapshot(&ui, "received-message-traditional-chinese");
        message.force_close();
        settle();
        ui.0.stack.set_visible_child_name("settings");
        for (index, locale, name) in [
            (4, i18n::Locale::Ja, "settings-japanese"),
            (5, i18n::Locale::Ko, "settings-korean"),
        ] {
            language.set_selected(index);
            settle();
            assert_eq!(i18n::current(), locale);
            assert_eq!(
                (theme.selected(), color.selected(), language.selected()),
                (1, 3, index)
            );
            snapshot(&ui, name);
        }
        language.set_selected(1);
        assert_eq!(i18n::current(), i18n::Locale::En);
        let mut report = transfer::TransferProgress::queued(&ui.0.selection.borrow());
        report.files[0].outcome = transfer::FileOutcome::Finished;
        report.files[0].bytes_sent = 4;
        report.files[1].outcome = transfer::FileOutcome::Sending;
        report.files[1].bytes_sent = 3_000_000;
        report.bytes_sent = 3_000_004;
        report.current_file = Some(1);
        report.finished_files = 1;
        report.elapsed = std::time::Duration::from_secs(8);
        report.bytes_per_second = 375_000.5;
        report.remaining = Some(std::time::Duration::from_secs(19));
        report.started = true;
        ui.fixture_outgoing_report(&peer.fingerprint, report);
        ui.0.stack.set_visible_child_name("send");
        ui.send(peer.clone());
        snapshot(&ui, "sending-file-details");
        language.set_selected(2);
        snapshot(&ui, "sending-file-details-chinese");
        assert!(!token.is_cancelled());
        language.set_selected(1);
        respond(&ui, "close");
        ui.fixture_finish_outgoing(&peer.fingerprint, Ok(2));
        ui.0.selection.replace(vec![Selection {
            name: format!("{}.jpg", "LongFileName".repeat(18)),
            size: 5_000_000,
            source: Source::Text("A long-name fixture".into()),
        }]);
        ui.refresh_selection();
        let token = ui.fixture_start_outgoing(peer.clone(), true).unwrap();
        let mut report = transfer::TransferProgress::queued(&ui.0.selection.borrow());
        report.files[0].outcome = transfer::FileOutcome::Sending;
        report.files[0].bytes_sent = 1_000_000;
        report.bytes_sent = 1_000_000;
        report.current_file = Some(0);
        report.elapsed = std::time::Duration::from_secs(5);
        report.bytes_per_second = 200_000.0;
        report.remaining = Some(std::time::Duration::from_secs(20));
        report.started = true;
        ui.fixture_outgoing_report(&peer.fingerprint, report);
        ui.0.window.set_default_size(360, 540);
        settle();
        // GTK retains the intermediate size when the desktop rail collapses.
        // First prove the new content minimum fits, then request the final size.
        let minimum = ui.0.window.measure(gtk::Orientation::Horizontal, -1).0;
        assert!(minimum <= 360, "Long-file content requires {minimum}px");
        ui.0.window.set_default_size(360, 540);
        settle();
        ui.send(peer.clone());
        snapshot(&ui, "sending-long-file-small");
        assert!(ui.0.window.width() <= 360 && ui.0.window.height() <= 540);
        ui.cancel_outgoing(&peer.fingerprint);
        respond(&ui, "cancel");
        assert!(token.is_cancelled());
        ui.fixture_finish_outgoing(&peer.fingerprint, Err(transfer::SendError::Cancelled));
        snapshot(&ui, "sending-canceled-small");
        assert!(ui.0.window.width() <= 360 && ui.0.window.height() <= 540);
        respond(&ui, "done");
        // A stale Cancel all confirmation must not cancel replacement sessions
        // or transfers that started after the confirmation was opened.
        let original_a = ui.fixture_start_outgoing(peer.clone(), true).unwrap();
        let original_b = ui.fixture_start_outgoing(other.clone(), true).unwrap();
        ui.0.cancel_send.emit_clicked();
        settle();
        ui.fixture_finish_outgoing(&peer.fingerprint, Ok(1));
        let replacement = ui.fixture_start_outgoing(peer.clone(), true).unwrap();
        let mut third = peer.clone();
        third.fingerprint = "c".repeat(64);
        third.alias = "Quiet Pear".into();
        let late = ui.fixture_start_outgoing(third.clone(), true).unwrap();
        respond(&ui, "cancel");
        assert!(!original_a.is_cancelled() && original_b.is_cancelled());
        assert!(!replacement.is_cancelled() && !late.is_cancelled());
        ui.fixture_finish_outgoing(&other.fingerprint, Err(transfer::SendError::Cancelled));
        ui.fixture_finish_outgoing(&peer.fingerprint, Ok(1));
        ui.fixture_finish_outgoing(&third.fingerprint, Ok(1));
        // Use the actual detail buttons: Continue returns to progress, Close
        // leaves it running, and a confirmed Cancel returns to the main page.
        let token = ui.fixture_start_outgoing(peer.clone(), true).unwrap();
        ui.send(peer.clone());
        respond(&ui, "cancel");
        assert!(!token.is_cancelled());
        respond(&ui, "continue");
        assert!(!token.is_cancelled());
        respond(&ui, "close");
        assert!(!token.is_cancelled());
        ui.send(peer.clone());
        respond(&ui, "cancel");
        respond(&ui, "cancel");
        assert!(token.is_cancelled());
        assert!(ui.0.window.visible_dialog().is_none());
        ui.fixture_finish_outgoing(&peer.fingerprint, Err(transfer::SendError::Cancelled));
        ui.0.window.close();
    }
}

pub fn install_styles() {
    gio::resources_register_include!("localsend.gresource").expect("Bundled icons");
    gtk::IconTheme::for_display(&gdk::Display::default().expect("No display"))
        .add_resource_path("/org/localsend/localsend_gtk/assets/icons");
    let provider = gtk::CssProvider::new();
    provider.load_from_string(include_str!("../assets/css/style.css"));
    gtk::style_context_add_provider_for_display(
        &gdk::Display::default().expect("No display"),
        &provider,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
}

#[derive(Clone)]
pub struct Ui(Rc<Inner>);
struct Inner {
    window: adw::ApplicationWindow,
    stack: gtk::Stack,
    toast: adw::ToastOverlay,
    #[cfg(test)]
    fixture_toasts: RefCell<Vec<adw::Toast>>,
    settings: RefCell<Settings>,
    appearance: crate::appearance::Appearance,
    network_controls: RefCell<Option<settings_view::NetworkControls>>,
    selection: RefCell<Vec<Selection>>,
    selection_generation: Cell<u64>,
    selection_request_counter: Cell<u64>,
    pending_selection: RefCell<Option<selection_view::SelectionContinuation>>,
    files: gtk::Box,
    file_strip: gtk::ScrolledWindow,
    selection_actions: gtk::Box,
    picker_choices: gtk::FlowBox,
    selection_card: gtk::Box,
    count: gtk::Label,
    clear: gtk::Button,
    devices: gtk::Box,
    empty: gtk::Box,
    peers: RefCell<HashMap<String, (DeviceInfo, gtk::Button)>>,
    identity: RefCell<Option<DeviceInfo>>,
    server_running: Cell<bool>,
    alias_label: gtk::Label,
    online: gtk::Label,
    progress: gtk::ProgressBar,
    progress_title: gtk::Label,
    progress_box: gtk::Box,
    cancel_send: gtk::Button,
    outgoing_cancel: RefCell<Option<CancellationToken>>,
    outgoing: RefCell<HashMap<String, sending::OutgoingTransfer>>,
    outgoing_rows: RefCell<HashMap<String, sending::OutgoingRow>>,
    receive_view: receive_progress::ReceiveProgressView,
    cancel_receive: gtk::Button,
    canceling_receive: Cell<bool>,
    incoming_pending: Cell<usize>,
    incoming_generation: Cell<u64>,
    incoming_dialogs: RefCell<HashMap<String, glib::WeakRef<adw::AlertDialog>>>,
    web: RefCell<Option<web::WebDialog>>,
    web_stopping: Cell<bool>,
    share: RefCell<Option<sharing::ShareDialog>>,
    share_stopping: Cell<bool>,
    share_auto_row: RefCell<Option<adw::SwitchRow>>,
    tray: RefCell<Option<crate::desktop::TrayHandle>>,
    tray_generation: Cell<u64>,
    tray_status: RefCell<Option<adw::ActionRow>>,
    background_hold: RefCell<Option<gio::ApplicationHoldGuard>>,
    hidden_start: Cell<bool>,
    quitting: Cell<bool>,
    sending: Cell<bool>,
    history: RefCell<crate::history::History>,
    history_load_error: Option<String>,
    history_view: RefCell<Option<history_view::HistoryView>>,
    commands: async_channel::Sender<Command>,
    offline_fixture: bool,
    adapt_layout: RefCell<Option<Rc<dyn Fn()>>>,
}

fn margins(widget: &impl IsA<gtk::Widget>, value: i32) {
    widget.set_margin_top(value);
    widget.set_margin_bottom(value);
    widget.set_margin_start(value);
    widget.set_margin_end(value);
}
fn label(text: &str, class: &str) -> gtk::Label {
    let label = gtk::Label::new(Some(text));
    label.add_css_class(class);
    label.set_xalign(0.0);
    label
}
fn translated_label(text: &str, class: &str) -> gtk::Label {
    let label = label("", class);
    i18n::bind_property(&label, "label", text);
    label
}
fn translated_dialog(heading: Option<&str>, body: Option<&str>) -> adw::AlertDialog {
    let dialog = adw::AlertDialog::new(None, None);
    if let Some(heading) = heading {
        i18n::bind_property(&dialog, "heading", heading);
    }
    if let Some(body) = body {
        i18n::bind_property(&dialog, "body", body);
    }
    dialog
}
fn add_response(dialog: &adw::AlertDialog, id: &str, source: &str) {
    dialog.add_response(id, source);
    i18n::bind_response(dialog, id, source);
}
fn add_responses(dialog: &adw::AlertDialog, responses: &[(&str, &str)]) {
    for (id, source) in responses {
        add_response(dialog, id, source);
    }
}
fn icon_button(icon: &str, tooltip: &str) -> gtk::Button {
    let button = gtk::Button::from_icon_name(icon);
    if let Some(image) = button.child().and_downcast::<gtk::Image>() {
        image.set_pixel_size(24);
    }
    button.add_css_class("icon-button");
    i18n::bind_property(&button, "tooltip-text", tooltip);
    i18n::bind_accessible_label(&button, tooltip);
    button
}
fn button_content(icon: &str, text: &str, vertical: bool) -> gtk::Box {
    let content = gtk::Box::new(
        if vertical {
            gtk::Orientation::Vertical
        } else {
            gtk::Orientation::Horizontal
        },
        8,
    );
    content.set_halign(gtk::Align::Center);
    content.set_valign(gtk::Align::Center);
    let image = gtk::Image::from_icon_name(icon);
    image.set_pixel_size(24);
    content.append(&image);
    content.append(&translated_label(text, "button-label"));
    content
}
fn scrolled(child: &impl IsA<gtk::Widget>) -> gtk::ScrolledWindow {
    gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(child)
        .vexpand(true)
        .build()
}
fn clamped(child: &impl IsA<gtk::Widget>) -> adw::Clamp {
    adw::Clamp::builder()
        .maximum_size(600)
        .tightening_threshold(600)
        .child(child)
        .hexpand(true)
        .build()
}
fn alert(window: &adw::ApplicationWindow, title: &str, body: &str) {
    let dialog = translated_dialog(Some(title), None);
    // The body can contain filenames, user content or external diagnostics.
    dialog.set_body(body);
    add_response(&dialog, "ok", "OK");
    dialog.set_default_response(Some("ok"));
    dialog.set_close_response("ok");
    dialog.present(Some(window));
}
fn alert_text(window: &adw::ApplicationWindow, title: &str, body: &str) {
    let dialog = translated_dialog(Some(title), Some(body));
    add_response(&dialog, "ok", "OK");
    dialog.set_default_response(Some("ok"));
    dialog.set_close_response("ok");
    dialog.present(Some(window));
}
fn apply_theme(value: u32) {
    adw::StyleManager::default().set_color_scheme(match value {
        1 => adw::ColorScheme::ForceLight,
        2 => adw::ColorScheme::ForceDark,
        _ => adw::ColorScheme::Default,
    });
}

pub fn build(app: &adw::Application, mut settings: Settings, offline_fixture: bool) -> Ui {
    i18n::set_locale(settings.language);
    if !offline_fixture && crate::desktop::autostart_supported() {
        if let Ok(enabled) = crate::desktop::is_autostart_enabled() {
            settings.autostart = enabled;
        }
    }
    apply_theme(settings.theme);
    let appearance = crate::appearance::Appearance::new(&settings);
    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("LocalSend")
        .default_width(1000)
        .default_height(700)
        .build();
    i18n::bind_property(&window, "title", "LocalSend");
    window.add_css_class("localsend");
    if settings.oled {
        window.add_css_class("oled");
    }
    window.set_size_request(360, 420);
    let style_manager = adw::StyleManager::default();
    if style_manager.is_dark() {
        window.add_css_class("dark");
    }
    style_manager.connect_dark_notify(glib::clone!(
        #[weak]
        window,
        move |style| {
            if style.is_dark() {
                window.add_css_class("dark");
            } else {
                window.remove_css_class("dark");
            }
        }
    ));
    let vertical = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let titlebar = gtk::WindowHandle::new();
    let title_content = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    title_content.add_css_class("titlebar-strip");
    title_content.append(&gtk::Box::builder().hexpand(true).build());
    title_content.append(&gtk::WindowControls::new(gtk::PackType::End));
    titlebar.set_child(Some(&title_content));
    vertical.append(&titlebar);
    let body = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    body.set_vexpand(true);
    let rail = gtk::Box::new(gtk::Orientation::Vertical, 0);
    rail.add_css_class("nav-rail");
    let brand = translated_label("LocalSend", "brand");
    brand.set_halign(gtk::Align::Center);
    rail.append(&brand);
    let stack = gtk::Stack::new();
    stack.set_hexpand(true);
    stack.set_vexpand(true);
    stack.set_vhomogeneous(false);
    stack.set_transition_type(gtk::StackTransitionType::Crossfade);
    stack.set_transition_duration(if settings.animations { 150 } else { 0 });
    let bottom = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    bottom.add_css_class("bottom-nav");
    bottom.set_homogeneous(true);
    let mut nav = Vec::new();
    let mut rail_labels = Vec::new();
    for (name, title, icon) in [
        ("receive", "Receive", "ls-wifi-symbolic"),
        ("send", "Send", "ls-send-symbolic"),
        ("settings", "Settings", "ls-settings-symbolic"),
    ] {
        let rail_button = gtk::ToggleButton::new();
        rail_button.add_css_class("nav-button");
        i18n::bind_property(&rail_button, "tooltip-text", title);
        i18n::bind_accessible_label(&rail_button, title);
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        let image = gtk::Image::from_icon_name(icon);
        image.set_pixel_size(24);
        row.append(&image);
        let text = translated_label(title, "navigation-label");
        row.append(&text);
        rail_labels.push(text);
        rail_button.set_child(Some(&row));
        rail.append(&rail_button);
        let bottom_button = gtk::ToggleButton::new();
        bottom_button.add_css_class("nav-button");
        i18n::bind_accessible_label(&bottom_button, title);
        bottom_button.set_child(Some(&button_content(icon, title, true)));
        bottom.append(&bottom_button);
        for button in [&rail_button, &bottom_button] {
            button.connect_clicked(glib::clone!(
                #[weak]
                stack,
                move |_| stack.set_visible_child_name(name)
            ));
        }
        nav.push((name, rail_button, bottom_button));
    }
    let toast = adw::ToastOverlay::new();
    toast.set_hexpand(true);
    toast.set_vexpand(true);
    toast.set_child(Some(&stack));
    let page_column = gtk::Box::new(gtk::Orientation::Vertical, 0);
    page_column.set_hexpand(true);
    page_column.append(&toast);
    body.append(&rail);
    body.append(&page_column);
    vertical.append(&body);
    vertical.append(&bottom);
    window.set_content(Some(&vertical));

    // Receive matches the upstream dimensions; the link action stays at the bottom.
    let receive = gtk::Overlay::new();
    let receive_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
    margins(&receive_box, 30);
    let center = gtk::Box::new(gtk::Orientation::Vertical, 0);
    center.set_vexpand(true);
    center.set_valign(gtk::Align::Center);
    let stream = gio::MemoryInputStream::from_bytes(&glib::Bytes::from_static(include_bytes!(
        "../assets/logo.png"
    )));
    let pixbuf =
        gdk_pixbuf::Pixbuf::from_stream_at_scale(&stream, 200, 200, true, gio::Cancellable::NONE)
            .expect("Bundled logo");
    let logo = gtk::DrawingArea::new();
    logo.set_widget_name("receive-logo");
    logo.set_content_width(200);
    logo.set_content_height(200);
    logo.set_halign(gtk::Align::Center);
    let logo_angle = Rc::new(Cell::new(0.0_f64));
    let angle = logo_angle.clone();
    logo.set_draw_func(move |logo, cr, width, height| {
        use gdk::prelude::GdkCairoContextExt;
        cr.translate(width as f64 / 2.0, height as f64 / 2.0);
        cr.rotate(angle.get());
        let scale = f64::from(width.min(height)) / 200.0;
        cr.scale(scale, scale);
        cr.translate(-100.0, -100.0);
        cr.push_group();
        cr.set_source_pixbuf(&pixbuf, 0.0, 0.0);
        if cr.paint().is_err() {
            return;
        }
        let Ok(mask) = cr.pop_group() else {
            return;
        };
        let color = logo.color();
        cr.set_source_rgba(
            color.red().into(),
            color.green().into(),
            color.blue().into(),
            color.alpha().into(),
        );
        let _ = cr.mask(&mask);
    });
    adw::StyleManager::default().connect_dark_notify(glib::clone!(
        #[weak]
        logo,
        move |_| logo.queue_draw()
    ));
    logo.add_css_class("receive-logo");
    center.append(&logo);
    let alias = label(&settings.alias, "receive-alias");
    alias.set_halign(gtk::Align::Center);
    alias.set_wrap(true);
    alias.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    alias.set_max_width_chars(24);
    alias.set_lines(2);
    alias.set_ellipsize(gtk::pango::EllipsizeMode::End);
    alias.set_tooltip_text(Some(&settings.alias));
    alias.set_justify(gtk::Justification::Center);
    center.append(&alias);
    let online = translated_label("Starting…", "receive-status");
    online.set_halign(gtk::Align::Center);
    center.append(&online);
    receive_box.append(&center);
    let link = gtk::Button::new();
    link.set_child(Some(&button_content(
        "ls-language-symbolic",
        "Receive via link",
        false,
    )));
    link.add_css_class("outlined");
    link.set_halign(gtk::Align::Center);
    link.set_margin_top(12);
    link.set_margin_bottom(24);
    i18n::bind_property(
        &link,
        "tooltip-text",
        "Receive files from another device's web browser",
    );
    // Keep the persistent receive action outside transient notifications.
    // Toasts sit above the footer, transfer controls and bottom navigation.
    let receive_footer = gtk::Box::new(gtk::Orientation::Vertical, 0);
    receive_footer.append(&link);
    page_column.append(&receive_footer);
    let receive_view = receive_progress::ReceiveProgressView::new(&window);
    let cancel_receive = receive_view.cancel_button();
    let receive_status = receive_view.widget();
    receive_status.set_margin_top(12);
    receive_status.set_margin_bottom(12);
    receive_box.append(&receive_status);
    // LocalSend switches from its idle identity artwork to transfer feedback.
    // Doing the same also keeps a 360×540 tiled window from being enlarged by
    // the progress card; multiple retained sessions remain scrollable.
    receive_status.connect_visible_notify(glib::clone!(
        #[weak]
        center,
        move |status| center.set_visible(!status.is_visible())
    ));
    center.set_visible(!receive_status.is_visible());
    receive.set_child(Some(&scrolled(&clamped(&receive_box))));
    let corners = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    margins(&corners, 20);
    corners.set_halign(gtk::Align::End);
    corners.set_valign(gtk::Align::Start);
    let history_button = icon_button("ls-history-symbolic", "History");
    let info_button = icon_button("ls-info-symbolic", "Network information");
    corners.append(&history_button);
    corners.append(&info_button);
    receive.add_overlay(&corners);
    stack.add_named(&receive, Some("receive"));

    let send = gtk::Box::new(gtk::Orientation::Vertical, 16);
    margins(&send, 15);
    send.set_margin_top(20);
    let selection_card = gtk::Box::new(gtk::Orientation::Vertical, 12);
    let selection_header = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let selection_title = translated_label("Selection", "section-title");
    selection_title.set_hexpand(true);
    selection_header.append(&selection_title);
    let clear = icon_button("ls-close-symbolic", "Clear selection");
    clear.set_visible(false);
    selection_header.append(&clear);
    selection_card.append(&selection_header);
    let count = label("", "body-text");
    count.set_visible(false);
    selection_card.append(&count);
    let files = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    let file_strip = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Automatic)
        .vscrollbar_policy(gtk::PolicyType::Never)
        .min_content_height(56)
        .vexpand(false)
        .child(&files)
        .visible(false)
        .build();
    selection_card.append(&file_strip);
    let choices = gtk::FlowBox::new();
    choices.set_selection_mode(gtk::SelectionMode::None);
    choices.set_homogeneous(true);
    choices.set_min_children_per_line(2);
    choices.set_max_children_per_line(4);
    choices.set_column_spacing(10);
    choices.set_row_spacing(10);
    let mut pickers = Vec::new();
    for (title, icon) in [
        ("File", "ls-description-symbolic"),
        ("Folder", "ls-folder-symbolic"),
        ("Text", "ls-subject-symbolic"),
        ("Paste", "ls-content-paste-symbolic"),
    ] {
        let button = gtk::Button::new();
        button.add_css_class("picker-button");
        button.set_child(Some(&button_content(icon, title, true)));
        choices.insert(&button, -1);
        pickers.push(button);
    }
    selection_card.append(&choices);
    let selection_actions = gtk::Box::new(gtk::Orientation::Horizontal, 15);
    selection_actions.set_halign(gtk::Align::End);
    selection_actions.set_visible(false);
    let edit_selection = i18n::button("Edit");
    edit_selection.add_css_class("text-button");
    let add_selection = gtk::Button::new();
    add_selection.set_child(Some(&button_content("list-add-symbolic", "Add", false)));
    add_selection.add_css_class("suggested-action");
    add_selection.add_css_class("pill");
    selection_actions.append(&edit_selection);
    selection_actions.append(&add_selection);
    selection_card.append(&selection_actions);
    send.append(&selection_card);
    let nearby = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    nearby.append(&translated_label("Nearby devices", "section-title"));
    let refresh = icon_button("ls-refresh-symbolic", "Search devices");
    nearby.append(&refresh);
    send.append(&nearby);
    let devices = gtk::Box::new(gtk::Orientation::Vertical, 10);
    let empty = gtk::Box::new(gtk::Orientation::Vertical, 12);
    empty.add_css_class("empty-state");
    let ghost = gtk::Box::new(gtk::Orientation::Horizontal, 16);
    ghost.add_css_class("device-placeholder");
    let computer = gtk::Image::from_icon_name("computer-symbolic");
    computer.set_pixel_size(36);
    ghost.append(&computer);
    let skeleton = gtk::Box::new(gtk::Orientation::Vertical, 8);
    skeleton.set_hexpand(true);
    skeleton.append(&translated_label("Nearby device", "device-title"));
    skeleton.append(&translated_label("LocalSend", "secondary"));
    ghost.append(&skeleton);
    empty.append(&ghost);
    let hint = translated_label(
        "Please ensure that the desired target is also on the same Wi-Fi network.",
        "secondary",
    );
    hint.set_wrap(true);
    hint.set_justify(gtk::Justification::Center);
    hint.set_xalign(0.5);
    empty.append(&hint);
    devices.append(&empty);
    send.append(&devices);
    let troubleshoot = i18n::button("Troubleshooting");
    troubleshoot.add_css_class("text-button");
    troubleshoot.set_halign(gtk::Align::Center);
    send.append(&troubleshoot);
    let progress_box = gtk::Box::new(gtk::Orientation::Vertical, 8);
    progress_box.add_css_class("selection-card");
    progress_box.set_visible(false);
    let progress_title = label("", "body-text");
    progress_title.set_wrap(true);
    progress_title.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    progress_title.set_width_chars(1);
    progress_title.set_lines(2);
    progress_title.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
    progress_box.append(&progress_title);
    let progress = gtk::ProgressBar::new();
    progress_box.append(&progress);
    let cancel_send = i18n::button("Cancel transfer");
    cancel_send.add_css_class("text-button");
    cancel_send.set_halign(gtk::Align::End);
    progress_box.append(&cancel_send);
    margins(&progress_box, 12);
    vertical.insert_child_after(&progress_box, Some(&body));
    stack.add_named(&scrolled(&clamped(&send)), Some("send"));

    let (commands, command_rx) = async_channel::unbounded();
    let (history, history_load_error) = if offline_fixture {
        (crate::history::History::default(), None)
    } else {
        match crate::history::History::load(Settings::directory().join("history.json")) {
            Ok(history) => (history, None),
            Err(error) => (crate::history::History::default(), Some(error.to_string())),
        }
    };
    let ui = Ui(Rc::new(Inner {
        window: window.clone(),
        stack: stack.clone(),
        toast,
        #[cfg(test)]
        fixture_toasts: RefCell::new(Vec::new()),
        settings: RefCell::new(settings.clone()),
        appearance,
        network_controls: RefCell::new(None),
        selection: RefCell::new(Vec::new()),
        selection_generation: Cell::new(0),
        selection_request_counter: Cell::new(0),
        pending_selection: RefCell::new(None),
        files,
        file_strip,
        selection_actions,
        picker_choices: choices.clone(),
        selection_card,
        count,
        clear: clear.clone(),
        devices,
        empty,
        peers: RefCell::new(HashMap::new()),
        identity: RefCell::new(None),
        server_running: Cell::new(false),
        alias_label: alias.clone(),
        online,
        progress,
        progress_title,
        progress_box,
        cancel_send: cancel_send.clone(),
        outgoing_cancel: RefCell::new(None),
        outgoing: RefCell::new(HashMap::new()),
        outgoing_rows: RefCell::new(HashMap::new()),
        receive_view,
        cancel_receive: cancel_receive.clone(),
        canceling_receive: Cell::new(false),
        incoming_pending: Cell::new(0),
        incoming_generation: Cell::new(0),
        incoming_dialogs: RefCell::new(HashMap::new()),
        web: RefCell::new(None),
        web_stopping: Cell::new(false),
        share: RefCell::new(None),
        share_stopping: Cell::new(false),
        share_auto_row: RefCell::new(None),
        tray: RefCell::new(None),
        tray_generation: Cell::new(0),
        tray_status: RefCell::new(None),
        background_hold: RefCell::new(None),
        hidden_start: Cell::new(false),
        quitting: Cell::new(false),
        sending: Cell::new(false),
        history: RefCell::new(history),
        history_load_error,
        history_view: RefCell::new(None),
        commands,
        offline_fixture,
        adapt_layout: RefCell::new(None),
    }));
    {
        let ui_accept = ui.clone();
        let accept_action =
            gio::SimpleAction::new("accept-transfer", Some(glib::VariantTy::STRING));
        accept_action.connect_activate(move |_, target| {
            if let Some(id) = target.and_then(glib::Variant::str) {
                ui_accept.respond_incoming(id, "accept");
            }
        });
        app.add_action(&accept_action);

        let ui_decline = ui.clone();
        let decline_action =
            gio::SimpleAction::new("decline-transfer", Some(glib::VariantTy::STRING));
        decline_action.connect_activate(move |_, target| {
            if let Some(id) = target.and_then(glib::Variant::str) {
                ui_decline.respond_incoming(id, "decline");
            }
        });
        app.add_action(&decline_action);

        let ui_show = ui.clone();
        let show_action = gio::SimpleAction::new("show-window", None);
        show_action.connect_activate(move |_, _| {
            ui_show.show_window();
        });
        app.add_action(&show_action);
    }
    nearby.append(&ui.device_actions());
    nearby.append(&ui.send_options());
    {
        let ui = ui.clone();
        edit_selection.connect_clicked(move |_| ui.show_selection_editor());
    }
    {
        let ui = ui.clone();
        add_selection.connect_clicked(move |_| ui.show_add_selection());
    }
    {
        let ui = ui.clone();
        cancel_send.connect_clicked(move |_| {
            if !ui.0.sending.get() {
                ui.0.progress_box.set_visible(false);
                return;
            }
            ui.cancel_all_outgoing();
        });
    }
    {
        let ui = ui.clone();
        link.connect_clicked(move |_| ui.show_web_receive());
    }
    {
        let ui = ui.clone();
        cancel_receive.connect_clicked(move |_| ui.cancel_incoming());
    }
    stack.add_named(&ui.settings_page(), Some("settings"));
    let weak_ui = Rc::downgrade(&ui.0);
    let last_frame = Cell::new(0_i64);
    logo.add_tick_callback(move |logo, clock| {
        let Some(ui) = weak_ui.upgrade() else {
            return glib::ControlFlow::Break;
        };
        let active = ui.server_running.get()
            && ui.settings.borrow().animations
            && logo.settings().is_gtk_enable_animations()
            && ui.stack.visible_child_name().as_deref() == Some("receive");
        if !active {
            last_frame.set(0);
            return glib::ControlFlow::Continue;
        }
        let now = clock.frame_time();
        let previous = last_frame.replace(now);
        if previous != 0 {
            // Match LocalSend's 15-second turn. Do not fast-forward across a
            // hidden window, paused compositor or suspended session.
            let elapsed = (now - previous).clamp(0, 100_000);
            logo_angle.set(
                (logo_angle.get() + elapsed as f64 / 15_000_000.0 * std::f64::consts::TAU)
                    % std::f64::consts::TAU,
            );
            logo.queue_draw();
        }
        glib::ControlFlow::Continue
    });
    stack.set_visible_child_name("receive");
    let sync_nav = move |stack: &gtk::Stack| {
        receive_footer.set_visible(stack.visible_child_name().as_deref() == Some("receive"));
        for (name, rail_button, bottom_button) in &nav {
            let active = stack.visible_child_name().as_deref() == Some(*name);
            rail_button.set_active(active);
            bottom_button.set_active(active);
        }
    };
    sync_nav(&stack);
    stack.connect_visible_child_name_notify(sync_nav);
    let adaptive_logo = logo.clone();
    let weak_ui = Rc::downgrade(&ui.0);
    let adapt = move |window: &adw::ApplicationWindow| {
        let width = window.width();
        let short = window.height() < 620;
        if short {
            window.add_css_class("short-window");
        } else {
            window.remove_css_class("short-window");
        }
        let logo_size = if short { 120 } else { 200 };
        adaptive_logo.set_content_width(logo_size);
        adaptive_logo.set_content_height(logo_size);
        receive_box.set_margin_top(if short { 54 } else { 30 });
        receive_box.set_margin_bottom(if short { 12 } else { 30 });
        receive_box.set_margin_start(if width < 550 { 12 } else { 30 });
        receive_box.set_margin_end(if width < 550 { 12 } else { 30 });
        if width < 550 {
            choices.set_min_children_per_line(2);
            choices.set_max_children_per_line(2);
        } else {
            choices.set_max_children_per_line(4);
            choices.set_min_children_per_line(4);
        }
        let compact_rail = weak_ui
            .upgrade()
            .map(|inner| inner.settings.borrow().compact_rail_narrow)
            .unwrap_or(false);
        if compact_rail {
            rail.set_visible(true);
            bottom.set_visible(false);
        } else {
            rail.set_visible(width >= 700);
            bottom.set_visible(width < 700);
        }
        brand.set_visible(width >= 800);
        for text in &rail_labels {
            text.set_visible(width >= 800);
        }
        if width >= 800 {
            rail.remove_css_class("compact");
        } else {
            rail.add_css_class("compact");
        }
        if width < 700 {
            alias.add_css_class("small-alias");
        } else {
            alias.remove_css_class("small-alias");
        }
    };
    let adapt = Rc::new(adapt);
    {
        let window = window.clone();
        let adapt = adapt.clone();
        *ui.0.adapt_layout.borrow_mut() = Some(Rc::new(move || {
            adapt(&window);
        }));
    }
    let previous_size = Cell::new((-1, -1));
    {
        let adapt = adapt.clone();
        window.add_tick_callback(move |window, _| {
            if previous_size.replace((window.width(), window.height()))
                != (window.width(), window.height())
            {
                adapt(window);
            }
            glib::ControlFlow::Continue
        });
    }
    {
        let ui = ui.clone();
        clear.connect_clicked(move |_| {
            ui.0.selection.borrow_mut().clear();
            ui.refresh_selection();
        });
    }
    for (index, button) in pickers.into_iter().enumerate() {
        let ui = ui.clone();
        button.connect_clicked(move |_| match index {
            0 => ui.pick(false),
            1 => ui.pick(true),
            2 => ui.text_dialog(),
            _ => ui.paste(),
        });
    }
    {
        let ui = ui.clone();
        refresh.connect_clicked(move |_| {
            let _ = ui.0.commands.try_send(Command::Refresh);
            ui.toast_text("Searching for nearby devices…");
        });
    }
    {
        let ui = ui.clone();
        history_button.connect_clicked(move |_| ui.show_history());
    }
    {
        let ui = ui.clone();
        info_button.connect_clicked(move |_| {
            let info = ui.0.identity.borrow();
            let current = ui.0.settings.borrow();
            let ip = localsend_rs::core::get_local_ip()
                .map(|ip| ip.to_string())
                .unwrap_or_else(|_| "Unavailable".into());
            let dialog = translated_dialog(Some("Network information"), None);
            let content = gtk::Box::new(gtk::Orientation::Vertical, 8);
            let details = dialogs::wrapped_label("", "body-text");
            details.set_selectable(true);
            i18n::bind_format_property(
                &details,
                "label",
                "Alias: {alias}\nIP: {ip}\nPort: {port}\nProtocol: HTTPS",
                &[
                    (
                        "alias",
                        info.as_ref()
                            .map(|i| i.alias.clone())
                            .unwrap_or_else(|| current.alias.clone()),
                    ),
                    ("ip", ip),
                    (
                        "port",
                        info.as_ref()
                            .map(|i| i.port)
                            .unwrap_or(current.port)
                            .to_string(),
                    ),
                ],
            );
            content.append(&details);
            content.append(&translated_label(
                if ui.server_is_running() {
                    "Ready to receive"
                } else {
                    "Offline"
                },
                "secondary",
            ));
            dialog.set_extra_child(Some(&content));
            add_response(&dialog, "ok", "OK");
            dialog.set_close_response("ok");
            dialog.present(Some(&ui.0.window));
        });
    }
    {
        let ui = ui.clone();
        troubleshoot.connect_clicked(move |_| alert_text(&ui.0.window, "Troubleshooting", "1. Open LocalSend on both devices.\n2. Connect both devices to the same local network.\n3. Allow TCP and UDP port 53317 through your firewall (or the port selected in Settings).\n4. Disable access-point isolation on guest Wi-Fi.\n5. Check that a VPN is not blocking local traffic, then refresh."));
    }
    let drop = gtk::DropTarget::new(gdk::FileList::static_type(), gdk::DragAction::COPY);
    {
        let ui = ui.clone();
        drop.connect_drop(move |_, value, _, _| {
            let Ok(files) = value.get::<gdk::FileList>() else {
                return false;
            };
            let paths = files
                .files()
                .iter()
                .filter_map(|file| file.path())
                .collect();
            ui.add_paths(paths);
            true
        });
    }
    window.add_controller(drop);
    let keys = gtk::EventControllerKey::new();
    {
        let ui = ui.clone();
        keys.connect_key_pressed(move |_, key, _, modifiers| {
            if ui.0.window.visible_dialog().is_some() {
                return glib::Propagation::Proceed;
            }
            if modifiers.contains(gdk::ModifierType::CONTROL_MASK) {
                match key {
                    gdk::Key::o => ui.pick(false),
                    gdk::Key::v if ui.0.stack.visible_child_name().as_deref() == Some("send") => {
                        ui.paste()
                    }
                    gdk::Key::_1 => ui.0.stack.set_visible_child_name("receive"),
                    gdk::Key::_2 => ui.0.stack.set_visible_child_name("send"),
                    gdk::Key::_3 => ui.0.stack.set_visible_child_name("settings"),
                    _ => return glib::Propagation::Proceed,
                }
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
    }
    window.add_controller(keys);
    {
        let ui = ui.clone();
        window.connect_close_request(move |_| {
            if !ui.0.quitting.get()
                && ui.0.settings.borrow().close_to_tray
                && (ui.hide_in_tray() || ui.hide_to_background())
            {
                return glib::Propagation::Stop;
            }
            ui.0.quitting.set(true);
            ui.0.tray.borrow_mut().take();
            ui.0.background_hold.borrow_mut().take();
            if let Some(token) = ui.0.outgoing_cancel.borrow().as_ref() {
                token.cancel();
            }
            let _ = ui.0.commands.try_send(Command::Shutdown);
            glib::Propagation::Proceed
        });
    }
    if !offline_fixture {
        ui.save_settings();
        let (tx, rx) = async_channel::unbounded();
        tokio::spawn(network::run(ui.0.settings.borrow().clone(), command_rx, tx));
        let weak = Rc::downgrade(&ui.0);
        glib::spawn_future_local(async move {
            while let Ok(event) = rx.recv().await {
                let Some(inner) = weak.upgrade() else {
                    break;
                };
                Ui(inner).network_event(event);
            }
        });
    } else {
        i18n::bind_property(&ui.0.online, "label", "Ready");
    }
    window.present();
    ui.configure_desktop();
    ui
}

impl Ui {
    pub fn adapt_layout(&self) {
        if let Some(adapt) = self.0.adapt_layout.borrow().as_ref() {
            adapt();
        }
    }
    pub fn open_files(&self, files: &[gio::File]) {
        let mut paths = Vec::new();
        for file in files {
            if let Some(path) = file.path() {
                paths.push(path);
            }
        }
        self.open_paths(paths);
    }
    pub fn open_paths(&self, paths: Vec<PathBuf>) {
        if !paths.is_empty() {
            self.add_paths(paths);
            self.show_window();
        }
    }
    fn respond_incoming(&self, id: &str, response: &str) {
        // GTK signals can synchronously remove this entry. Release the borrow
        // before emitting a response or closing the dialog.
        let dialog = self
            .0
            .incoming_dialogs
            .borrow()
            .get(id)
            .and_then(glib::WeakRef::upgrade);
        let Some(dialog) = dialog else {
            return;
        };
        if response == "accept" {
            self.show_window();
            if !dialog.is_response_enabled("accept") {
                return;
            }
        }
        dialog.emit_by_name::<()>("response", &[&response]);
        dialog.close();
    }
    fn withdraw_incoming_notification(&self, id: &str) {
        self.0.incoming_dialogs.borrow_mut().remove(id);
        if let Some(app) = self.0.window.application() {
            app.withdraw_notification(id);
        }
    }
    fn toast_text(&self, source: &str) {
        let toast = adw::Toast::new("");
        i18n::bind_property(&toast, "title", source);
        #[cfg(test)]
        self.0.fixture_toasts.borrow_mut().push(toast.clone());
        self.0.toast.add_toast(toast);
    }
    fn toast(&self, text: &str) {
        let toast = adw::Toast::new(text);
        #[cfg(test)]
        self.0.fixture_toasts.borrow_mut().push(toast.clone());
        self.0.toast.add_toast(toast);
    }
    fn save_settings(&self) -> bool {
        if self.0.offline_fixture {
            return true;
        }
        match self.0.settings.borrow().save() {
            Ok(()) => true,
            Err(e) => {
                alert(&self.0.window, "Could not save settings", &e.to_string());
                false
            }
        }
    }
    fn add_paths(&self, paths: Vec<PathBuf>) {
        self.cancel_selection_request(self.pending_selection_id());
        self.add_paths_for(paths, None);
    }
    fn add_paths_for(&self, paths: Vec<PathBuf>, request: Option<u64>) {
        if !self.selection_request_is_current(request) {
            return;
        }
        let ui = self.clone();
        self.0.stack.set_visible_child_name("send");
        glib::spawn_future_local(async move {
            let result = tokio::task::spawn_blocking(move || transfer::collect_paths(&paths)).await;
            if !ui.selection_request_is_current(request) {
                return;
            }
            match result {
                Ok(Ok(items)) => {
                    if items.is_empty() {
                        ui.cancel_selection_request(request);
                        ui.toast_text("No regular files found in this selection.");
                        return;
                    }
                    let mut selected = ui.0.selection.borrow_mut();
                    for item in items {
                        if !selected.iter().any(|old| matches!((&old.source, &item.source), (Source::File(a), Source::File(b)) if a == b)) { selected.push(item); }
                    }
                    drop(selected);
                    ui.refresh_selection();
                    ui.resume_selection_request(request);
                }
                Ok(Err(e)) => {
                    ui.cancel_selection_request(request);
                    alert(&ui.0.window, "Could not read selection", &e.to_string());
                }
                Err(e) => {
                    ui.cancel_selection_request(request);
                    alert(&ui.0.window, "Could not read selection", &e.to_string());
                }
            }
        });
    }
    fn pick(&self, folder: bool) {
        self.cancel_selection_request(self.pending_selection_id());
        self.pick_for(folder, None);
    }
    fn pick_for(&self, folder: bool, request: Option<u64>) {
        if !self.selection_request_is_current(request) {
            return;
        }
        let dialog = gtk::FileDialog::builder()
            .title(if folder {
                "Select folder"
            } else {
                "Select files"
            })
            .build();
        i18n::bind_property(
            &dialog,
            "title",
            if folder {
                "Select folder"
            } else {
                "Select files"
            },
        );
        let ui = self.clone();
        glib::spawn_future_local(async move {
            if folder {
                let result = dialog.select_folder_future(Some(&ui.0.window)).await;
                if !ui.selection_request_is_current(request) {
                    return;
                }
                match result {
                    Ok(file) => {
                        if let Some(path) = file.path() {
                            ui.add_paths_for(vec![path], request);
                        } else {
                            ui.cancel_selection_request(request);
                            ui.toast_text("Only local folders can be selected.");
                        }
                    }
                    Err(e) => {
                        ui.cancel_selection_request(request);
                        if !e.matches(gtk::DialogError::Dismissed) {
                            alert(&ui.0.window, "Could not open folder picker", &e.to_string());
                        }
                    }
                }
            } else {
                let result = dialog.open_multiple_future(Some(&ui.0.window)).await;
                if !ui.selection_request_is_current(request) {
                    return;
                }
                match result {
                    Ok(files) => {
                        let paths: Vec<_> = (0..files.n_items())
                            .filter_map(|i| {
                                files
                                    .item(i)
                                    .and_downcast::<gio::File>()
                                    .and_then(|f| f.path())
                            })
                            .collect();
                        if paths.is_empty() {
                            ui.cancel_selection_request(request);
                            ui.toast_text("Only local files can be selected.");
                        } else {
                            ui.add_paths_for(paths, request);
                        }
                    }
                    Err(e) => {
                        ui.cancel_selection_request(request);
                        if !e.matches(gtk::DialogError::Dismissed) {
                            alert(&ui.0.window, "Could not open file picker", &e.to_string());
                        }
                    }
                }
            }
        });
    }
    #[cfg(test)]
    fn add_text(&self, text: String) {
        self.cancel_selection_request(self.pending_selection_id());
        self.add_text_for(text, None);
    }
    pub(super) fn add_text_for(&self, text: String, request: Option<u64>) {
        if !self.selection_request_is_current(request) {
            return;
        }
        if text.trim().is_empty() {
            self.cancel_selection_request(request);
            self.toast_text("The clipboard or message is empty.");
            return;
        }
        self.0.selection.borrow_mut().push(Selection::text(text));
        self.refresh_selection();
        self.0.stack.set_visible_child_name("send");
        self.resume_selection_request(request);
    }
    fn paste(&self) {
        self.cancel_selection_request(self.pending_selection_id());
        self.paste_for(None);
    }
    fn paste_for(&self, request: Option<u64>) {
        if !self.selection_request_is_current(request) {
            return;
        }
        let ui = self.clone();
        glib::spawn_future_local(async move {
            let clipboard = ui.0.window.clipboard();
            if clipboard
                .formats()
                .contains_type(gdk::FileList::static_type())
            {
                let result = clipboard
                    .read_value_future(gdk::FileList::static_type(), glib::Priority::DEFAULT)
                    .await;
                if !ui.selection_request_is_current(request) {
                    return;
                }
                match result {
                    Ok(value) => {
                        if let Ok(files) = value.get::<gdk::FileList>() {
                            ui.add_paths_for(
                                files.files().iter().filter_map(|f| f.path()).collect(),
                                request,
                            );
                        } else {
                            ui.cancel_selection_request(request);
                            ui.toast_text("Copy text or files to the clipboard first.");
                        }
                    }
                    Err(e) => {
                        ui.cancel_selection_request(request);
                        alert(&ui.0.window, "Could not read clipboard", &e.to_string());
                    }
                }
            } else {
                let result = clipboard.read_text_future().await;
                if !ui.selection_request_is_current(request) {
                    return;
                }
                match result {
                    Ok(Some(text)) => ui.add_text_for(text.to_string(), request),
                    Ok(None) => {
                        ui.cancel_selection_request(request);
                        ui.toast_text("Copy text or files to the clipboard first.");
                    }
                    Err(e) => {
                        ui.cancel_selection_request(request);
                        alert(&ui.0.window, "Could not read clipboard", &e.to_string());
                    }
                }
            }
        });
    }
    fn text_dialog(&self) {
        self.cancel_selection_request(self.pending_selection_id());
        self.text_dialog_for(None);
    }
    fn text_dialog_for(&self, request: Option<u64>) {
        if !self.selection_request_is_current(request) {
            return;
        }
        let dialog = translated_dialog(Some("Type message"), None);
        let text = gtk::TextView::new();
        text.set_wrap_mode(gtk::WrapMode::WordChar);
        margins(&text, 12);
        let scroll = scrolled(&text);
        scroll.set_min_content_height(180);
        dialog.set_extra_child(Some(&scroll));
        add_responses(&dialog, &[("cancel", "Cancel"), ("add", "Confirm")]);
        dialog.set_response_appearance("add", adw::ResponseAppearance::Suggested);
        dialog.set_close_response("cancel");
        dialog.set_response_enabled("add", false);
        let weak_dialog = dialog.downgrade();
        text.buffer().connect_changed(move |buffer| {
            if let Some(dialog) = weak_dialog.upgrade() {
                let value = buffer.text(&buffer.start_iter(), &buffer.end_iter(), false);
                dialog.set_response_enabled("add", !value.trim().is_empty());
            }
        });
        let ui = self.clone();
        dialog.connect_response(None, move |_, response| {
            if !ui.selection_request_is_current(request) {
                return;
            }
            if response == "add" {
                let buffer = text.buffer();
                ui.add_text_for(
                    buffer
                        .text(&buffer.start_iter(), &buffer.end_iter(), false)
                        .to_string(),
                    request,
                );
            } else {
                ui.cancel_selection_request(request);
            }
        });
        dialog.present(Some(&self.0.window));
    }
    fn network_event(&self, event: Event) {
        match event {
            Event::ServerState { running, settings } => {
                self.server_state_changed(running, settings)
            }
            Event::Ready(identity) => {
                self.0.server_running.set(true);
                i18n::bind_property(&self.0.online, "label", "Ready");
                self.0.alias_label.set_label(&identity.alias);
                self.0.alias_label.set_tooltip_text(Some(&identity.alias));
                *self.0.identity.borrow_mut() = Some(identity);
            }
            Event::TransferRequest(request) => self.incoming_offer(request),
            Event::TextReceived {
                text,
                sender_alias,
                preview_handled,
            } => {
                self.record_received_message(text.clone(), sender_alias.clone());
                if !preview_handled {
                    self.show_received_message(text, sender_alias);
                }
            }
            Event::IncomingCanceled => self.incoming_canceled(),
            Event::IncomingCancelFailed(error) => {
                self.0.canceling_receive.set(false);
                self.0.receive_view.cancel_failed();
                self.toast(&i18n::tr_format(
                    "Could not cancel the incoming transfer: {error}",
                    &[("error", error)],
                ));
            }
            Event::WebReceiveReady(links) => self.web_receive_ready(links),
            Event::WebReceiveStopped => self.web_receive_stopped(),
            Event::WebShareReady(links) => self.web_share_ready(links),
            Event::WebShareStopped => self.web_share_stopped(),
            Event::WebShare(event) => self.web_share_event(event),
            Event::Peer(peer) => self.add_peer(peer),
            Event::Error(e) => self.toast(&e),
            Event::Offline(e) => {
                self.0.server_running.set(false);
                self.0.canceling_receive.set(false);
                self.0.receive_view.cancel_failed();
                i18n::bind_property(&self.0.online, "label", "Offline");
                self.0.identity.borrow_mut().take();
                alert(&self.0.window, "Could not start receiving", &e);
            }
            Event::Server(event) => match event {
                ServerEvent::FileReceiveProgress {
                    session_id,
                    file_id,
                    file_name,
                    sender_alias,
                    bytes_received,
                    total_bytes,
                    file_bytes_received,
                    file_size,
                    file_count,
                } => {
                    let reveal_receive = !self.0.receive_view.has_active();
                    self.0.receive_view.progress(
                        session_id.as_str(),
                        file_id.as_str(),
                        file_name,
                        sender_alias,
                        bytes_received,
                        total_bytes,
                        file_bytes_received,
                        file_size,
                        file_count,
                    );
                    if reveal_receive {
                        self.0.stack.set_visible_child_name("receive");
                    }
                }
                ServerEvent::FileReceived {
                    session_id,
                    file_id,
                    file_name,
                    path,
                    sender_alias,
                    size,
                    ..
                } => {
                    self.0.receive_view.file_received(
                        session_id.as_str(),
                        file_id.as_str(),
                        file_name.clone(),
                        path.clone(),
                        size,
                        sender_alias.clone(),
                    );
                    self.record_received_file(file_name.clone(), path, size, sender_alias);
                    self.toast(&i18n::tr_format("Received {name}", &[("name", file_name)]));
                }
                ServerEvent::TextReceived {
                    text, sender_alias, ..
                } => {
                    self.record_received_message(text.clone(), sender_alias.clone());
                    self.show_received_message(text, sender_alias);
                }
                ServerEvent::SessionDone { session_id } => {
                    self.0.receive_view.session_done(session_id.as_str());
                }
                _ => {}
            },
        }
    }
    fn add_peer(&self, mut peer: DeviceInfo) {
        peer.fingerprint = crate::settings::fingerprint_key(&peer.fingerprint);
        if self.0.settings.borrow().favorites.iter().any(|favorite| {
            crate::settings::same_fingerprint(&favorite.fingerprint, &peer.fingerprint)
                && favorite.protocol != peer.protocol
        }) {
            return;
        }
        self.remember_favorite_peer(&peer);
        if self.0.identity.borrow().as_ref().is_some_and(|local| {
            crate::settings::same_fingerprint(&local.fingerprint, &peer.fingerprint)
        }) {
            return;
        }
        let key = peer.fingerprint.clone();
        let button = self
            .0
            .peers
            .borrow()
            .get(&key)
            .map(|(_, button)| button.clone())
            .unwrap_or_else(|| {
                let outer = gtk::Box::new(gtk::Orientation::Horizontal, 4);
                outer.add_css_class("device-row");
                let button = gtk::Button::new();
                button.add_css_class("device-card");
                button.set_hexpand(true);
                let ui = self.clone();
                let send_key = key.clone();
                button.connect_clicked(move |_| {
                    let peer =
                        ui.0.peers
                            .borrow()
                            .get(&send_key)
                            .map(|(peer, _)| peer.clone());
                    if let Some(peer) = peer {
                        ui.send(peer);
                    }
                });
                outer.append(&button);
                let details = icon_button("ls-info-symbolic", "Device details and favorite");
                details.set_valign(gtk::Align::Center);
                let ui = self.clone();
                let details_key = key.clone();
                details.connect_clicked(move |_| {
                    let peer =
                        ui.0.peers
                            .borrow()
                            .get(&details_key)
                            .map(|(peer, _)| peer.clone());
                    if let Some(peer) = peer {
                        ui.show_device_details(peer);
                    }
                });
                outer.append(&details);
                let device = gtk::Box::new(gtk::Orientation::Vertical, 6);
                device.append(&outer);
                device.append(&self.outgoing_device_status(&key));
                self.0.devices.append(&device);
                button
            });
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 16);
        let image = gtk::Image::from_icon_name(match peer.device_type {
            Some(DeviceType::Mobile) => "phone-symbolic",
            _ => "computer-symbolic",
        });
        image.set_pixel_size(36);
        row.append(&image);
        let details = gtk::Box::new(gtk::Orientation::Vertical, 4);
        details.set_hexpand(true);
        let favorite = self
            .0
            .settings
            .borrow()
            .favorites
            .iter()
            .find(|favorite| {
                crate::settings::same_fingerprint(&favorite.fingerprint, &peer.fingerprint)
            })
            .cloned();
        let alias = favorite
            .as_ref()
            .filter(|favorite| favorite.custom_alias)
            .map(|favorite| favorite.alias.as_str())
            .unwrap_or(&peer.alias);
        let name = label(alias, "device-title");
        name.set_ellipsize(gtk::pango::EllipsizeMode::End);
        details.append(&name);
        let ip = peer.ip.as_deref().unwrap_or("?");
        details.append(&label(
            peer.device_model.as_deref().unwrap_or("LocalSend"),
            "secondary",
        ));
        row.append(&details);
        if favorite.is_some() {
            row.append(&gtk::Image::from_icon_name("ls-favorite-symbolic"));
        }
        row.append(&gtk::Image::from_icon_name("go-next-symbolic"));
        button.set_child(Some(&row));
        i18n::bind_format_property(
            &button,
            "tooltip-text",
            "Send to {name} ({ip})",
            &[("name", peer.alias.clone()), ("ip", ip.to_owned())],
        );
        self.0.peers.borrow_mut().insert(key, (peer, button));
        self.0.empty.set_visible(false);
        self.refresh_send_controls();
    }
}
