#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
#![allow(dead_code)]
#![allow(
    clippy::too_many_arguments,
    clippy::upper_case_acronyms,
    clippy::uninlined_format_args,
    clippy::wrong_self_convention
)]

mod options;
mod image_raw;
mod image;
mod image_info;
mod stars_offset;
mod indi_api;
mod gtk_utils;
mod gui_main;
mod gui_hardware;
mod gui_indi;
mod gui_camera;
mod io_utils;
mod image_processing;
mod log_utils;
mod simple_fits;
mod state;
mod math;
mod plots;
mod sexagesimal;

use std::path::Path;
use gtk::prelude::*;
use crate::io_utils::*;

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

    _ = msgbox::create(&message_caption, &message_text, msgbox::IconType::Error);

    def_panic_handler(panic_info);
}


fn main() -> anyhow::Result<()> {
    let mut logs_dir = get_app_dir()?;
    logs_dir.push("logs");
    log_utils::cleanup_old_logs(&logs_dir, 14/*days*/);
    log_utils::start_logger(&logs_dir)?;
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

    let logs_dir_for_panic = logs_dir.clone();
    let default_panic_handler = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic_info|
        panic_handler(panic_info, &logs_dir_for_panic, &default_panic_handler)
    ));

    let application = gtk::Application::new(
        Some(&format!("com.github.art-den.{}", env!("CARGO_PKG_NAME"))),
        Default::default(),
    );
    application.connect_activate(move |app| gui_main::build_ui(app, &logs_dir));
    application.run();
    Ok(())
}

