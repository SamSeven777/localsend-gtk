//! Watches the standardized XDG desktop portal accent color.
//!
//! The Settings portal describes `org.freedesktop.appearance/accent-color`
//! as an `(ddd)` tuple of sRGB components in the inclusive range `[0, 1]`.
//! Not every portal backend exposes it, so absence and malformed values are
//! reported as `None`; callers should retain LocalSend teal as their fallback.

use gio::prelude::*;
use glib::variant::ToVariant;
use std::{
    cell::{Cell, RefCell},
    rc::{Rc, Weak},
};

const PORTAL_NAME: &str = "org.freedesktop.portal.Desktop";
const PORTAL_PATH: &str = "/org/freedesktop/portal/desktop";
const SETTINGS_INTERFACE: &str = "org.freedesktop.portal.Settings";
const APPEARANCE_NAMESPACE: &str = "org.freedesktop.appearance";
const ACCENT_KEY: &str = "accent-color";
const READ_TIMEOUT_MS: i32 = 1_500;

/// A portal-provided sRGB accent color, quantized to the same 8-bit channels
/// accepted by LocalSend's custom-color setting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AccentColor {
    red: u8,
    green: u8,
    blue: u8,
}

impl AccentColor {
    fn from_srgb(red: f64, green: f64, blue: f64) -> Option<Self> {
        fn channel(value: f64) -> Option<u8> {
            if !value.is_finite() || !(0.0..=1.0).contains(&value) {
                return None;
            }
            Some((value * 255.0).round() as u8)
        }

        Some(Self {
            red: channel(red)?,
            green: channel(green)?,
            blue: channel(blue)?,
        })
    }

    pub fn hex(self) -> String {
        format!("#{:02X}{:02X}{:02X}", self.red, self.green, self.blue)
    }
}

type ChangeCallback = Box<dyn FnMut(Option<AccentColor>) + 'static>;
type StopWatcher = Box<dyn FnOnce() + 'static>;

struct Inner {
    callback: RefCell<ChangeCallback>,
    // The outer option distinguishes "not reported yet" from a reported
    // unavailable value. This guarantees one initial callback without later
    // repeating identical changes.
    last_reported: Cell<Option<Option<AccentColor>>>,
    generation: Cell<u64>,
    cancellable: gio::Cancellable,
    connection: RefCell<Option<gio::DBusConnection>>,
    subscription: RefCell<Option<gio::SignalSubscriptionId>>,
    stop_watcher: RefCell<Option<StopWatcher>>,
}

impl Inner {
    fn next_generation(&self) -> u64 {
        let next = self.generation.get().wrapping_add(1);
        self.generation.set(next);
        next
    }

    fn publish(&self, accent: Option<AccentColor>) {
        if self.last_reported.get() == Some(accent) {
            return;
        }
        self.last_reported.set(Some(accent));
        (self.callback.borrow_mut())(accent);
    }

    fn publish_if_current(&self, generation: u64, accent: Option<AccentColor>) {
        if self.generation.get() == generation {
            self.publish(accent);
        }
    }

    fn disconnect_signal(&self) {
        let subscription = self.subscription.borrow_mut().take();
        if let (Some(connection), Some(subscription)) =
            (self.connection.borrow().as_ref(), subscription)
        {
            connection.signal_unsubscribe(subscription);
        }
        self.connection.borrow_mut().take();
    }
}

impl Drop for Inner {
    fn drop(&mut self) {
        self.cancellable.cancel();
        if let Some(stop) = self.stop_watcher.borrow_mut().take() {
            stop();
        }
        self.disconnect_signal();
    }
}

/// Keeps the portal watch alive. Drop it with the owning GTK object to stop
/// all subscriptions and outstanding reads.
#[must_use = "the monitor must be retained for accent updates"]
pub struct SystemAccentMonitor {
    _inner: Rc<Inner>,
}

