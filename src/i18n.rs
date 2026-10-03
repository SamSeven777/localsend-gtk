//! Explicit GTK-thread localization. Only authored interface text is bound;
//! filenames, aliases, entry contents and received messages are never scanned.
use gtk4::{self as gtk, prelude::*};
use serde::{Deserialize, Serialize};
use std::{cell::RefCell, collections::HashMap, rc::Rc, sync::OnceLock};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Locale {
    #[default]
    #[serde(rename = "system")]
    System,
    #[serde(rename = "en")]
    En,
    #[serde(rename = "zh-CN", alias = "zh_CN")]
    ZhCn,
    #[serde(rename = "zh-TW", alias = "zh_TW")]
    ZhTw,
    #[serde(rename = "ja")]
    Ja,
    #[serde(rename = "ko")]
    Ko,
    #[serde(rename = "de")]
    De,
    #[serde(rename = "fr")]
    Fr,
    #[serde(rename = "es", alias = "es-ES", alias = "es_ES")]
    Es,
    #[serde(rename = "ru")]
    Ru,
}

impl Locale {
    pub const ALL: [Self; 10] = [
        Self::System,
        Self::En,
        Self::ZhCn,
        Self::ZhTw,
        Self::Ja,
        Self::Ko,
        Self::De,
        Self::Fr,
        Self::Es,
        Self::Ru,
    ];

    pub fn native_name(self) -> &'static str {
        match self {
            Self::System => "System",
            Self::En => "English",
            Self::ZhCn => "简体中文",
            Self::ZhTw => "繁體中文",
            Self::Ja => "日本語",
            Self::Ko => "한국어",
            Self::De => "Deutsch",
            Self::Fr => "Français",
            Self::Es => "Español",
            Self::Ru => "Русский",
        }
    }

    pub fn from_language_tag(tag: &str) -> Option<Self> {
        let normalized = tag
            .split(['.', '@'])
            .next()?
            .replace('_', "-")
            .to_ascii_lowercase();
        let parts: Vec<_> = normalized.split('-').collect();
        match parts.first().copied()? {
            "en" | "c" | "posix" => Some(Self::En),
            "zh" if parts.contains(&"hans") => Some(Self::ZhCn),
            "zh" if parts
                .iter()
                .any(|part| matches!(*part, "hant" | "tw" | "hk" | "mo")) =>
            {
                Some(Self::ZhTw)
            }
            "zh" => Some(Self::ZhCn),
            "ja" => Some(Self::Ja),
            "ko" => Some(Self::Ko),
            "de" => Some(Self::De),
            "fr" => Some(Self::Fr),
            "es" => Some(Self::Es),
            "ru" => Some(Self::Ru),
            _ => None,
        }
    }

    pub fn resolve(self) -> Self {
        if self != Self::System {
            return self;
        }
        // GLib honors LANGUAGE/LC_ALL/LC_MESSAGES/LANG on Linux and the user's
        // locale on Windows. Unsupported languages fall back to English.
        glib::language_names()
            .iter()
            .find_map(|tag| Self::from_language_tag(tag))
            .unwrap_or(Self::En)
    }
}

#[derive(Clone, Deserialize)]
#[serde(untagged)]
enum Translation {
    Text(String),
    Plural(PluralForms),
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct PluralForms {
    one: Option<String>,
    few: Option<String>,
    many: Option<String>,
    other: String,
}

impl Translation {
    fn template(&self, category: &str) -> &str {
        match self {
            Self::Text(text) => text,
            Self::Plural(forms) => match category {
                "one" => forms.one.as_deref(),
                "few" => forms.few.as_deref(),
                "many" => forms.many.as_deref(),
                _ => None,
            }
            .unwrap_or(&forms.other),
        }
    }
}

type Catalog = HashMap<String, Translation>;
fn catalogs() -> &'static [Catalog; 9] {
    static CATALOGS: OnceLock<[Catalog; 9]> = OnceLock::new();
    CATALOGS.get_or_init(|| {
        [
            (
                include_str!("../assets/i18n/en.json"),
                include_str!("../assets/i18n/gtk-en.json"),
            ),
            (
                include_str!("../assets/i18n/zh-CN.json"),
                include_str!("../assets/i18n/gtk-zh-CN.json"),
            ),
            (
                include_str!("../assets/i18n/zh-TW.json"),
                include_str!("../assets/i18n/gtk-zh-TW.json"),
            ),
            (
                include_str!("../assets/i18n/ja.json"),
                include_str!("../assets/i18n/gtk-ja.json"),
            ),
            (
                include_str!("../assets/i18n/ko.json"),
                include_str!("../assets/i18n/gtk-ko.json"),
            ),
            (
                include_str!("../assets/i18n/de.json"),
                include_str!("../assets/i18n/gtk-de.json"),
            ),
            (
                include_str!("../assets/i18n/fr.json"),
                include_str!("../assets/i18n/gtk-fr.json"),
            ),
            (
                include_str!("../assets/i18n/es.json"),
                include_str!("../assets/i18n/gtk-es.json"),
            ),
            (
                include_str!("../assets/i18n/ru.json"),
                include_str!("../assets/i18n/gtk-ru.json"),
            ),
        ]
        .map(|(official, native)| {
            let mut catalog: Catalog =
                serde_json::from_str(official).expect("Bundled official locale catalog");
            catalog.extend(
                serde_json::from_str::<Catalog>(native).expect("Bundled native locale catalog"),
            );
            catalog
        })
    })
}

