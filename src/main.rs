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
mod gui_map;
mod gui_guiding;
mod gui_common;

use std::{path::Path, sync::{Arc, RwLock}};
use gtk::{prelude::*, glib, glib::clone};
use crate::{io_utils::*, options::Options, state::State};

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

    let indi = Arc::new(indi_api::Connection::new());
    let options = Arc::new(RwLock::new(Options::default()));
    let state = Arc::new(State::new(&indi, &options));

    std::panic::set_hook({
        let logs_dir = logs_dir.clone();
        let default_panic_handler = std::panic::take_hook();
        let indi = Arc::clone(&indi);
        Box::new(move |panic_info| {
            log::info!("Disconnecting (and stop) INDI server after panic...");
            _ = indi.disconnect_and_wait();
            log::info!("Done!");

            panic_handler(panic_info, &logs_dir, &default_panic_handler)
        })
    });

    let application = gtk::Application::new(
        Some(&format!("com.github.art-den.{}", env!("CARGO_PKG_NAME"))),
        Default::default(),
    );

    application.connect_activate(clone!(@weak options => move |app|
        gui_main::build_ui(app, &indi, &options, &state, &logs_dir)
    ));
    application.run();

    log::info!("Exited from application.run");

    let opts = options.read().unwrap();
    _ = save_json_to_config::<Options>(&opts, "options");
    drop(opts);
    log::info!("Options saved");

    Ok(())
}