impl SystemAccentMonitor {
    /// Starts an asynchronous portal watch on the current GLib main context.
    ///
    /// The callback runs on that context. It receives the initial result and
    /// later changes. `None` means that the desktop has no standardized accent
    /// available, so the application should use its LocalSend teal fallback.
    pub fn new<F>(on_change: F) -> Self
    where
        F: FnMut(Option<AccentColor>) + 'static,
    {
        let inner = Rc::new(Inner {
            callback: RefCell::new(Box::new(on_change)),
            last_reported: Cell::new(None),
            generation: Cell::new(0),
            cancellable: gio::Cancellable::new(),
            connection: RefCell::new(None),
            subscription: RefCell::new(None),
            stop_watcher: RefCell::new(None),
        });

        let weak = Rc::downgrade(&inner);
        gio::bus_get(
            gio::BusType::Session,
            Some(&inner.cancellable),
            move |result| match result {
                Ok(connection) => start_name_watch(&weak, &connection),
                Err(_) => {
                    if let Some(inner) = weak.upgrade() {
                        inner.next_generation();
                        inner.publish(None);
                    }
                }
            },
        );

        Self { _inner: inner }
    }
}

fn start_name_watch(weak: &Weak<Inner>, connection: &gio::DBusConnection) {
    let appeared = weak.clone();
    let vanished = weak.clone();
    let watcher = gio::bus_watch_name_on_connection(
        connection,
        PORTAL_NAME,
        gio::BusNameWatcherFlags::AUTO_START,
        move |connection, _, _| {
            let Some(inner) = appeared.upgrade() else {
                return;
            };
            inner.disconnect_signal();
            inner.connection.replace(Some(connection.clone()));

            let changed = Rc::downgrade(&inner);
            let subscription = connection.signal_subscribe(
                Some(PORTAL_NAME),
                Some(SETTINGS_INTERFACE),
                Some("SettingChanged"),
                Some(PORTAL_PATH),
                Some(APPEARANCE_NAMESPACE),
                gio::DBusSignalFlags::NONE,
                move |_, _, _, _, _, parameters| {
                    let Some(inner) = changed.upgrade() else {
                        return;
                    };
                    let Some(accent) = parse_changed_signal(parameters) else {
                        return;
                    };
                    // A signal is newer than any read already in flight.
                    inner.next_generation();
                    inner.publish(accent);
                },
            );
            inner.subscription.replace(Some(subscription));
            request_accent(&inner, &connection);
        },
        move |_, _| {
            if let Some(inner) = vanished.upgrade() {
                inner.next_generation();
                inner.disconnect_signal();
                inner.publish(None);
            }
        },
    );

    if let Some(inner) = weak.upgrade() {
        inner.stop_watcher.replace(Some(Box::new(move || {
            gio::bus_unwatch_name(watcher);
        })));
    } else {
        gio::bus_unwatch_name(watcher);
    }
}

fn request_accent(inner: &Rc<Inner>, connection: &gio::DBusConnection) {
    let generation = inner.next_generation();
    let parameters = (APPEARANCE_NAMESPACE, ACCENT_KEY).to_variant();
    let weak = Rc::downgrade(inner);
    let fallback_connection = connection.clone();
    let cancellable = inner.cancellable.clone();
    connection.call(
        Some(PORTAL_NAME),
        PORTAL_PATH,
        SETTINGS_INTERFACE,
        "ReadOne",
        Some(&parameters),
        None,
        gio::DBusCallFlags::NONE,
        READ_TIMEOUT_MS,
        Some(&inner.cancellable),
        move |result| match result {
            Ok(reply) => finish_read(&weak, generation, &reply),
            Err(error) if is_unknown_method(&error) => {
                request_legacy_read(weak, generation, fallback_connection, cancellable);
            }
            Err(_) => {
                if let Some(inner) = weak.upgrade() {
                    inner.publish_if_current(generation, None);
                }
            }
        },
    );
}

fn request_legacy_read(
    weak: Weak<Inner>,
    generation: u64,
    connection: gio::DBusConnection,
    cancellable: gio::Cancellable,
) {
    let parameters = (APPEARANCE_NAMESPACE, ACCENT_KEY).to_variant();
    connection.call(
        Some(PORTAL_NAME),
        PORTAL_PATH,
        SETTINGS_INTERFACE,
        "Read",
        Some(&parameters),
        None,
        gio::DBusCallFlags::NONE,
        READ_TIMEOUT_MS,
        Some(&cancellable),
        move |result| {
            if let Some(inner) = weak.upgrade() {
                let accent = result.ok().as_ref().and_then(parse_read_reply);
                inner.publish_if_current(generation, accent);
            }
        },
    );
}