fn catalog(locale: Locale) -> &'static Catalog {
    let index = match locale.resolve() {
        Locale::ZhCn => 1,
        Locale::ZhTw => 2,
        Locale::Ja => 3,
        Locale::Ko => 4,
        Locale::De => 5,
        Locale::Fr => 6,
        Locale::Es => 7,
        Locale::Ru => 8,
        _ => 0,
    };
    &catalogs()[index]
}

fn translate(locale: Locale, source: &str) -> String {
    catalog(locale)
        .get(source)
        .map(|translation| translation.template("other").to_owned())
        .unwrap_or_else(|| source.to_owned())
}

// Integer cardinal forms used by the supported locales, following CLDR.
// Counts are integral file/recipient totals, never decimal measurements.
fn plural_category(locale: Locale, count: u64) -> &'static str {
    match locale.resolve() {
        Locale::Ru if count % 10 == 1 && count % 100 != 11 => "one",
        Locale::Ru if (2..=4).contains(&(count % 10)) && !(12..=14).contains(&(count % 100)) => {
            "few"
        }
        Locale::Ru => "many",
        Locale::ZhCn | Locale::ZhTw | Locale::Ja | Locale::Ko => "other",
        Locale::Fr if count <= 1 => "one",
        Locale::Fr | Locale::Es if count != 0 && count.is_multiple_of(1_000_000) => "many",
        _ if count == 1 => "one",
        _ => "other",
    }
}

fn translate_plural(locale: Locale, one: &str, other: &str, count: u64) -> String {
    let category = plural_category(locale, count);
    if let Some(translation @ Translation::Plural(_)) = catalog(locale).get(other) {
        return translation.template(category).to_owned();
    }
    // Legacy singular templates can say "a file" without a numeric placeholder.
    // They are valid only for exactly one, even if the locale groups 0 with 1.
    translate(
        locale,
        if count == 1 && category == "one" {
            one
        } else {
            other
        },
    )
}

fn format_plural(
    locale: Locale,
    one: &str,
    other: &str,
    count: u64,
    arguments: &[(&str, String)],
) -> String {
    let mut arguments = arguments.to_vec();
    arguments.insert(0, ("n", count.to_string()));
    substitute(&translate_plural(locale, one, other, count), &arguments)
}

/// One pass substitution preserves braces in user-supplied values verbatim.
fn substitute(template: &str, arguments: &[(&str, String)]) -> String {
    let mut result = String::with_capacity(template.len());
    let mut remaining = template;
    while let Some(start) = remaining.find('{') {
        result.push_str(&remaining[..start]);
        let tail = &remaining[start..];
        let Some(end) = tail.find('}') else {
            result.push_str(tail);
            return result;
        };
        let name = &tail[1..end];
        if let Some((_, value)) = arguments.iter().find(|(key, _)| *key == name) {
            result.push_str(value);
        } else {
            result.push_str(&tail[..=end]);
        }
        remaining = &tail[end + 1..];
    }
    result.push_str(remaining);
    result
}

type ApplyBinding = Rc<dyn Fn(&glib::Object, Locale)>;
#[derive(Clone)]
struct Binding {
    object: glib::WeakRef<glib::Object>,
    slot: String,
    apply: ApplyBinding,
}

struct State {
    locale: Locale,
    updating: bool,
    bindings: Vec<Binding>,
    listeners: Vec<Rc<dyn Fn() -> bool>>,
}
thread_local! {
    static STATE: RefCell<State> = RefCell::new(State {
        locale: Locale::System.resolve(),
        updating: false,
        bindings: Vec::new(),
        listeners: Vec::new(),
    });
}

