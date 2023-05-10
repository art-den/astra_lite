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
mod fits_reader;
mod state;
mod math;
mod plots;

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

    let application = gtk::Application::new(
        Some(&format!("com.github.art-den.{}", env!("CARGO_PKG_NAME"))),
        Default::default(),
    );
    application.connect_activate(move |app| gui_main::build_ui(app, &logs_dir));
    application.run();
    Ok(())
}

