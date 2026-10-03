//! Small, resizable building blocks shared by native dialogs.
use super::*;

pub(super) fn wrapped_label(text: &str, class: &str) -> gtk::Label {
    let text = label(text, class);
    text.set_wrap(true);
    text.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    text.set_max_width_chars(40);
    text
}

pub(super) fn translated_wrapped_label(text: &str, class: &str) -> gtk::Label {
    let label = wrapped_label("", class);
    i18n::bind_property(&label, "label", text);
    label
}

pub(super) fn content_scroll(
    child: &impl IsA<gtk::Widget>,
    natural_height: i32,
) -> gtk::ScrolledWindow {
    gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .propagate_natural_height(true)
        .max_content_height(natural_height)
        .child(child)
        .build()
}