pub fn current() -> Locale {
    STATE.with(|state| state.borrow().locale)
}

/// Guard selection/value callbacks while translated properties are refreshed.
pub fn is_updating() -> bool {
    STATE.with(|state| state.borrow().updating)
}

pub fn tr(source: &str) -> String {
    translate(current(), source)
}

pub fn tr_format(source: &str, arguments: &[(&str, String)]) -> String {
    substitute(&tr(source), arguments)
}

pub fn tr_plural(one: &str, other: &str, count: u64, arguments: &[(&str, String)]) -> String {
    format_plural(current(), one, other, count, arguments)
}

pub fn set_locale(locale: Locale) {
    let effective = locale.resolve();
    let changed = STATE.with(|state| {
        let mut state = state.borrow_mut();
        let changed = state.locale != effective;
        state.locale = effective;
        changed
    });
    if changed && !is_updating() {
        notify_bindings();
    }
}

fn notify_bindings() {
    STATE.with(|state| state.borrow_mut().updating = true);
    loop {
        let (locale, bindings, listeners) = STATE.with(|state| {
            let state = state.borrow();
            (
                state.locale,
                state.bindings.clone(),
                state.listeners.clone(),
            )
        });
        // Never hold a RefCell borrow while GTK emits property notifications.
        for binding in bindings {
            if let Some(object) = binding.object.upgrade() {
                (binding.apply)(&object, locale);
            }
        }
        let expired: Vec<_> = listeners
            .into_iter()
            .filter(|listener| !listener())
            .collect();
        let finished = STATE.with(|state| {
            let mut state = state.borrow_mut();
            state
                .bindings
                .retain(|binding| binding.object.upgrade().is_some());
            state
                .listeners
                .retain(|listener| !expired.iter().any(|old| Rc::ptr_eq(listener, old)));
            if state.locale == locale {
                state.updating = false;
                true
            } else {
                false
            }
        });
        if finished {
            break;
        }
    }
}

/// Return false once the weak owner has disappeared; stale listeners are pruned.
pub(crate) fn on_locale_changed(callback: impl Fn() -> bool + 'static) {
    STATE.with(|state| state.borrow_mut().listeners.push(Rc::new(callback)));
}

fn register(object: &glib::Object, slot: String, apply: impl Fn(&glib::Object, Locale) + 'static) {
    let binding = Binding {
        object: object.downgrade(),
        slot,
        apply: Rc::new(apply),
    };
    let (locale, was_updating) = STATE.with(|state| {
        let mut state = state.borrow_mut();
        state.bindings.retain(|old| {
            old.object
                .upgrade()
                .is_some_and(|target| target != *object || old.slot != binding.slot)
        });
        state.bindings.push(binding.clone());
        let was_updating = state.updating;
        state.updating = true;
        (state.locale, was_updating)
    });
    (binding.apply)(object, locale);
    STATE.with(|state| state.borrow_mut().updating = was_updating);
    if !was_updating && current() != locale {
        notify_bindings();
    }
}

pub fn bind_property(object: &impl IsA<glib::Object>, property: &str, source: &str) {
    bind_format_property(object, property, source, &[]);
}

pub fn button(source: &str) -> gtk::Button {
    let button = gtk::Button::new();
    bind_property(&button, "label", source);
    button
}

pub fn bind_format_property(
    object: &impl IsA<glib::Object>,
    property: &str,
    source: &str,
    arguments: &[(&str, String)],
) {
    let source = source.to_owned();
    let property = property.to_owned();
    let arguments: Vec<_> = arguments
        .iter()
        .map(|(key, value)| ((*key).to_owned(), value.clone()))
        .collect();
    register(
        object.as_ref(),
        format!("property:{property}"),
        move |object, locale| {
            let args: Vec<_> = arguments
                .iter()
                .map(|(key, value)| (key.as_str(), value.clone()))
                .collect();
            object.set_property(&property, substitute(&translate(locale, &source), &args));
        },
    );
}

/// Keep the count and both templates so changing language also changes grammar.
pub fn bind_plural_property(
    object: &impl IsA<glib::Object>,
    property: &str,
    one: &str,
    other: &str,
    count: u64,
    arguments: &[(&str, String)],
) {
    let one = one.to_owned();
    let other = other.to_owned();
    let property = property.to_owned();
    let arguments: Vec<_> = arguments
        .iter()
        .map(|(key, value)| ((*key).to_owned(), value.clone()))
        .collect();
    register(
        object.as_ref(),
        format!("property:{property}"),
        move |object, locale| {
            let args: Vec<_> = arguments
                .iter()
                .map(|(key, value)| (key.as_str(), value.clone()))
                .collect();
            object.set_property(&property, format_plural(locale, &one, &other, count, &args));
        },
    );
}

