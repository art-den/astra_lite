#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
#![allow(
    clippy::too_many_arguments,
    clippy::upper_case_acronyms,
    clippy::uninlined_format_args,
    clippy::wrong_self_convention
)]

mod ui;
mod utils;
mod image;
mod indi;
mod guiding;
mod plate_solve;
mod core;
mod sky_math;
mod options;

use std::{path::Path, sync::Arc};
use gtk::{prelude::*, glib, glib::clone};
use ui::gtk_utils::exec_and_show_error;
use crate::{
    utils::io_utils::*,
    utils::log_utils::*,
    options::*,
    core::core::Core,
};

fn panic_handler(
    panic_info:        &std::panic::PanicHookInfo,
    stop_indi_servers: bool,
    logs_dir:          &Path,
    def_panic_handler: &Box<dyn Fn(&std::panic::PanicHookInfo<'_>) + 'static + Sync + Send>,
) {
    let payload_str =
        if let Some(msg) = panic_info.payload().downcast_ref::<&'static str>() {
            Some(*msg)
        } else if let Some(msg) = panic_info.payload().downcast_ref::<String>() {
            Some(msg.as_str())
        } else {
            None
        };

    eprintln!("{}", payload_str.unwrap_or_default());
    eprintln!("{}", panic_info.location().map(|loc| loc.to_string()).unwrap_or_default());

    log::error!("(╯°□°）╯︵ ┻━┻ PANIC OCCURRED");

    if let Some(payload) = &payload_str {
        log::error!("Panic paiload: {}", payload);
        eprintln!("PANIC: {}", payload);
    }

    if let Some(loc) = panic_info.location() {
        log::error!("Panic location: {}", loc);
    }

    log::error!(
        "Panic stacktrace: {}",
        std::backtrace::Backtrace::force_capture().to_string()
    );

    let message_caption = format!(
        "{} {} ver {} crashed ;-(",
        env!("CARGO_PKG_NAME"),
        std::env::consts::ARCH,
        env!("CARGO_PKG_VERSION")
    );

    let message_text = format!(
        "{}\n\nat {}\n\n\nLook logs at\n{}",
        payload_str.unwrap_or_default(),
        panic_info.location().map(|loc| loc.to_string()).unwrap_or_default(),
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


fn main() -> anyhow::Result<()> {
    let mut logs_dir = get_app_dir()?;
    logs_dir.push("logs");
    cleanup_old_logs(&logs_dir, 14/*days*/);
    start_logger(&logs_dir)?;
    log::set_max_level(log::LevelFilter::Info);

    #[cfg(debug_assertions)] {
        std::env::set_var("RUST_BACKTRACE", "1");
    }

    log::info!(
        "{} {} ver. {} is started",
        env!("CARGO_PKG_NAME"),
        std::env::consts::ARCH,
        env!("CARGO_PKG_VERSION")
    );

    log::info!("Creating Core...");
    let core = Core::new();

    let indi_for_panic = Arc::clone(core.indi());
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

    log::info!("Creating gtk::Application...");
    let application = gtk::Application::new(
        Some(&format!("com.github.art-den.{}", env!("CARGO_PKG_NAME"))),
        Default::default(),
    );

    application.connect_activate(clone!(@weak core => move |app|
        exec_and_show_error(None::<&gtk::Window>, || {
            log::info!("Loading options...");
            let mut options = core.options().write().unwrap();
            load_json_from_config_file::<Options>(&mut options, "options")?;

            log::info!("Check options...");
            options.check()?;

            Ok(())
        });

        ui::ui_main::init_ui(app, &core, &logs_dir)
    ));

    log::info!("Running application.run...");
    application.run();

    log::info!("Exited from application.run");

    log::info!("Saving options...");
    let options = core.options().read().unwrap();
    _ = save_json_to_config::<Options>(&options, "options");
    drop(options);
    log::info!("Options saved");

    core.stop();
    log::info!("Core stopped");

    Ok(())
}