fn finish_read(weak: &Weak<Inner>, generation: u64, reply: &glib::Variant) {
    if let Some(inner) = weak.upgrade() {
        inner.publish_if_current(generation, parse_read_reply(reply));
    }
}

fn is_unknown_method(error: &glib::Error) -> bool {
    error.matches(gio::DBusError::UnknownMethod)
        || gio::DBusError::remote_error(error).as_deref()
            == Some("org.freedesktop.DBus.Error.UnknownMethod")
}

fn parse_changed_signal(parameters: &glib::Variant) -> Option<Option<AccentColor>> {
    let namespace = parameters.try_child_value(0)?.get::<String>()?;
    let key = parameters.try_child_value(1)?.get::<String>()?;
    if namespace != APPEARANCE_NAMESPACE || key != ACCENT_KEY {
        return None;
    }
    Some(parse_accent_value(&parameters.try_child_value(2)?))
}

fn parse_read_reply(reply: &glib::Variant) -> Option<AccentColor> {
    parse_accent_value(&reply.try_child_value(0)?)
}

fn parse_accent_value(value: &glib::Variant) -> Option<AccentColor> {
    let mut value = value.clone();
    // ReadOne and SettingChanged contain one variant layer; deprecated Read
    // accidentally contains two. Accept both without accepting other shapes.
    for _ in 0..=2 {
        if let Some((red, green, blue)) = value.get::<(f64, f64, f64)>() {
            return AccentColor::from_srgb(red, green, blue);
        }
        if !value.is::<glib::Variant>() {
            return None;
        }
        value = value.as_variant()?;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn srgb_components_quantize_to_custom_color_channels() {
        assert_eq!(
            AccentColor::from_srgb(0.0, 0.5, 1.0),
            Some(AccentColor {
                red: 0,
                green: 128,
                blue: 255,
            })
        );
        assert_eq!(
            AccentColor::from_srgb(0.0, 0.5, 1.0).unwrap().hex(),
            "#0080FF"
        );
    }

    #[test]
    fn invalid_portal_components_are_unset() {
        for components in [
            (-f64::EPSILON, 0.0, 0.0),
            (1.0 + f64::EPSILON, 0.0, 0.0),
            (f64::NAN, 0.0, 0.0),
            (f64::INFINITY, 0.0, 0.0),
            (0.0, f64::NEG_INFINITY, 0.0),
        ] {
            assert_eq!(
                AccentColor::from_srgb(components.0, components.1, components.2),
                None
            );
        }
    }

    #[test]
    fn parser_accepts_standard_and_deprecated_variant_layers() {
        let raw = (0.0_f64, 150.0 / 255.0, 136.0 / 255.0).to_variant();
        assert_eq!(parse_accent_value(&raw).unwrap().hex(), "#009688");

        let one_layer = glib::Variant::from_variant(&raw);
        assert_eq!(parse_accent_value(&one_layer).unwrap().hex(), "#009688");

        let two_layers = glib::Variant::from_variant(&one_layer);
        assert_eq!(parse_accent_value(&two_layers).unwrap().hex(), "#009688");

        let wrong_type = (0_u32, 150_u32, 136_u32).to_variant();
        assert_eq!(parse_accent_value(&wrong_type), None);
    }

    #[test]
    fn signal_parser_filters_namespace_and_key() {
        let color = glib::Variant::from_variant(&(1.0_f64, 0.0_f64, 0.5_f64).to_variant());
        let expected = Some(AccentColor {
            red: 255,
            green: 0,
            blue: 128,
        });
        let valid = glib::Variant::tuple_from_iter([
            APPEARANCE_NAMESPACE.to_variant(),
            ACCENT_KEY.to_variant(),
            color.clone(),
        ]);
        assert_eq!(parse_changed_signal(&valid), Some(expected));

        let other_key = glib::Variant::tuple_from_iter([
            APPEARANCE_NAMESPACE.to_variant(),
            "color-scheme".to_variant(),
            color,
        ]);
        assert_eq!(parse_changed_signal(&other_key), None);
    }
}
