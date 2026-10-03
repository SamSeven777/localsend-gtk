//! Native counterparts of LocalSend's address, favorite, and device dialogs.
use super::dialogs::translated_wrapped_label;
use super::dialogs::{content_scroll, wrapped_label};
use super::*;
use crate::settings::{same_fingerprint, FavoriteDevice};
use localsend_rs::protocol::Protocol;

fn device_dialog(title: &str) -> (adw::Dialog, gtk::Box) {
    let dialog = adw::Dialog::builder()
        .title(title)
        .content_width(440)
        .build();
    i18n::bind_property(&dialog, "title", title);
    let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
    content.append(&adw::HeaderBar::new());
    let body = gtk::Box::new(gtk::Orientation::Vertical, 16);
    margins(&body, 20);
    let scroll = content_scroll(&body, 480);
    scroll.set_vexpand(true);
    content.append(&scroll);
    dialog.set_child(Some(&content));
    (dialog, body)
}

fn entry(body: &gtk::Box, title: &str, value: &str) -> gtk::Entry {
    let field = gtk::Box::new(gtk::Orientation::Vertical, 6);
    let caption = translated_label(title, "secondary");
    let input = gtk::Entry::builder().text(value).hexpand(true).build();
    caption.set_mnemonic_widget(Some(&input));
    field.append(&caption);
    field.append(&input);
    body.append(&field);
    input
}

fn status_label(body: &gtk::Box) -> gtk::Label {
    let status = wrapped_label("", "secondary");
    status.set_visible(false);
    body.append(&status);
    status
}

fn error_label(status: &gtk::Label, message: &str) {
    i18n::bind_format_property(status, "label", "{message}", &[("message", message.into())]);
    status.add_css_class("error");
    status.set_visible(true);
}

fn buttons(dialog: &adw::Dialog, body: &gtk::Box, action: &str) -> gtk::Button {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    row.set_halign(gtk::Align::End);
    let cancel = i18n::button("Cancel");
    cancel.add_css_class("flat");
    cancel.connect_clicked(glib::clone!(
        #[weak]
        dialog,
        move |_| {
            dialog.close();
        }
    ));
    row.append(&cancel);
    let submit = i18n::button(action);
    submit.add_css_class("suggested-action");
    row.append(&submit);
    body.append(&row);
    dialog.set_default_widget(Some(&submit));
    submit
}

struct Lookup {
    address: String,
    port: u16,
    protocol: Protocol,
    fingerprint: Option<String>,
}

fn lookup_for_dialog(
    dialog: &adw::Dialog,
    fields: &gtk::Box,
    status: &gtk::Label,
    submit: &gtk::Button,
    request: Lookup,
    on_found: impl Fn(DeviceInfo) + 'static,
) {
    let address = match crate::address::parse(&request.address) {
        Ok(address) => address,
        Err(error) => {
            error_label(status, &error);
            return;
        }
    };
    fields.set_sensitive(false);
    submit.set_sensitive(false);
    status.remove_css_class("error");
    i18n::bind_property(status, "label", "Connecting…");
    status.set_visible(true);
    let task = tokio::spawn(async move {
        crate::http_client::lookup_peer(
            &address.to_string(),
            request.port,
            request.protocol,
            request.fingerprint.as_deref(),
            crate::network::certificate()?,
        )
        .await
    });
    let closed = Rc::new(Cell::new(false));
    let was_closed = closed.clone();
    let abort = task.abort_handle();
    let closed_handler = dialog.connect_closed(move |_| {
        was_closed.set(true);
        abort.abort();
    });
    let dialog = dialog.downgrade();
    let fields = fields.clone();
    let submit = submit.clone();
    let status = status.clone();
    glib::spawn_future_local(async move {
        let result = task.await;
        let Some(dialog) = dialog.upgrade() else {
            return;
        };
        dialog.disconnect(closed_handler);
        if closed.get() {
            return;
        }
        fields.set_sensitive(true);
        submit.set_sensitive(true);
        match result {
            Ok(Ok(peer)) => on_found(peer),
            Ok(Err(error)) => error_label(&status, &error),
            Err(error) => error_label(&status, &error.to_string()),
        }
    });
}

