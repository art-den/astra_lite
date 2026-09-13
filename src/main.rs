use std::{path::Path, sync::Arc};
use gtk::{prelude::*, glib, glib::clone};
use astra_lite::{ui, ui::gtk_utils::{exec_and_show_error, open_logs_folder}};
use astra_lite::{
    core::engine::Engine, options::*, utils::{io_utils::*, log_utils::*}
};

// Option used to launch a separate instance that only shows the panic dialog
const PANIC_DIALOG_OPT: &str = "show-panic-dialog";

fn main() -> eyre::Result<()> {
    // The panic-dialog instance must not use GApplication:
    // - with the main app id, GApplication would forward the command line
    //   via D-Bus to the running primary instance (the one that is about
    //   to abort), so the dialog would never be shown;
    // - with a unique app id, "activate" would be emitted and the whole
    //   app (engine, UI) would start in the dialog process; the
    //   HANDLES_COMMAND_LINE flag does not help either: in this GLib
    //   version "activate" is not emitted after the command-line handler
    //   returns, so run() exits right away without starting the app.

    let mut args = std::env::args();
    let _ = args.next(); // program name
    let flag = format!("--{PANIC_DIALOG_OPT}");
    if args.next().as_deref() == Some(flag.as_str()) {
        if let Some(file_path) = args.next() {
            show_panic_dialog(&file_path);
            let _ = std::fs::remove_file(&file_path);
        }
        return Ok(());
    }

    let application = gtk::Application::new(
        Some(&format!("com.github.art-den.{}", env!("CARGO_PKG_NAME"))),
        Default::default(),
    );
    application.connect_activate(app_activate_handler);
    application.run();
    Ok(())
}

fn show_panic_dialog(file_path: &str) {
    if gtk::init().is_err() {
        eprintln!("Can't show panic dialog: failed to init GTK");
        return;
    }
    let message_text = std::fs::read_to_string(file_path).unwrap_or_default();

    let dialog = gtk::MessageDialog::builder()
        .message_type(gtk::MessageType::Error)
        .buttons(gtk::ButtonsType::Ok)
        .text(format!(
            "{} {} ver {} crashed ;-(",
            env!("CARGO_PKG_NAME"),
            std::env::consts::ARCH,
            env!("CARGO_PKG_VERSION")
        ))
        .secondary_text(message_text)
        .build();

    const OPEN_LOGS_RESPONSE: u16 = 1;
    // The same logs dir the main app uses

    let logs_dir = get_app_dir().ok().map(|mut d| { d.push("logs"); d });
    if logs_dir.is_some() {
        dialog.add_button(
            "Open logs folder",
            gtk::ResponseType::Other(OPEN_LOGS_RESPONSE),
        );
    }

    loop {
        match dialog.run() {
            gtk::ResponseType::Ok | gtk::ResponseType::Cancel | gtk::ResponseType::Close => break,
            gtk::ResponseType::Other(id) if id == OPEN_LOGS_RESPONSE => {
                if let Some(dir) = &logs_dir
                    && let Err(e) = open_logs_folder(dir) {
                    eprintln!("Can't open logs folder: {}", e);
                }
            }
            _ => break,
        }
    }
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

    log::info!("Creating engine...");
    let engine = Engine::new();

    // Register panic handler

    let indi_for_panic = Arc::clone(engine.hal.indi_impl().indi());
    if cfg!(not(debug_assertions)) {
        // The panic hook is only needed in a release build.
        // In a debug build, the debugger will automatically stop
        // at the point where the panic occurred.
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
        let mut options = engine.options.write().unwrap();
        load_json_from_config_file::<Options>(&mut options, "options")?;

        log::info!("Checking options...");
        options.check()?;

        drop(options);

        Ok(())
    });

    // Create UI

    log::info!("Building UI...");
    ui::ui_main::init_ui(app, &engine, &logs_dir);

    // Connect shutdown signal

    app.connect_shutdown(clone!(@weak engine => move |app| {
        app_shutdown_handler(app, &engine);
    }));
}

fn app_shutdown_handler(_app: &gtk::Application, engine: &Arc<Engine>) {
    log::info!("Application shutdown signal");

    // Save options

    log::info!("Saving options...");
    let options = engine.options.read().unwrap();
    _ = save_json_to_config::<Options>(&options, "options");
    drop(options);
    log::info!("Options saved");

    // Stop core

    log::info!("Core stopping...");
    engine.stop();
    log::info!("Core stopped");

    dbg!(Arc::strong_count(engine));
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
        } else {
            panic_info.payload().downcast_ref::<String>().map(|msg| msg.as_str())
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

    // Write the error text to a temp file and launch a separate instance of
    // this application to show the dialog. Showing a GTK dialog from the
    // panicking thread (usually a worker thread) is unsafe, so it is done
    // in a dedicated process.

    match std::env::current_exe() {
        Ok(exe) => {
            let tmp_file = std::env::temp_dir().join(format!(
                "{}_panic_{}.txt",
                env!("CARGO_PKG_NAME"),
                std::process::id()
            ));
            let message_text = format!(
                "{payload}\n\nat {location}\n\n\nLook logs at\n{}",
                logs_dir.to_str().unwrap_or_default()
            );
            if std::fs::write(&tmp_file, message_text).is_ok() {
                if let Err(e) = std::process::Command::new(exe)
                    .arg(format!("--{PANIC_DIALOG_OPT}"))
                    .arg(&tmp_file)
                    .spawn()
                {
                    log::error!("Failed to spawn panic dialog process: {}", e);
                }
            } else {
                log::error!("Failed to write panic info file: {}", tmp_file.display());
            }
        }
        Err(e) => log::error!("Failed to get current exe path: {}", e),
    }

    if stop_indi_servers && cfg!(target_os = "linux") {
        log::info!("Stop INDI server...");
        _ = std::process::Command::new("pkill")
            .args(["indiserver"])
            .spawn();
        log::info!("Done!");
    }

    def_panic_handler(panic_info);
}
