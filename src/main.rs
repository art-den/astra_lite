#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
#![allow(dead_code)]
#![allow(
    clippy::too_many_arguments,
    clippy::upper_case_acronyms,
    clippy::uninlined_format_args,
    clippy::wrong_self_convention
)]

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

use gtk::prelude::*;
use crate::io_utils::*;

fn main() -> anyhow::Result<()> {
    let mut logs_dir = get_app_dir()?;
    logs_dir.push("logs");
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

    std::panic::set_hook(Box::new(panic_handler));

    let application = gtk::Application::new(
        Some(&format!("com.github.art-den.{}", env!("CARGO_PKG_NAME"))),
        Default::default(),
    );
    application.connect_activate(gui_main::build_ui);
    application.run();
    Ok(())
}

fn panic_handler(panic_info: &std::panic::PanicInfo) {
    let payload_str =
        if let Some(msg) = panic_info.payload().downcast_ref::<&'static str>() {
            Some(*msg)
        } else if let Some(msg) = panic_info.payload().downcast_ref::<String>() {
            Some(msg.as_str())
        } else {
            None
        };

    log::error!("(╯°□°）╯︵ ┻━┻ PANIC OCCURRED");

    if let Some(payload) = payload_str {
        log::error!("Panic paiload: {}", payload);
    }

    if let Some(loc) = panic_info.location() {
        log::error!("Panic location: {}", loc);
    }
}
