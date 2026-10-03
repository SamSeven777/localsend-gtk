use super::dialogs::content_scroll;
use super::dialogs::translated_wrapped_label;
use super::*;

pub(super) struct WebDialog {
    pub dialog: adw::AlertDialog,
    content: gtk::Box,
}

impl Ui {
    pub(super) fn show_web_receive(&self) {
        if self.0.web_stopping.get() {
            self.toast_text("The previous link is closing. Try again in a moment.");
            return;
        }
        if let Some(web) = self.0.web.borrow().as_ref() {
            web.dialog.present(Some(&self.0.window));
            return;
        }
        let dialog = translated_dialog(
            Some("Receive via link"),
            Some("Open this link in your browser:"),
        );
        let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
        content.append(&translated_label("Starting server…", "body-text"));
        let body = gtk::Box::new(gtk::Orientation::Vertical, 16);
        body.append(&content);
        body.append(&translated_wrapped_label(
            "Devices must be on the same network. Keep the link private. Browser transfers use unencrypted HTTP. You approve each transfer in LocalSend.",
            "secondary",
        ));
        dialog.set_extra_child(Some(&content_scroll(&body, 400)));
        add_response(&dialog, "stop", "Close");
        dialog.set_close_response("stop");
        let ui = self.clone();
        dialog.connect_response(None, move |_, _| {
            if ui.0.web.borrow_mut().take().is_some() && !ui.0.offline_fixture {
                ui.0.web_stopping.set(true);
                if ui.0.commands.try_send(Command::StopWebReceive).is_err() {
                    ui.0.web_stopping.set(false);
                }
            }
        });
        *self.0.web.borrow_mut() = Some(WebDialog {
            dialog: dialog.clone(),
            content,
        });
        if !self.0.offline_fixture && self.0.commands.try_send(Command::StartWebReceive).is_err() {
            self.web_receive_stopped();
            self.toast_text("The browser receiver could not be started.");
            return;
        }
        dialog.present(Some(&self.0.window));
    }

    pub(super) fn web_receive_ready(&self, links: Vec<String>) {
        let web = self.0.web.borrow();
        let Some(web) = web.as_ref() else {
            let _ = self.0.commands.try_send(Command::StopWebReceive);
            return;
        };
        while let Some(child) = web.content.first_child() {
            web.content.remove(&child);
        }
        i18n::bind_property(
            &web.dialog,
            "body",
            if links.len() == 1 {
                "Open this link in your browser:"
            } else {
                "Open one of these links in your browser:"
            },
        );
        web.content.append(&self.browser_links(links));
    }

    pub(super) fn browser_links(&self, links: Vec<String>) -> gtk::Box {
        let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
        if let Some(first) = links.first() {
            match qrcode::QrCode::new(first.as_bytes()) {
                Ok(code) => {
                    let qr = gtk::DrawingArea::new();
                    qr.set_content_width(192);
                    qr.set_content_height(192);
                    qr.set_halign(gtk::Align::Center);
                    i18n::bind_property(&qr, "tooltip-text", "Scan with the other device's camera");
                    qr.set_draw_func(move |_, cr, width, height| {
                        let side = code.width();
                        let quiet = 4.0;
                        let scale = f64::from(width.min(height)) / (side as f64 + quiet * 2.0);
                        cr.set_source_rgb(1.0, 1.0, 1.0);
                        let _ = cr.paint();
                        cr.set_antialias(gtk::cairo::Antialias::None);
                        cr.set_source_rgb(0.0, 0.0, 0.0);
                        for y in 0..side {
                            for x in 0..side {
                                if code[(x, y)] == qrcode::Color::Dark {
                                    cr.rectangle(
                                        (x as f64 + quiet) * scale,
                                        (y as f64 + quiet) * scale,
                                        scale,
                                        scale,
                                    );
                                }
                            }
                        }
                        let _ = cr.fill();
                    });
                    content.append(&qr);
                }
                Err(_) => {
                    content.append(&translated_label(
                        "Copy a link below to connect.",
                        "secondary",
                    ));
                }
            }
        }
        for link in links {
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            let text = label(&link, "link-address");
            text.set_wrap(true);
            text.set_wrap_mode(gtk::pango::WrapMode::WordChar);
            text.set_selectable(true);
            text.set_hexpand(true);
            text.set_max_width_chars(30);
            text.set_tooltip_text(Some(&link));
            row.append(&text);
            let copy = icon_button("edit-copy-symbolic", "Copy browser link");
            copy.set_valign(gtk::Align::Center);
            let ui = self.clone();
            copy.connect_clicked(move |_| {
                ui.0.window.clipboard().set_text(&link);
                ui.toast_text("Copied to Clipboard");
            });
            row.append(&copy);
            content.append(&row);
        }
        content
    }

    pub(super) fn web_receive_stopped(&self) {
        self.0.web_stopping.set(false);
        let web = self.0.web.borrow_mut().take();
        if let Some(web) = web {
            web.dialog.force_close();
        }
    }
}
