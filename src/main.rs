#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
#![allow(dead_code)]
#![allow(
    clippy::too_many_arguments,
    clippy::upper_case_acronyms,
    clippy::uninlined_format_args,
    clippy::wrong_self_convention
)]

mod gui;
mod utils;
mod image;
mod indi;
mod guiding;
mod core;
//mod sky_map;
mod options;

use std::{path::Path, sync::{Arc, RwLock}};
use gtk::{prelude::*, glib, glib::clone};
use crate::{
    utils::io_utils::*,
    utils::log_utils::*,
    options::*,
    core::state::State,
    core::frame_processing::*
};

fn panic_handler(
    panic_info:        &std::panic::PanicInfo,
    logs_dir:          &Path,
    def_panic_handler: &Box<dyn Fn(&std::panic::PanicInfo<'_>) + 'static + Sync + Send>,
) {
    let payload_str =
        if let Some(msg) = panic_info.payload().downcast_ref::<&'static str>() {
            Some(*msg)
        } else if let Some(msg) = panic_info.payload().downcast_ref::<String>() {
            Some(msg.as_str())
        } else {
            None
        };

    log::error!("(╯°□°）╯︵ ┻━┻ PANIC OCCURRED");

    if let Some(payload) = &payload_str {
        log::error!("Panic paiload: {}", payload);
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

    if cfg!(target_os = "linux") {
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

    std::panic::set_hook({
        let logs_dir = logs_dir.clone();
        let default_panic_handler = std::panic::take_hook();
        Box::new(move |panic_info| {
            panic_handler(panic_info, &logs_dir, &default_panic_handler)
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

    let indi = Arc::new(indi::indi_api::Connection::new());
    let options = Arc::new(RwLock::new(Options::default()));
    let (img_cmds_sender, frame_process_thread) = start_frame_processing_thread();
    let state = Arc::new(State::new(&indi, &options, img_cmds_sender));

    let application = gtk::Application::new(
        Some(&format!("com.github.art-den.{}", env!("CARGO_PKG_NAME"))),
        Default::default(),
    );

    application.connect_activate(clone!(@weak options => move |app|
        gui::gui_main::build_ui(app, &indi, &options, &state, &logs_dir)
    ));
    application.run();

    log::info!("Exited from application.run");

    let opts = options.read().unwrap();
    _ = save_json_to_config::<Options>(&opts, "options");
    drop(opts);
    log::info!("Options saved");

    _ = frame_process_thread.join();
    log::info!("Process thread joined");

    Ok(())
}