impl Ui {
    fn sync_favorites(&self) {
        let favorites = self.0.settings.borrow().favorites.clone();
        let _ = self.0.commands.try_send(Command::Favorites(favorites));
    }

    pub(super) fn device_actions(&self) -> gtk::Box {
        let actions = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        let manual = icon_button("ls-ads-click-symbolic", "Enter address");
        let ui = self.clone();
        manual.connect_clicked(move |_| {
            let next = ui.clone();
            ui.after_selection(move || next.show_manual_address());
        });
        actions.append(&manual);
        let favorites = icon_button("ls-favorite-symbolic", "Favorites");
        let ui = self.clone();
        favorites.connect_clicked(move |_| ui.show_favorites());
        actions.append(&favorites);
        actions
    }

    pub(super) fn remember_favorite_peer(&self, peer: &DeviceInfo) {
        let previous = self.0.settings.borrow().favorites.clone();
        let mut changed = false;
        for favorite in &mut self.0.settings.borrow_mut().favorites {
            changed |= favorite.observe(peer);
        }
        if changed {
            if self.save_settings() {
                self.sync_favorites();
            } else {
                self.0.settings.borrow_mut().favorites = previous;
            }
        }
    }

    fn store_favorite(&self, favorite: FavoriteDevice) -> bool {
        let previous = self.0.settings.borrow().favorites.clone();
        {
            let mut settings = self.0.settings.borrow_mut();
            if let Some(old) = settings
                .favorites
                .iter_mut()
                .find(|old| same_fingerprint(&old.fingerprint, &favorite.fingerprint))
            {
                *old = favorite;
            } else {
                settings.favorites.push(favorite);
            }
        }
        if self.save_settings() {
            self.sync_favorites();
            self.toast_text("Favorite saved");
            true
        } else {
            self.0.settings.borrow_mut().favorites = previous;
            false
        }
    }

    fn remove_favorite(&self, fingerprint: &str) -> bool {
        let previous = self.0.settings.borrow().favorites.clone();
        self.0
            .settings
            .borrow_mut()
            .favorites
            .retain(|favorite| !same_fingerprint(&favorite.fingerprint, fingerprint));
        if self.save_settings() {
            self.sync_favorites();
            let peer = self
                .0
                .peers
                .borrow()
                .values()
                .find(|(peer, _)| same_fingerprint(&peer.fingerprint, fingerprint))
                .map(|(peer, _)| peer.clone());
            if let Some(peer) = peer {
                self.add_peer(peer);
            }
            self.toast_text("Favorite removed");
            true
        } else {
            self.0.settings.borrow_mut().favorites = previous;
            false
        }
    }

    pub(super) fn show_device_details(&self, peer: DeviceInfo) {
        let saved = self
            .0
            .settings
            .borrow()
            .favorites
            .iter()
            .find(|favorite| same_fingerprint(&favorite.fingerprint, &peer.fingerprint))
            .cloned();
        let name = saved
            .as_ref()
            .map_or(peer.alias.as_str(), |favorite| favorite.alias.as_str());
        let dialog = adw::AlertDialog::new(Some(name), None);
        let details = gtk::Box::new(gtk::Orientation::Vertical, 8);
        for (caption, value) in [
            (
                "IP address",
                peer.ip.clone().unwrap_or_else(|| "Unknown".into()),
            ),
            ("Port", peer.port.to_string()),
            (
                "Device",
                peer.device_model
                    .clone()
                    .unwrap_or_else(|| "LocalSend".into()),
            ),
            ("Protocol", peer.protocol.to_string().to_uppercase()),
            ("Fingerprint", peer.fingerprint.clone()),
        ] {
            details.append(&translated_label(caption, "secondary"));
            let value = label(&value, "body-text");
            value.set_selectable(true);
            value.set_wrap(true);
            value.set_wrap_mode(gtk::pango::WrapMode::Char);
            value.set_max_width_chars(40);
            details.append(&value);
        }
        dialog.set_extra_child(Some(&content_scroll(&details, 340)));
        add_responses(
            &dialog,
            &[
                ("close", "Close"),
                (
                    "favorite",
                    if saved.is_some() {
                        "Settings"
                    } else {
                        "Add to favorites"
                    },
                ),
            ],
        );
        dialog.set_close_response("close");
        let ui = self.clone();
        dialog.connect_response(None, move |_, response| {
            if response == "favorite" {
                ui.show_address_dialog(true, saved.clone(), Some(peer.clone()), false);
            }
        });
        dialog.present(Some(&self.0.window));
    }