fn update_strings(list: &gtk::StringList, sources: &[String], locale: Locale) {
    let values: Vec<_> = sources
        .iter()
        .map(|source| translate(locale, source))
        .collect();
    if values.len() == list.n_items() as usize
        && values
            .iter()
            .enumerate()
            .all(|(index, value)| list.string(index as u32).as_deref() == Some(value.as_str()))
    {
        return;
    }
    let values: Vec<_> = values.iter().map(String::as_str).collect();
    list.splice(0, list.n_items(), &values);
}

pub fn bind_combo_strings(row: &adw::ComboRow, sources: &[&str]) {
    use adw::prelude::ComboRowExt;
    let sources: Vec<_> = sources.iter().map(|source| (*source).to_owned()).collect();
    register(
        row.upcast_ref(),
        "combo-strings".into(),
        move |object, locale| {
            let row = object
                .downcast_ref::<adw::ComboRow>()
                .expect("ComboRow binding");
            let list = row
                .model()
                .and_downcast::<gtk::StringList>()
                .expect("ComboRow StringList model");
            let selected = row.selected();
            update_strings(&list, &sources, locale);
            // Splicing StringList replaces immutable StringObjects, which can reset
            // selection. Restore it before is_updating() becomes false.
            row.set_selected(selected);
        },
    );
}

pub fn bind_response(dialog: &adw::AlertDialog, id: &str, source: &str) {
    use adw::prelude::AlertDialogExt;
    let id = id.to_owned();
    let source = source.to_owned();
    register(
        dialog.upcast_ref(),
        format!("response:{id}"),
        move |object, locale| {
            let dialog = object
                .downcast_ref::<adw::AlertDialog>()
                .expect("AlertDialog binding");
            // Completed transfers remove their Cancel response while retaining the
            // dialog. A later language change must not recreate or address it.
            if dialog.has_response(&id) {
                dialog.set_response_label(&id, &translate(locale, &source));
            }
        },
    );
}

