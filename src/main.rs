use std::{path::Path, sync::Arc};
use gtk::{prelude::*, glib, glib::clone};
use astra_lite::{ui, ui::gtk_utils::exec_and_show_error};
use astra_lite::{
    core::core::Core, options::*, utils::{io_utils::*, log_utils::*}
};

fn main() -> eyre::Result<()> {
    let application = gtk::Application::new(
        Some(&format!("com.github.art-den.{}", env!("CARGO_PKG_NAME"))),
        Default::default(),
    );
    application.connect_activate(app_activate_handler);
    application.run();
    Ok(())
}

fn app_activate_handler(app: &gtk::Application) {
    // Check if application is already running

    if let Some(window) = app.active_window() {
        log::info!("Launched twice. Activating main window...");
        window.present();
        return;
    }

    // Init logger and log startup

    let Ok(mut logs_dir) = get_app_dir() else {
        eprintln!("Can't get app dir!");
        return;
    };
    logs_dir.push("logs");
    cleanup_old_logs(&logs_dir, 14/*days*/);
    let start_log_res = start_logger(&logs_dir);
    if let Err(start_log_res) = start_log_res {
        eprintln!("Failed to start logger: {}!", start_log_res);
        return;
    }
    log::set_max_level(log::LevelFilter::Info);

    log::info!(
        "{} {} ver. {} is started",
        env!("CARGO_PKG_NAME"),
        std::env::consts::ARCH,
        env!("CARGO_PKG_VERSION")
    );

    // Enable stack trace in errors in debug builds

    if cfg!(debug_assertions) {
        unsafe { std::env::set_var("RUST_BACKTRACE", "full"); }
        log::set_max_level(log::LevelFilter::Debug);
    } else {
        unsafe { std::env::set_var("RUST_BACKTRACE", "0"); }
    }

    // Create core

    log::info!("Creating core...");
    let core = Core::new();

    // Register panic handler

    let indi_for_panic = Arc::clone(core.hal.indi_impl().indi());
    if cfg!(not(debug_assertions)) {
        std::panic::set_hook({
            let logs_dir = logs_dir.clone();
            let indi = Arc::clone(&indi_for_panic);
            let default_panic_handler = std::panic::take_hook();
            Box::new(move |panic_info| {
                panic_handler(
                    panic_info,
                    indi.is_drivers_started(),
                    &logs_dir,
                    &default_panic_handler
                )
            })
        });
    }

    // Load options

    exec_and_show_error(None::<&gtk::Window>, || {
        log::info!("Loading options...");
        let mut options = core.options.write().unwrap();
        load_json_from_config_file::<Options>(&mut options, "options")?;

        log::info!("Checking options...");
        options.check()?;

        drop(options);

        Ok(())
    });

    // Create UI

    log::info!("Building UI...");
    ui::ui_main::init_ui(app, &core, &logs_dir);

    // Connect shutdown signal

    app.connect_shutdown(clone!(@weak core => move |app| {
        app_shutdown_handler(app, &core);
    }));
}

fn app_shutdown_handler(_app: &gtk::Application, core: &Arc<Core>) {
    log::info!("Application shutdown signal");

    // Save options

    log::info!("Saving options...");
    let options = core.options.read().unwrap();
    _ = save_json_to_config::<Options>(&options, "options");
    drop(options);
    log::info!("Options saved");

    // Stop core

    log::info!("Core stopping...");
    core.stop();
    log::info!("Core stopped");

    dbg!(Arc::strong_count(core));
}

fn panic_handler(
    panic_info:        &std::panic::PanicHookInfo,
    stop_indi_servers: bool,
    logs_dir:          &Path,
    def_panic_handler: &(dyn Fn(&std::panic::PanicHookInfo<'_>) + 'static + Sync + Send),
) {
    let payload_str =
        if let Some(msg) = panic_info.payload().downcast_ref::<&'static str>() {
            Some(*msg)
        } else if let Some(msg) = panic_info.payload().downcast_ref::<String>() {
            Some(msg.as_str())
        } else {
            None
        };

    let payload = payload_str.unwrap_or_default();
    let location = panic_info.location().map(|loc| loc.to_string()).unwrap_or_default();

    eprintln!("{payload}");
    eprintln!("{location}");

    log::error!("(╯°□°）╯︵ ┻━┻ PANIC OCCURRED");

    if let Some(payload) = &payload_str {
        log::error!("Panic payload: {}", payload);
        eprintln!("PANIC: {}", payload);
    }

    if let Some(loc) = panic_info.location() {
        log::error!("Panic location: {}", loc);
    }

    log::error!(
        "Panic stacktrace: {}",
        std::backtrace::Backtrace::force_capture()
    );

    let message_caption = format!(
        "{} {} ver {} crashed ;-(",
        env!("CARGO_PKG_NAME"),
        std::env::consts::ARCH,
        env!("CARGO_PKG_VERSION")
    );

    let message_text = format!(
        "{payload}\n\nat {location}\n\n\nLook logs at\n{}",
        logs_dir.to_str().unwrap_or_default()
    );

    if stop_indi_servers && cfg!(target_os = "linux") {
        log::info!("Stop INDI server...");
        _ = std::process::Command::new("pkill")
            .args(["indiserver"])
            .spawn();
        log::info!("Done!");
    }

    _ = msgbox::create(&message_caption, &message_text, msgbox::IconType::Error);

    def_panic_handler(panic_info);
}