    pub(super) fn show_manual_address(&self) {
        self.show_address_dialog(false, None, None, false);
    }

    pub(super) fn show_favorites(&self) {
        let (dialog, body) = device_dialog("Favorites");
        let favorites = self.0.settings.borrow().favorites.clone();
        let list = gtk::Box::new(gtk::Orientation::Vertical, 8);
        if favorites.is_empty() {
            let empty = translated_wrapped_label("No favorite devices yet.", "secondary");
            list.append(&empty);
        }
        let status = wrapped_label("", "secondary");
        status.set_visible(false);
        for favorite in favorites {
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            let connect = gtk::Button::new();
            connect.set_hexpand(true);
            connect.add_css_class("flat");
            let content = gtk::Box::new(gtk::Orientation::Vertical, 4);
            let name = label(&favorite.alias, "body-text");
            name.set_ellipsize(gtk::pango::EllipsizeMode::End);
            name.set_tooltip_text(Some(&favorite.alias));
            content.append(&name);
            let address = label(
                &format!("{} · {}", favorite.address, favorite.port),
                "secondary",
            );
            address.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
            address.set_tooltip_text(Some(&format!("{} · {}", favorite.address, favorite.port)));
            content.append(&address);
            connect.set_child(Some(&content));
            let ui = self.clone();
            let target = favorite.clone();
            connect.connect_clicked(glib::clone!(
                #[weak]
                dialog,
                #[weak]
                list,
                #[strong]
                status,
                move |button| {
                    // Discovery may have changed the address while this dialog was open.
                    let target =
                        ui.0.settings
                            .borrow()
                            .favorites
                            .iter()
                            .find(|favorite| {
                                same_fingerprint(&favorite.fingerprint, &target.fingerprint)
                            })
                            .cloned()
                            .unwrap_or_else(|| target.clone());
                    let ui = ui.clone();
                    let weak_dialog = dialog.downgrade();
                    lookup_for_dialog(
                        &dialog,
                        &list,
                        &status,
                        button,
                        Lookup {
                            address: target.address.clone(),
                            port: target.port,
                            protocol: target.protocol,
                            fingerprint: Some(target.fingerprint.clone()),
                        },
                        move |peer| {
                            ui.add_peer(peer.clone());
                            if let Some(dialog) = weak_dialog.upgrade() {
                                dialog.close();
                            }
                            ui.0.stack.set_visible_child_name("send");
                            ui.send(peer);
                        },
                    );
                }
            ));
            row.append(&connect);
            let edit = icon_button("document-edit-symbolic", "Edit favorite");
            let ui = self.clone();
            edit.connect_clicked(glib::clone!(
                #[weak]
                dialog,
                move |_| {
                    dialog.close();
                    ui.show_address_dialog(true, Some(favorite.clone()), None, true);
                }
            ));
            row.append(&edit);
            list.append(&row);
        }
        body.append(&list);
        body.append(&status);
        let add = buttons(&dialog, &body, "Add");
        let ui = self.clone();
        add.connect_clicked(glib::clone!(
            #[weak]
            dialog,
            move |_| {
                dialog.close();
                ui.show_address_dialog(true, None, None, true);
            }
        ));
        dialog.present(Some(&self.0.window));
    }