pub fn bind_accessible_label(widget: &impl IsA<gtk::Widget>, source: &str) {
    let source = source.to_owned();
    register(
        widget.as_ref().upcast_ref(),
        "accessible:label".into(),
        move |object, locale| {
            let widget = object
                .downcast_ref::<gtk::Widget>()
                .expect("Widget binding");
            widget.update_property(&[gtk::accessible::Property::Label(&translate(
                locale, &source,
            ))]);
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn locale_tags_resolve_region_script_and_encoding() {
        for (tag, expected) in [
            ("zh_CN.UTF-8", Locale::ZhCn),
            ("zh-Hans-SG", Locale::ZhCn),
            ("zh-Hans-TW", Locale::ZhCn),
            ("zh-Hant", Locale::ZhTw),
            ("zh_HK", Locale::ZhTw),
            ("zh-MO", Locale::ZhTw),
            ("ja_JP", Locale::Ja),
            ("ko-KR", Locale::Ko),
            ("de_DE.UTF-8", Locale::De),
            ("fr_FR", Locale::Fr),
            ("es-ES", Locale::Es),
            ("ru_RU.UTF-8", Locale::Ru),
            ("en_GB.UTF-8", Locale::En),
            ("C", Locale::En),
        ] {
            assert_eq!(Locale::from_language_tag(tag), Some(expected));
        }
        assert_eq!(Locale::from_language_tag("it_IT"), None);
        assert_eq!(Locale::from_language_tag(""), None);
        for locale in Locale::ALL {
            let encoded = serde_json::to_string(&locale).unwrap();
            assert_eq!(serde_json::from_str::<Locale>(&encoded).unwrap(), locale);
        }
        assert!(serde_json::from_str::<Locale>("\"invalid\"").is_err());
    }

    fn placeholders(text: &str) -> BTreeSet<&str> {
        text.split('{')
            .skip(1)
            .filter_map(|part| part.split_once('}').map(|(name, _)| name))
            .collect()
    }

    #[test]
    fn catalogs_cover_same_keys_and_preserve_template_arguments() {
        let english = &catalogs()[0];
        assert!(english.len() > 250);
        for catalog in catalogs() {
            assert_eq!(
                english.keys().collect::<BTreeSet<_>>(),
                catalog.keys().collect()
            );
            for (source, translated) in catalog {
                for category in ["one", "few", "many", "other"] {
                    let template = translated.template(category);
                    assert!(!template.is_empty(), "{source}: {category}");
                    assert!(
                        !template.contains("@:"),
                        "Unresolved Slang reference: {source}"
                    );
                    assert_eq!(placeholders(source), placeholders(template), "{source}");
                }
            }
        }
    }

    #[test]
    fn upstream_terminology_and_unknown_text_are_preserved() {
        assert_eq!(translate(Locale::ZhCn, "Receive"), "接收");
        assert_eq!(translate(Locale::ZhTw, "Settings"), "設定");
        assert_eq!(translate(Locale::Ja, "Send"), "送信");
        assert_eq!(translate(Locale::Ko, "Cancel"), "취소");
        assert_eq!(translate(Locale::De, "Receive"), "Empfangen");
        assert_eq!(translate(Locale::Fr, "Receive"), "Recevoir");
        assert_eq!(translate(Locale::Es, "Receive"), "Recibir");
        assert_eq!(translate(Locale::Ru, "Receive"), "Получить");
        assert_eq!(
            translate(Locale::ZhCn, "My file {name}.png"),
            "My file {name}.png"
        );
        assert_eq!(
            substitute(
                "{name}: {n}",
                &[("name", "{n}青い".into()), ("n", "2".into())]
            ),
            "{n}青い: 2"
        );
        assert_eq!(substitute("Unmatched {name", &[]), "Unmatched {name");
    }

    #[test]
    fn plural_rules_switch_without_affecting_other_threads() {
        set_locale(Locale::En);
        assert_eq!(tr_plural("{n} file", "{n} files", 1, &[]), "1 file");
        assert_eq!(tr_plural("{n} file", "{n} files", 0, &[]), "0 files");
        set_locale(Locale::Ru);
        assert_eq!(tr_plural("{n} file", "{n} files", 21, &[]), "21 файл");
        std::thread::spawn(|| {
            set_locale(Locale::En);
            assert_eq!(tr_plural("{n} file", "{n} files", 21, &[]), "21 files");
        })
        .join()
        .unwrap();
        assert_eq!(current(), Locale::Ru);
        set_locale(Locale::ZhCn);
        assert_eq!(tr_plural("{n} file", "{n} files", 1, &[]), "1 个文件");
        set_locale(Locale::En);
    }

    #[test]
    fn russian_counted_messages_follow_integer_cardinal_rules() {
        for (count, category, file, item, download) in [
            (0, "many", "файлов", "элементов", "активных загрузок"),
            (1, "one", "файл", "элемент", "активная загрузка"),
            (2, "few", "файла", "элемента", "активные загрузки"),
            (4, "few", "файла", "элемента", "активные загрузки"),
            (5, "many", "файлов", "элементов", "активных загрузок"),
            (11, "many", "файлов", "элементов", "активных загрузок"),
            (14, "many", "файлов", "элементов", "активных загрузок"),
            (21, "one", "файл", "элемент", "активная загрузка"),
            (22, "few", "файла", "элемента", "активные загрузки"),
            (25, "many", "файлов", "элементов", "активных загрузок"),
            (111, "many", "файлов", "элементов", "активных загрузок"),
            (112, "many", "файлов", "элементов", "активных загрузок"),
            (121, "one", "файл", "элемент", "активная загрузка"),
        ] {
            assert_eq!(plural_category(Locale::Ru, count), category);
            assert_eq!(
                format_plural(Locale::Ru, "{n} file", "{n} files", count, &[]),
                format!("{count} {file}")
            );
            assert_eq!(
                format_plural(Locale::Ru, "{n} item", "{n} items", count, &[]),
                format!("{count} {item}")
            );
            assert_eq!(
                format_plural(
                    Locale::Ru,
                    "wants to send you a file · {size}",
                    "wants to send you {n} files · {size}",
                    count,
                    &[("size", "42 MB".into())]
                ),
                format!("хочет отправить вам {count} {file} · 42 MB")
            );
            assert_eq!(
                format_plural(
                    Locale::Ru,
                    "{name} wants to download a file · {size}",
                    "{name} wants to download {n} files · {size}",
                    count,
                    &[("name", "Device {n}".into()), ("size", "42 MB".into())]
                ),
                format!("Device {{n}} хочет скачать {count} {file} · 42 MB")
            );
            assert_eq!(
                format_plural(
                    Locale::Ru,
                    "{n} active download · {sent} / {total}",
                    "{n} active downloads · {sent} / {total}",
                    count,
                    &[("sent", "1 MB".into()), ("total", "42 MB".into())]
                ),
                format!("{count} {download} · 1 MB / 42 MB")
            );
        }
    }
}
