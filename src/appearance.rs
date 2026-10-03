//! LocalSend's Flutter 3.41 / Material Color Utilities 0.13.0 color roles,
//! mapped to native GTK controls. Reference vectors come from the pinned Dart
//! package; Yaru constants come from the upstream app's Yaru 10.2.0 theme.
use crate::settings::Settings;
use gtk4::{gdk, CssProvider};
use material_colors::{color::Argb, scheme::variant::SchemeTonalSpot};
use serde::{Deserialize, Serialize};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

const LOCALSEND_TEAL: Color = Color(0, 150, 136);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ColorMode {
    System,
    #[default]
    LocalSend,
    Oled,
    Yaru,
    Custom,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Color(u8, u8, u8);

impl Color {
    fn from_argb(color: Argb) -> Self {
        Self(color.red, color.green, color.blue)
    }

    fn argb(self) -> Argb {
        Argb::new(255, self.0, self.1, self.2)
    }

    pub fn parse(input: &str) -> Option<Self> {
        let input = input.trim().strip_prefix('#').unwrap_or(input.trim());
        if input.len() != 6 || !input.bytes().all(|c| c.is_ascii_hexdigit()) {
            return None;
        }
        let value = u32::from_str_radix(input, 16).ok()?;
        Some(Self((value >> 16) as u8, (value >> 8) as u8, value as u8))
    }

    pub fn hex(self) -> String {
        format!("#{:02X}{:02X}{:02X}", self.0, self.1, self.2)
    }

    pub fn rgba(self) -> gdk::RGBA {
        gdk::RGBA::new(
            self.0 as f32 / 255.0,
            self.1 as f32 / 255.0,
            self.2 as f32 / 255.0,
            1.0,
        )
    }

    pub fn from_rgba(color: &gdk::RGBA) -> Self {
        Self(
            (color.red() * 255.0).round() as u8,
            (color.green() * 255.0).round() as u8,
            (color.blue() * 255.0).round() as u8,
        )
    }

    fn mix(self, other: Self, amount: f64) -> Self {
        let component =
            |a: u8, b: u8| (a as f64 * (1.0 - amount) + b as f64 * amount).round() as u8;
        Self(
            component(self.0, other.0),
            component(self.1, other.1),
            component(self.2, other.2),
        )
    }

    fn luminance(self) -> f64 {
        let linear = |c: u8| {
            let c = c as f64 / 255.0;
            if c <= 0.04045 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * linear(self.0) + 0.7152 * linear(self.1) + 0.0722 * linear(self.2)
    }

    fn contrast(self, other: Self) -> f64 {
        let (a, b) = (self.luminance(), other.luminance());
        (a.max(b) + 0.05) / (a.min(b) + 0.05)
    }

    fn with_contrast(self, background: Self, target: Self, ratio: f64) -> Self {
        for step in 0..=100 {
            let color = self.mix(target, step as f64 / 100.0);
            if color.contrast(background) >= ratio {
                return color;
            }
        }
        target
    }
}

const BLACK: Color = Color(0, 0, 0);
const WHITE: Color = Color(255, 255, 255);

struct Palette {
    surface: Color,
    rail: Color,
    card: Color,
    text: Color,
    secondary: Color,
    primary: Color,
    on_primary: Color,
    selected: Color,
    on_selected: Color,
    outline: Color,
    bottom: Color,
    dialog: Color,
    input: Color,
    picker: Color,
    on_picker: Color,
    device: Color,
    on_device: Color,
    outlined: Color,
    error: Color,
    on_error: Color,
    toast: Color,
    on_toast: Color,
    yaru: bool,
}

impl Palette {
    fn new(mode: ColorMode, seed: Color, dark: bool, oled: bool) -> Self {
        // OLED is its own upstream color mode: it keeps the LocalSend palette
        // and changes only ColorScheme.surface to black.
        let oled = oled || mode == ColorMode::Oled;
        let yaru = mode == ColorMode::Yaru && !oled;
        let source = if yaru {
            Color(233, 84, 32)
        } else if matches!(mode, ColorMode::System | ColorMode::Custom) && !oled {
            seed
        } else {
            LOCALSEND_TEAL // Flutter Colors.teal
        };
        let scheme = SchemeTonalSpot::new(source.argb().into(), dark, Some(0.0)).scheme;
        let surface = Color::from_argb(scheme.surface());
        let primary = Color::from_argb(scheme.primary());
        let selected = Color::from_argb(scheme.secondary_container());
        // The Rust port's older default uses light tone 10 for this role.
        // Dart MCU 0.13.0 uses tone 30 (dark stays 90) for standard TonalSpot.
        let on_selected =
            Color::from_argb(scheme.secondary_palette.tone(if dark { 90 } else { 30 }));
        let card = Color::from_argb(scheme.surface_container_low());
        let text = Color::from_argb(scheme.on_surface());
        let mut palette = Self {
            surface,
            // LocalSend explicitly applies elevation 1 to ThemeData.cardColor:
            // Flutter rounds the 5% opacity to 13/255 before alpha blending.
            rail: surface.mix(Color::from_argb(scheme.surface_tint()), 13.0 / 255.0),
            card,
            text,
            secondary: Color::from_argb(scheme.on_surface_variant()),
            primary,
            on_primary: Color::from_argb(scheme.on_primary()),
            selected,
            on_selected,
            outline: Color::from_argb(scheme.outline()),
            bottom: Color::from_argb(scheme.surface_container()),
            dialog: Color::from_argb(scheme.surface_container_high()),
            input: selected,
            picker: if dark { selected } else { card },
            on_picker: if dark { on_selected } else { primary },
            device: if dark { selected } else { card },
            on_device: text,
            outlined: primary,
            error: Color::from_argb(scheme.error()),
            on_error: Color::from_argb(scheme.on_error()),
            toast: Color::from_argb(scheme.inverse_surface()),
            on_toast: Color::from_argb(scheme.inverse_on_surface()),
            yaru,
        };
        if yaru {
            // Exact results of Yaru 10.2.0 common_themes.dart/colors.dart HSL
            // operations for its default orange, white, jet and porcelain.
            let values = if dark {
                [
                    "202020", "2A2A2A", "2B2B2B", "FAFAFA", "E1E1E1", "40302A", "3F3F3F", "1A1A1A",
                    "E86581",
                ]
            } else {
                [
                    "FFFFFF", "F3F3F3", "F2F2F2", "363636", "4D4D4D", "F6E8E4", "CCCCCC", "FFFFFF",
                    "B52A4A",
                ]
            };
            let [surface, rail, card, text, secondary, input, outline, dialog, error] =
                values.map(|value| Color::parse(value).unwrap());
            palette.surface = surface;
            palette.rail = rail;
            palette.card = card;
            palette.text = text;
            palette.secondary = secondary;
            palette.primary = source;
            palette.on_primary = WHITE;
            palette.selected = rail.mix(text, 0.1);
            palette.on_selected = text;
            palette.outline = outline;
            palette.bottom = surface;
            palette.dialog = dialog;
            palette.input = input;
            palette.picker = if dark { input } else { palette.picker };
            palette.on_picker = if dark { WHITE } else { source };
            palette.device = if dark { input } else { card };
            palette.on_device = text;
            palette.outlined = text;
            palette.error = error;
            palette.on_error = WHITE;
            palette.toast = if dark {
                Color(250, 250, 250)
            } else {
                Color(32, 32, 32)
            };
            palette.on_toast = if dark { Color(32, 32, 32) } else { WHITE };
        }
        if oled && dark {
            palette.surface = BLACK;
            palette.rail = BLACK.mix(primary, 13.0 / 255.0);
        }
        palette
    }

    fn apply_high_contrast(&mut self, dark: bool) {
        let target = if dark { WHITE } else { BLACK };
        // Pick the least favorable of every surface, including selected rows.
        // Keeping the surfaces preserves color identity while the foregrounds
        // gain enough contrast for small text and the active keyboard focus.
        let background = [
            self.surface,
            self.rail,
            self.card,
            self.selected,
            self.dialog,
            self.bottom,
            self.input,
            self.picker,
            self.device,
        ]
        .into_iter()
        .min_by(|left, right| target.contrast(*left).total_cmp(&target.contrast(*right)))
        .expect("Palette has surfaces");
        self.text = self.text.with_contrast(background, target, 7.1);
        self.secondary = self.secondary.with_contrast(background, target, 7.1);
        self.primary = self.primary.with_contrast(background, target, 7.1);
        self.outline = self.outline.with_contrast(background, target, 4.6);
        self.on_primary =
            self.on_primary
                .with_contrast(self.primary, if dark { BLACK } else { WHITE }, 7.1);
        self.on_selected = self.on_selected.with_contrast(self.selected, target, 7.1);
        self.on_picker = self.on_picker.with_contrast(background, target, 7.1);
        self.on_device = self.on_device.with_contrast(self.device, target, 7.1);
        self.outlined = self.outlined.with_contrast(background, target, 7.1);
        self.error = self.error.with_contrast(background, target, 7.1);
        self.on_error =
            self.on_error
                .with_contrast(self.error, if dark { BLACK } else { WHITE }, 7.1);
    }

    fn css(&self, high_contrast: bool) -> String {
        let mut css = String::new();
        for (name, value) in [
            ("ls_surface", self.surface),
            ("ls_rail", self.rail),
            ("ls_card", self.card),
            ("ls_text", self.text),
            ("ls_secondary", self.secondary),
            ("ls_primary", self.primary),
            ("ls_selected", self.selected),
            ("ls_outline", self.outline),
            ("ls_on_primary", self.on_primary),
            ("ls_on_selected", self.on_selected),
            ("ls_bottom", self.bottom),
            ("ls_dialog", self.dialog),
            (
                "ls_popover",
                if self.yaru { self.dialog } else { self.bottom },
            ),
            ("ls_input", self.input),
            ("ls_picker", self.picker),
            ("ls_on_picker", self.on_picker),
            ("ls_device", self.device),
            ("ls_on_device", self.on_device),
            ("ls_outlined", self.outlined),
            ("ls_error", self.error),
            ("ls_on_error", self.on_error),
            ("ls_toast", self.toast),
            ("ls_on_toast", self.on_toast),
        ] {
            css.push_str(&format!("@define-color {name} {};\n", value.hex()));
        }
        // libadwaita 1.5 uses named colors. Supplying these alongside the scoped
        // application rules also keeps its native menus and dialogs consistent.
        for (name, role) in [
            ("accent_bg_color", "primary"),
            ("accent_color", "primary"),
            ("accent_fg_color", "on_primary"),
            ("window_bg_color", "surface"),
            ("window_fg_color", "text"),
            ("view_bg_color", "surface"),
            ("view_fg_color", "text"),
            ("headerbar_bg_color", "rail"),
            ("headerbar_fg_color", "text"),
            ("headerbar_backdrop_color", "rail"),
            ("headerbar_border_color", "outline"),
            ("card_bg_color", "card"),
            ("card_fg_color", "text"),
            ("dialog_bg_color", "dialog"),
            ("dialog_fg_color", "text"),
            ("popover_bg_color", "popover"),
            ("popover_fg_color", "text"),
            ("sidebar_bg_color", "rail"),
            ("sidebar_fg_color", "text"),
            ("destructive_bg_color", "error"),
            ("destructive_fg_color", "on_error"),
            ("destructive_color", "error"),
            ("error_color", "error"),
        ] {
            css.push_str(&format!("@define-color {name} @ls_{role};\n"));
        }
        css.push_str(
            ".localsend .nav-button:checked { color: @ls_on_selected; }\n\
             .localsend .bottom-nav { background: @ls_bottom; }\n\
             .localsend .picker-button { background: @ls_picker; color: @ls_on_picker; }\n\
             .localsend .picker-button:hover { background: @ls_selected; }\n\
             .localsend .device-card, .localsend .device-placeholder { background: @ls_device; color: @ls_on_device; }\n\
             .localsend .device-card.selected-device, .localsend .device-card:hover { background: @ls_selected; color: @ls_on_selected; }\n\
             .localsend .outlined { color: @ls_outlined; }\n\
             .localsend textview, .localsend textview text { background: @ls_input; color: @ls_text; }\n\
             .localsend popover > contents { background: @ls_popover; color: @ls_text; }\n\
             .localsend toast { background: @ls_toast; color: @ls_on_toast; }\n",
        );
        if high_contrast {
            // Inset outlines strengthen boundaries without changing allocation
            // or moving controls when the desktop toggles high contrast.
            css.push_str(
                ".localsend .outlined, .localsend .picker-button { box-shadow: inset 0 0 0 2px @ls_outline; }\n\
                 .localsend .nav-button:checked, .localsend .device-card.selected-device { box-shadow: inset 0 0 0 2px @ls_primary; }\n\
                 .localsend entry, .localsend textview, .localsend row.entry { box-shadow: inset 0 0 0 2px @ls_outline; }\n\
                 .localsend row.entry:focus-within { box-shadow: inset 0 0 0 3px @ls_primary; }\n\
                 .localsend .nav-button:hover, .localsend .icon-button:hover, .localsend .icon-button > button:hover, .localsend .outlined:hover, .localsend .text-button:hover, .localsend list.boxed-list > row:hover { background: @ls_selected; }\n\
                 .localsend button.suggested-action:hover { background: @ls_primary; box-shadow: inset 0 0 0 2px @ls_on_primary; }\n\
                 .localsend button:focus-visible, .localsend entry:focus-within, .localsend textview:focus-within, .localsend switch:focus-visible, .localsend checkbutton:focus-visible { outline: 3px solid @ls_primary; outline-offset: 2px; }\n",
            );
        }
        css
    }
}

#[derive(Clone, Copy)]
struct Configuration {
    mode: ColorMode,
    custom_seed: Color,
    system_seed: Option<Color>,
    oled: bool,
}

impl Configuration {
    fn new(settings: &Settings, system_seed: Option<Color>) -> Self {
        Self {
            mode: settings.color_mode,
            custom_seed: Color::parse(&settings.custom_color).unwrap_or(LOCALSEND_TEAL),
            system_seed,
            oled: settings.oled,
        }
    }

    fn seed(self) -> Color {
        match self.mode {
            ColorMode::System => self.system_seed.unwrap_or(LOCALSEND_TEAL),
            ColorMode::Custom => self.custom_seed,
            _ => LOCALSEND_TEAL,
        }
    }

    fn oled(self) -> bool {
        self.oled || self.mode == ColorMode::Oled
    }
}

type SystemAccentListener = Box<dyn Fn(Option<Color>) -> bool + 'static>;

struct Inner {
    provider: CssProvider,
    display: gdk::Display,
    configuration: RefCell<Configuration>,
    system_accent: Cell<Option<Color>>,
    system_accent_known: Cell<bool>,
    system_accent_listeners: RefCell<Vec<SystemAccentListener>>,
    _system_accent_monitor: RefCell<Option<crate::system_accent::SystemAccentMonitor>>,
}

impl Inner {
    fn refresh(&self) {
        let configuration = *self.configuration.borrow();
        let style = adw::StyleManager::default();
        let mut palette = Palette::new(
            configuration.mode,
            configuration.seed(),
            style.is_dark(),
            configuration.oled(),
        );
        if style.is_high_contrast() {
            palette.apply_high_contrast(style.is_dark());
        }
        self.provider
            .load_from_string(&palette.css(style.is_high_contrast()));
    }

    fn set_system_accent(&self, accent: Option<Color>) {
        let first = !self.system_accent_known.replace(true);
        let changed = self.system_accent.replace(accent) != accent;
        if changed {
            self.configuration.borrow_mut().system_seed = accent;
            if self.configuration.borrow().mode == ColorMode::System {
                self.refresh();
            }
        }
        if first || changed {
            self.system_accent_listeners
                .borrow_mut()
                .retain_mut(|listener| listener(accent));
        }
    }
}

impl Drop for Inner {
    fn drop(&mut self) {
        gtk4::style_context_remove_provider_for_display(&self.display, &self.provider);
    }
}

#[derive(Clone)]
pub struct Appearance(Rc<Inner>);

impl Appearance {
    pub fn new(settings: &Settings) -> Self {
        let inner = Rc::new(Inner {
            provider: CssProvider::new(),
            display: gdk::Display::default().expect("No display"),
            configuration: RefCell::new(Configuration::new(settings, None)),
            system_accent: Cell::new(None),
            system_accent_known: Cell::new(false),
            system_accent_listeners: RefCell::new(Vec::new()),
            _system_accent_monitor: RefCell::new(None),
        });
        inner.refresh();
        gtk4::style_context_add_provider_for_display(
            &inner.display,
            &inner.provider,
            gtk4::STYLE_PROVIDER_PRIORITY_APPLICATION + 1,
        );
        let weak = Rc::downgrade(&inner);
        adw::StyleManager::default().connect_dark_notify(move |_| {
            if let Some(inner) = weak.upgrade() {
                inner.refresh();
            }
        });
        let weak = Rc::downgrade(&inner);
        adw::StyleManager::default().connect_high_contrast_notify(move |_| {
            if let Some(inner) = weak.upgrade() {
                inner.refresh();
            }
        });
        let weak = Rc::downgrade(&inner);
        crate::i18n::on_locale_changed(move || {
            if let Some(inner) = weak.upgrade() {
                inner.refresh();
                true
            } else {
                false
            }
        });
        let weak = Rc::downgrade(&inner);
        let monitor = crate::system_accent::SystemAccentMonitor::new(move |accent| {
            if let Some(inner) = weak.upgrade() {
                inner.set_system_accent(accent.and_then(|accent| Color::parse(&accent.hex())));
            }
        });
        inner._system_accent_monitor.replace(Some(monitor));
        Self(inner)
    }

    pub fn update(&self, settings: &Settings) {
        *self.0.configuration.borrow_mut() =
            Configuration::new(settings, self.0.system_accent.get());
        self.0.refresh();
    }

    pub fn system_accent(&self) -> Option<Color> {
        self.0.system_accent.get()
    }

    pub fn on_system_accent_changed(&self, listener: impl Fn(Option<Color>) -> bool + 'static) {
        if self.0.system_accent_known.get() && !listener(self.0.system_accent.get()) {
            return;
        }
        self.0
            .system_accent_listeners
            .borrow_mut()
            .push(Box::new(listener));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colors_require_six_hex_digits_and_canonicalize_without_css_injection() {
        for value in [
            "",
            "#123",
            "12345678",
            "#GG00AA",
            "#000000; color: red",
            "rgb(0,0,0)",
            "００００００",
        ] {
            assert!(Color::parse(value).is_none(), "{value}");
        }
        assert_eq!(Color::parse(" #ab12cD ").unwrap().hex(), "#AB12CD");
        assert_eq!(Color::parse("009688").unwrap().hex(), "#009688");
    }

    #[test]
    fn palettes_keep_text_and_controls_readable_for_extreme_custom_colors() {
        for mode in [
            ColorMode::System,
            ColorMode::LocalSend,
            ColorMode::Oled,
            ColorMode::Yaru,
            ColorMode::Custom,
        ] {
            for seed in [
                BLACK,
                WHITE,
                Color(255, 0, 0),
                Color(0, 255, 0),
                Color(0, 0, 255),
                Color(255, 255, 0),
                Color(119, 119, 119),
            ] {
                for dark in [false, true] {
                    let palette = Palette::new(mode, seed, dark, false);
                    for background in [
                        palette.surface,
                        palette.rail,
                        palette.card,
                        palette.selected,
                    ] {
                        assert!(
                            palette.text.contrast(background) >= 4.5,
                            "body {mode:?}, {seed:?}, {dark}"
                        );
                        if mode != ColorMode::Yaru {
                            assert!(
                                palette.primary.contrast(background) >= 4.5,
                                "accent {mode:?}, {seed:?}, {dark}"
                            );
                        }
                    }
                    for background in [palette.surface, palette.card] {
                        assert!(
                            palette.secondary.contrast(background) >= 4.5,
                            "secondary {mode:?}, {seed:?}, {dark}"
                        );
                        if mode != ColorMode::Yaru {
                            assert!(
                                palette.outline.contrast(background) >= 3.0,
                                "outline {mode:?}, {seed:?}, {dark}"
                            );
                        }
                    }
                    assert!(
                        palette
                            .primary
                            .contrast(BLACK)
                            .max(palette.primary.contrast(WHITE))
                            >= 4.5
                    );
                    assert!(palette.on_selected.contrast(palette.selected) >= 4.5);
                    assert!(palette.on_device.contrast(palette.device) >= 4.5);
                }
            }
        }
    }

    #[test]
    fn tonal_spot_roles_match_pinned_dart_013_reference_vectors() {
        // Generated by the unmodified pub.dev material_color_utilities 0.13.0
        // package using SchemeTonalSpot(Hct.fromInt(seed), dark, contrastLevel: 0).
        // This is the dependency pinned by LocalSend's Flutter 3.41 reference.
        let cases = [
            (
                "009688",
                false,
                [
                    "F4FBF8", "EFF5F2", "E9EFED", "E3EAE7", "161D1C", "3F4947", "006A60", "FFFFFF",
                    "CCE8E2", "334B47", "6F7977", "BA1A1A", "FFFFFF", "2B3230", "ECF2EF",
                ],
            ),
            (
                "009688",
                true,
                [
                    "0E1513", "161D1C", "1A2120", "252B2A", "DDE4E1", "BEC9C6", "82D5C8", "003731",
                    "334B47", "CCE8E2", "899390", "FFB4AB", "690005", "DDE4E1", "2B3230",
                ],
            ),
            (
                "7357C8",
                false,
                [
                    "FDF7FF", "F7F2FA", "F2ECF4", "ECE6EE", "1C1B20", "48454E", "63568F", "FFFFFF",
                    "E7DEF8", "494458", "79757F", "BA1A1A", "FFFFFF", "322F35", "F5EFF7",
                ],
            ),
            (
                "7357C8",
                true,
                [
                    "141318", "1C1B20", "211F24", "2B292F", "E6E1E9", "CAC4CF", "CDBDFF", "34275E",
                    "494458", "E7DEF8", "938F99", "FFB4AB", "690005", "E6E1E9", "322F35",
                ],
            ),
            (
                "FF0000",
                false,
                [
                    "FFF8F6", "FFF0EE", "FCEAE7", "F7E4E1", "231918", "534341", "904B40", "FFFFFF",
                    "FFDAD4", "5D3F3B", "857370", "BA1A1A", "FFFFFF", "392E2C", "FFEDEA",
                ],
            ),
            (
                "FF0000",
                true,
                [
                    "1A1110", "231918", "271D1C", "322826", "F1DFDC", "D8C2BE", "FFB4A8", "561E16",
                    "5D3F3B", "FFDAD4", "A08C89", "FFB4AB", "690005", "F1DFDC", "392E2C",
                ],
            ),
            (
                "000000",
                false,
                [
                    "FFF8F8", "FFF0F2", "FAEAED", "F5E4E7", "22191C", "514347", "8C4A60", "FFFFFF",
                    "FFD9E2", "5A3F47", "837377", "BA1A1A", "FFFFFF", "372E30", "FDEDEF",
                ],
            ),
            (
                "000000",
                true,
                [
                    "191113", "22191C", "261D20", "31282A", "EFDFE1", "D5C2C6", "FFB1C8", "541D32",
                    "5A3F47", "FFD9E2", "9E8C90", "FFB4AB", "690005", "EFDFE1", "372E30",
                ],
            ),
            (
                "FFFFFF",
                false,
                [
                    "F5FAFB", "EFF5F6", "E9EFF0", "E3E9EA", "171D1E", "3F484A", "006874", "FFFFFF",
                    "CDE7EC", "334B4F", "6F797A", "BA1A1A", "FFFFFF", "2B3133", "ECF2F3",
                ],
            ),
            (
                "FFFFFF",
                true,
                [
                    "0E1415", "171D1E", "1B2122", "252B2C", "DEE3E5", "BFC8CA", "82D3E0", "00363D",
                    "334B4F", "CDE7EC", "899294", "FFB4AB", "690005", "DEE3E5", "2B3133",
                ],
            ),
        ];
        for (seed, dark, expected) in cases {
            let palette = Palette::new(ColorMode::Custom, Color::parse(seed).unwrap(), dark, false);
            let actual = [
                palette.surface,
                palette.card,
                palette.bottom,
                palette.dialog,
                palette.text,
                palette.secondary,
                palette.primary,
                palette.on_primary,
                palette.input,
                palette.on_selected,
                palette.outline,
                palette.error,
                palette.on_error,
                palette.toast,
                palette.on_toast,
            ];
            assert_eq!(
                actual,
                expected.map(|color| Color::parse(color).unwrap()),
                "seed={seed}, dark={dark}"
            );
        }
    }

    #[test]
    fn local_send_oled_and_yaru_match_upstream_surface_rules() {
        let teal = Color(0, 150, 136);
        for dark in [false, true] {
            let custom = Palette::new(ColorMode::Custom, teal, dark, false);
            let standard = Palette::new(ColorMode::LocalSend, WHITE, dark, false);
            assert_eq!(standard.primary, custom.primary);
            assert_eq!(
                standard.rail,
                standard.surface.mix(standard.primary, 13.0 / 255.0)
            );
            assert_eq!(
                standard.device,
                if dark { standard.input } else { standard.card }
            );
            assert_eq!(
                standard.picker,
                if dark { standard.input } else { standard.card }
            );
        }
        let oled = Palette::new(ColorMode::LocalSend, teal, true, true);
        let dark = Palette::new(ColorMode::LocalSend, teal, true, false);
        assert_eq!(oled.surface, BLACK);
        assert_eq!(oled.card, dark.card);
        assert_eq!(oled.input, dark.input);
        assert_eq!(oled.rail, BLACK.mix(dark.primary, 13.0 / 255.0));

        // Yaru 10.2.0 creates these with the orange/jet/porcelain constants and
        // the HSL scale/cap operations in lib/src/themes/common_themes.dart.
        for (dark, expected) in [
            (
                false,
                [
                    "FFFFFF", "F3F3F3", "F2F2F2", "363636", "4D4D4D", "E95420", "F6E8E4", "CCCCCC",
                    "FFFFFF",
                ],
            ),
            (
                true,
                [
                    "202020", "2A2A2A", "2B2B2B", "FAFAFA", "E1E1E1", "E95420", "40302A", "3F3F3F",
                    "1A1A1A",
                ],
            ),
        ] {
            let palette = Palette::new(ColorMode::Yaru, teal, dark, false);
            assert_eq!(
                [
                    palette.surface,
                    palette.rail,
                    palette.card,
                    palette.text,
                    palette.secondary,
                    palette.primary,
                    palette.input,
                    palette.outline,
                    palette.dialog
                ],
                expected.map(|color| Color::parse(color).unwrap())
            );
            assert_eq!(palette.outlined, palette.text);
        }
    }

    #[test]
    fn system_mode_uses_the_portal_seed_and_falls_back_to_localsend_teal() {
        let violet = Color(115, 87, 200);
        let settings = Settings {
            color_mode: ColorMode::System,
            ..Settings::default()
        };
        let available = Configuration::new(&settings, Some(violet));
        let unavailable = Configuration::new(&settings, None);
        assert_eq!(available.seed(), violet);
        assert_eq!(unavailable.seed(), LOCALSEND_TEAL);

        for dark in [false, true] {
            let system = Palette::new(ColorMode::System, available.seed(), dark, false);
            let custom = Palette::new(ColorMode::Custom, violet, dark, false);
            assert_eq!(
                [
                    system.surface,
                    system.card,
                    system.text,
                    system.primary,
                    system.on_primary,
                    system.selected,
                    system.on_selected,
                ],
                [
                    custom.surface,
                    custom.card,
                    custom.text,
                    custom.primary,
                    custom.on_primary,
                    custom.selected,
                    custom.on_selected,
                ]
            );

            let fallback = Palette::new(ColorMode::System, unavailable.seed(), dark, false);
            let localsend = Palette::new(ColorMode::LocalSend, violet, dark, false);
            assert_eq!(fallback.primary, localsend.primary);
            assert_eq!(fallback.surface, localsend.surface);
        }

        let oled_settings = Settings {
            color_mode: ColorMode::Oled,
            oled: false,
            ..Settings::default()
        };
        assert!(Configuration::new(&oled_settings, Some(violet)).oled());
    }

    #[test]
    fn high_contrast_palettes_strengthen_text_and_controls_including_oled() {
        for mode in [
            ColorMode::System,
            ColorMode::LocalSend,
            ColorMode::Oled,
            ColorMode::Yaru,
            ColorMode::Custom,
        ] {
            for seed in [
                BLACK,
                WHITE,
                Color(255, 0, 0),
                Color(0, 255, 0),
                Color(0, 0, 255),
                Color(255, 255, 0),
                Color(119, 119, 119),
            ] {
                for dark in [false, true] {
                    for oled in [false, true] {
                        let mut palette = Palette::new(mode, seed, dark, oled);
                        palette.apply_high_contrast(dark);
                        for background in [
                            palette.surface,
                            palette.rail,
                            palette.card,
                            palette.selected,
                            palette.bottom,
                            palette.dialog,
                            palette.input,
                            palette.picker,
                            palette.device,
                        ] {
                            for foreground in [palette.text, palette.secondary, palette.primary] {
                                assert!(
                                    foreground.contrast(background) >= 7.0,
                                    "text {mode:?}, {seed:?}, dark={dark}, oled={oled}"
                                );
                            }
                            assert!(
                                palette.outline.contrast(background) >= 4.5,
                                "outline {mode:?}, {seed:?}, dark={dark}, oled={oled}"
                            );
                        }
                        assert!(
                            palette
                                .primary
                                .contrast(BLACK)
                                .max(palette.primary.contrast(WHITE))
                                >= 7.0
                        );
                        assert!(palette.on_primary.contrast(palette.primary) >= 7.0);
                        assert!(palette.on_picker.contrast(palette.picker) >= 7.0);
                        assert!(palette.on_picker.contrast(palette.selected) >= 7.0);
                        assert!(palette.on_device.contrast(palette.device) >= 7.0);
                        assert!(palette.on_selected.contrast(palette.selected) >= 7.0);
                    }
                }
            }
        }
    }
}
