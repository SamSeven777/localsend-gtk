mod address;
mod alias_gen;
mod appearance;
mod desktop;
mod history;
mod http_client;
mod i18n;
mod network;
mod settings;
mod system_accent;
mod transfer;
mod ui;
mod web_receive;
mod web_share;

use adw::prelude::*;

fn main() -> glib::ExitCode {
    tracing_subscriber::fmt::init();
    if std::env::var_os("WAYLAND_DISPLAY").is_some() && std::env::var_os("GDK_BACKEND").is_none() {
        std::env::set_var("GDK_BACKEND", "wayland");
    }
    let runtime = tokio::runtime::Runtime::new().expect("Could not start network runtime");
    let _guard = runtime.enter();
    let app = adw::Application::builder()
        .application_id("org.localsend.localsend_gtk")
        .flags(gio::ApplicationFlags::HANDLES_OPEN)
        .build();
    app.add_main_option(
        "hidden",
        glib::Char::from(0),
        glib::OptionFlags::NONE,
        glib::OptionArg::None,
        "Start in the system tray when available",
        None,
    );
    app.add_main_option(
        "background",
        glib::Char::from(0),
        glib::OptionFlags::NONE,
        glib::OptionArg::None,
        "Start in background daemon mode",
        None,
    );
    let hidden = std::rc::Rc::new(std::cell::Cell::new(false));
    let requested_hidden = hidden.clone();
    app.connect_handle_local_options(move |_, options| {
        requested_hidden.set(options.contains("hidden") || options.contains("background"));
        -1
    });
    app.connect_startup(|_| ui::install_styles());

    let active_ui: std::rc::Rc<std::cell::RefCell<Option<ui::Ui>>> =
        std::rc::Rc::new(std::cell::RefCell::new(None));

    let ui_ref_activate = active_ui.clone();
    let hidden_ref_activate = hidden.clone();
    app.connect_activate(move |app| {
        if let Some(ui) = ui_ref_activate.borrow().as_ref() {
            ui.show_window();
            return;
        }
        match settings::Settings::load() {
            Ok(settings) => {
                let ui = ui::build(app, settings, false);
                if hidden_ref_activate.replace(false) {
                    ui.request_hidden_start();
                }
                *ui_ref_activate.borrow_mut() = Some(ui);
            }
            Err(error) => {
                let window = adw::ApplicationWindow::builder().application(app).title("LocalSend").build();
                window.present();
                let dialog = adw::AlertDialog::new(Some("Could not load settings"), Some(&format!("{error}\n\nCheck {} before restarting. Your saved settings have not been replaced.", settings::Settings::directory().display())));
                dialog.add_response("close", "Close");
                dialog.connect_response(None, glib::clone!(#[weak] app, move |_, _| app.quit()));
                dialog.present(Some(&window));
            }
        }
    });

    let ui_ref_open = active_ui.clone();
    app.connect_open(move |app, files, _hint| {
        // Use the same initialization and settings-error dialog as a normal
        // launch, without holding a RefCell borrow while activate creates Ui.
        app.activate();
        let ui = ui_ref_open.borrow().clone();
        if let Some(ui) = ui {
            ui.open_files(files);
        }
    });
    app.run()
}
