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
mod options;

use std::{path::Path, sync::{Arc, RwLock}};
use gtk::{prelude::*, glib, glib::clone};
use crate::{
    utils::io_utils::*,
    utils::log_utils::*,
    options::*,
    core::core::Core,
    core::frame_processing::*
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

    log::info!("Creating indi::Connection...");
    let indi = Arc::new(indi::Connection::new());

    std::panic::set_hook({
        let logs_dir = logs_dir.clone();
        let indi = Arc::clone(&indi);
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

    #[cfg(debug_assertions)] {
        std::env::set_var("RUST_BACKTRACE", "1");
    }

    log::info!(
        "{} {} ver. {} is started",
        env!("CARGO_PKG_NAME"),
        std::env::consts::ARCH,
        env!("CARGO_PKG_VERSION")
    );

    log::info!("Creating Options...");
    let options = Arc::new(RwLock::new(Options::default()));

    log::info!("Staring frame processing thread");
    let (img_cmds_sender, frame_process_thread) = start_frame_processing_thread();

    log::info!("Creating Core...");
    let core = Core::new(&indi, &options, img_cmds_sender);

    log::info!("Creating gtk::Application...");
    let application = gtk::Application::new(
        Some(&format!("com.github.art-den.{}", env!("CARGO_PKG_NAME"))),
        Default::default(),
    );

    application.connect_activate(clone!(@weak options, @weak core => move |app|
        ui::ui_main::init_ui(app, &indi, &options, &core, &logs_dir)
    ));

    log::info!("Running application.run...");
    application.run();

    log::info!("Exited from application.run");

    log::info!("Saving options...");
    let opts = options.read().unwrap();
    _ = save_json_to_config::<Options>(&opts, "options");
    drop(opts);
    log::info!("Options saved");

    _ = frame_process_thread.join();
    log::info!("Process thread joined");

    core.stop();
    log::info!("Core stopped");

    Ok(())
}