    fn show_address_dialog(
        &self,
        save: bool,
        existing: Option<FavoriteDevice>,
        peer: Option<DeviceInfo>,
        return_to_favorites: bool,
    ) {
        let (dialog, body) = device_dialog(if existing.is_some() {
            "Settings"
        } else if save {
            "Add to favorites"
        } else {
            "Enter address"
        });
        let fields = gtk::Box::new(gtk::Orientation::Vertical, 14);
        let alias_value = existing
            .as_ref()
            .map(|favorite| favorite.alias.as_str())
            .or_else(|| peer.as_ref().map(|peer| peer.alias.as_str()))
            .unwrap_or("");
        let alias = if save {
            let input = entry(&fields, "Device name", alias_value);
            input.set_placeholder_text(Some("(auto)"));
            input.set_max_length(80);
            Some(input)
        } else {
            None
        };
        let address_value = existing
            .as_ref()
            .map(|favorite| favorite.address.as_str())
            .or_else(|| peer.as_ref().and_then(|peer| peer.ip.as_deref()))
            .unwrap_or("");
        let address = entry(&fields, "IP Address", address_value);
        i18n::bind_accessible_label(&address, "IP Address");
        address.set_placeholder_text(Some("192.168.1.123"));
        fields.append(&translated_wrapped_label(
            "Example: 192.168.1.123",
            "secondary",
        ));
        let connection_fields = gtk::Box::new(gtk::Orientation::Vertical, 14);
        let port_value = existing
            .as_ref()
            .map(|favorite| favorite.port)
            .or_else(|| peer.as_ref().map(|peer| peer.port))
            .unwrap_or(self.0.settings.borrow().port);
        let port = entry(&connection_fields, "Port", &port_value.to_string());
        port.set_input_purpose(gtk::InputPurpose::Digits);
        let protocol = existing
            .as_ref()
            .map(|favorite| favorite.protocol)
            .or_else(|| peer.as_ref().map(|peer| peer.protocol))
            .unwrap_or(Protocol::Https);
        let https = gtk::CheckButton::with_label("Encryption (HTTPS)");
        i18n::bind_property(&https, "label", "Encryption (HTTPS)");
        https.set_active(protocol == Protocol::Https);
        // Editing a saved device may not silently discard its transport identity.
        https.set_sensitive(existing.is_none() && peer.is_none());
        connection_fields.append(&https);
        let http_notice = translated_wrapped_label("HTTP sends without encryption. Use it only when the receiving device has encryption disabled.", "secondary");
        http_notice.set_visible(!https.is_active());
        https.connect_toggled(glib::clone!(
            #[weak]
            http_notice,
            move |check| http_notice.set_visible(!check.is_active())
        ));
        connection_fields.append(&http_notice);
        if save {
            fields.append(&connection_fields);
        } else {
            let advanced = gtk::Expander::new(Some("Advanced"));
            i18n::bind_property(&advanced, "label", "Advanced");
            connection_fields.set_margin_top(12);
            advanced.set_child(Some(&connection_fields));
            fields.append(&advanced);
        }
        body.append(&fields);
        let status = status_label(&body);
        if let Some(favorite) = existing.as_ref() {
            let remove = i18n::button("Delete");
            remove.add_css_class("destructive-action");
            remove.set_halign(gtk::Align::Start);
            let fingerprint = favorite.fingerprint.clone();
            let favorite_name = favorite.alias.clone();
            let ui = self.clone();
            remove.connect_clicked(glib::clone!(
                #[weak]
                dialog,
                move |_| {
                    let confirm = translated_dialog(Some("Delete from favorites"), None);
                    i18n::bind_format_property(
                        &confirm,
                        "body",
                        "Do you really want to delete from favorites \"{name}\"?",
                        &[("name", favorite_name.clone())],
                    );
                    add_responses(&confirm, &[("cancel", "Cancel"), ("delete", "Delete")]);
                    confirm.set_close_response("cancel");
                    confirm.set_default_response(Some("cancel"));
                    confirm.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
                    let ui = ui.clone();
                    let fingerprint = fingerprint.clone();
                    let parent = dialog.downgrade();
                    confirm.connect_response(None, move |_, response| {
                        if response == "delete" && ui.remove_favorite(&fingerprint) {
                            if let Some(parent) = parent.upgrade() {
                                parent.close();
                            }
                        }
                    });
                    confirm.present(Some(&dialog));
                }
            ));
            fields.append(&remove);
        }
        let submit = buttons(&dialog, &body, "Confirm");
        address.set_activates_default(true);
        port.set_activates_default(true);
        let expected = existing
            .as_ref()
            .map(|favorite| favorite.fingerprint.clone())
            .or_else(|| peer.as_ref().map(|peer| peer.fingerprint.clone()));
        let editing = existing.clone();
        let ui = self.clone();
        submit.connect_clicked(glib::clone!(
            #[weak]
            dialog,
            #[strong]
            fields,
            #[strong]
            status,
            #[strong]
            address,
            move |button| {
                let address = address.text().trim().to_owned();
                let Ok(port) = port.text().trim().parse::<u16>() else {
                    error_label(&status, "Use a port from 1 to 65535.");
                    return;
                };
                if port == 0 {
                    error_label(&status, "Use a port from 1 to 65535.");
                    return;
                }
                let alias = alias
                    .as_ref()
                    .map(|entry| entry.text().trim().to_owned())
                    .unwrap_or_default();
                // Editing a saved favorite is local, as in the official app.
                // Retain its certificate fingerprint and protocol so changing
                // its name or address never replaces the trusted identity.
                if let Some(previous) = editing.as_ref() {
                    let mut favorite = previous.clone();
                    if alias.is_empty() {
                        favorite.custom_alias = false;
                    } else {
                        favorite.custom_alias |= alias != previous.alias;
                        favorite.alias = alias;
                    }
                    favorite.address = address;
                    favorite.port = port;
                    if let Err(error) = favorite.validate() {
                        error_label(&status, &error.to_string());
                        return;
                    }
                    if ui.store_favorite(favorite.clone()) {
                        let peer =
                            ui.0.peers
                                .borrow()
                                .values()
                                .find(|(peer, _)| {
                                    same_fingerprint(&peer.fingerprint, &favorite.fingerprint)
                                })
                                .map(|(peer, _)| peer.clone());
                        if let Some(mut peer) = peer {
                            peer.ip = Some(favorite.address);
                            peer.port = favorite.port;
                            ui.add_peer(peer);
                        }
                        dialog.close();
                    }
                    return;
                }
                let ui = ui.clone();
                let weak_dialog = dialog.downgrade();
                let result_status = status.clone();
                lookup_for_dialog(
                    &dialog,
                    &fields,
                    &status,
                    button,
                    Lookup {
                        address,
                        port,
                        protocol: if https.is_active() {
                            Protocol::Https
                        } else {
                            Protocol::Http
                        },
                        fingerprint: expected.clone(),
                    },
                    move |peer| {
                        if save {
                            let favorite = match FavoriteDevice::from_peer(&peer, &alias) {
                                Ok(favorite) => favorite,
                                Err(error) => {
                                    error_label(&result_status, &error.to_string());
                                    return;
                                }
                            };
                            if !ui.store_favorite(favorite) {
                                return;
                            }
                        }
                        ui.add_peer(peer.clone());
                        if let Some(dialog) = weak_dialog.upgrade() {
                            dialog.close();
                        }
                        if !save {
                            ui.0.stack.set_visible_child_name("send");
                            ui.send(peer);
                        }
                    },
                );
            }
        ));
        if return_to_favorites {
            let ui = self.clone();
            dialog.connect_closed(move |_| {
                if !ui.0.quitting.get() {
                    ui.show_favorites();
                }
            });
        }
        dialog.present(Some(&self.0.window));
        address.grab_focus();
    }
}
