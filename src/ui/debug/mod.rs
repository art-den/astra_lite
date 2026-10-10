#![allow(dead_code)]

pub mod utils;
use crate::core::engine::{Engine, ModeKind};
use crate::hal::HalImpl;
use gtk::traits::{AdjustmentExt, ComboBoxExt, ScrolledWindowExt, SpinButtonExt, ToggleButtonExt, WidgetExt};
pub use utils::*;

// Note: widget_by_name use widget name (not gtk-builder id)!
// Don't forget to set widget name in *.ui-file before using of widget_by_name!

fn debug_preview(app: &gtk::Application) {
    let cb_scale = widget_by_name::<gtk::ComboBoxText>(app, "cb_scale");
    let spb_cam_exp = widget_by_name::<gtk::SpinButton>(app, "spb_cam_exp");

    // Connect to INDI
    test_widget_click("btn_conn_indi", app);
    test_pause_ms(3000); // we need to wait long enough for all devices to initialize

    // Switch to tab "Common"
    test_widget_click("lb_tab_common", app);
    test_pause_ms(1000);

    // scale = 100%
    cb_scale.set_active_id(Some("orig"));
    test_pause_ms(200);

    // Take two shots with 2s duration; the 2nd frame's star overlay must be
    // drawn at the 2nd frame's star positions, not the 1st frame's
    spb_cam_exp.set_value(2.0);
    test_widget_click("btn_take_shot", app);
    test_pause_ms(10000);
    test_widget_click("btn_take_shot", app);
    test_pause_ms(10000);

    // Show the stars overlay
    let chb_stars = widget_by_name::<gtk::CheckButton>(app, "chb_stars");
    chb_stars.set_active(true);
    test_pause_ms(300);

    // Save screenshot with scale = 100%
    test_save_main_window_screenshot(app, ".tmp/scale_100.png");
    test_pause_ms(1000);

    // scale = 200%
    cb_scale.set_active_id(Some("p200"));
    test_pause_ms(200);
    test_save_main_window_screenshot(app, ".tmp/scale_200.png");

    test_pause_ms(1000);
}

fn debug_preview_scroll(app: &gtk::Application) {
    let cb_scale = widget_by_name::<gtk::ComboBoxText>(app, "cb_scale");
    let sw_img = widget_by_name::<gtk::ScrolledWindow>(app, "sw_img");
    let vadj = sw_img.vadjustment();

    // Scroll down at 200% (vadj can reach ~1450 there)
    cb_scale.set_active_id(Some("p200"));
    test_pause_ms(300);
    for _ in 0..20 {
        vadj.set_value(vadj.value() + 50.0);
        test_pause_ms(15);
    }
    test_pause_ms(300);

    // Switch to 100%: vadj (~1345) is out of range there (max ~418);
    // check that the scroll position is clamped and the image fills the viewport
    cb_scale.set_active_id(Some("orig"));
    test_pause_ms(300);
    log::info!("DBG vadj at 100% after 200%: {} (upper {}, page {})", vadj.value(), vadj.upper(), vadj.page_size());
    test_save_main_window_screenshot(app, ".tmp/scroll_clamp.png");
    test_pause_ms(300);

    // Switch to P50: the image becomes smaller than the viewport;
    // check that the uncovered area shows the background, not stale pixels
    cb_scale.set_active_id(Some("p50"));
    test_pause_ms(300);
    test_save_main_window_screenshot(app, ".tmp/stale_check.png");
    test_pause_ms(300);

    // The user's case: scroll down at 100% in small steps (like mouse wheel)
    cb_scale.set_active_id(Some("orig"));
    test_pause_ms(300);
    test_save_main_window_screenshot(app, ".tmp/scroll_100_before.png");
    test_pause_ms(200);
    for i in 0..30 {
        vadj.set_value(vadj.value() + 10.0);
        test_pause_ms(20);
        if i == 14 {
            test_save_main_window_screenshot(app, ".tmp/scroll_100_mid.png");
        }
    }
    test_pause_ms(300);
    test_save_main_window_screenshot(app, ".tmp/scroll_100_end.png");
    test_pause_ms(200);

    // Scroll to the very bottom: image rows ~434..1024 become visible
    vadj.set_value(vadj.upper() - vadj.page_size());
    test_pause_ms(400);
    test_save_main_window_screenshot(app, ".tmp/scroll_100_bottom.png");
    test_pause_ms(500);
}

/// Dumps the ASCOM Classic camera state and the widgets that depend on it.
/// Used to find out why an action (e.g. "Take shot") stays insensitive.
fn debug_ascom_camera(app: &gtk::Application, engine: &Engine) {
    // Autoconnect of saved drivers runs shortly after the UI is built
    test_pause_ms(4000);

    let cam_id = engine.options.read().unwrap().cam.device_id.clone();
    log::info!("DBG options cam device_id = {cam_id:?}");

    let btn_take_shot = widget_by_name::<gtk::Button>(app, "btn_take_shot");
    log::info!("DBG btn_take_shot sensitive = {}", btn_take_shot.is_sensitive());

    #[cfg(windows)]
    log::info!("DBG ASCOM impl state = {:?}", engine.hal.ascom_impl().state());
    log::info!("DBG mode kind = {:?}", engine.modes().active.kind());

    match engine.cur_devices.camera() {
        Some(camera) => {
            log::info!("DBG camera id = {}", camera.id());
            log::info!("DBG camera is_active() = {:?}", camera.is_active());
            log::info!("DBG exposure_range() = {:?}", camera.exposure_range());
            log::info!("DBG is_frame_supported() = {:?}", camera.is_frame_supported());
            log::info!("DBG is_gain_supported() = {:?}", camera.is_gain_supported());
            log::info!("DBG is_cooler_supported() = {:?}", camera.is_cooler_supported());
            log::info!("DBG ccd_size() = {:?}", camera.ccd_size());
            log::info!("DBG pixel_size_um() = {:?}", camera.pixel_size_um());
            log::info!("DBG temperature() = {:?}", camera.temperature());
        }
        None => log::info!("DBG camera in Core is None (not activated)")
    }

    test_switch_tab(app, "lb_tab_common");
    test_save_main_window_screenshot(app, ".tmp/ascom_cam_state.png");

    // Reproduce the user's flow: a single shot over the activated driver
    // (a real click cannot be simulated on Windows, gdk_test_simulate_button() is X11-only)
    if btn_take_shot.is_sensitive() {
        engine.options.write().unwrap().cam.frame.set_exposure(1.0);
        match engine.start_single_shot() {
            Ok(_) => log::info!("DBG single shot started"),
            Err(err) => log::error!("DBG start_single_shot failed: {err}"),
        }

        for i in 0..20 {
            test_pause_ms(1000);
            let mode = engine.modes().active.kind();
            let empty = engine.preview.image.read().unwrap().is_empty();
            log::info!("DBG after take shot: t={i}s mode={mode:?} image_empty={empty}");
            if mode == ModeKind::Waiting && !empty {
                break;
            }
        }
        test_switch_tab(app, "lb_tab_common");
        test_save_main_window_screenshot(app, ".tmp/ascom_after_shot.png");
    }
    test_pause_ms(500);
}

/// Reproduces the user's bug: shot on OmniSim, then switch to V3 Simulator,
/// then take shot -> "SetBinning: Invoke(MaxBinX): NotConnected".
fn debug_ascom_switch(app: &gtk::Application, engine: &Engine) {
    // Let startup autoconnect finish
    test_pause_ms(4000);

    let log_cam = |tag: &str| {
        match engine.cur_devices.camera() {
            Some(cam) => log::info!(
                "DBG [{tag}] cam id = {}, is_active = {:?}, exp_range = {:?}",
                cam.id(), cam.is_active().is_ok(), cam.exposure_range()
            ),
            None => log::info!("DBG [{tag}] camera is None"),
        }
    };

    log_cam("after autoconnect");

    // Step 1: switch to OmniSim and activate it
    engine.cur_devices.apply_camera("ASCOM.OmniSim.Camera");
    test_pause_ms(1000);
    log_cam("after apply OmniSim");

    // Step 2: switch to V3 Simulator (deactivates OmniSim)
    engine.cur_devices.apply_camera("ASCOM.Simulator.Camera");
    test_pause_ms(1000);
    log_cam("after apply V3 Simulator");

    // Step 3: the exact call that failed for the user
    if let Some(cam) = engine.cur_devices.camera() {
        log::info!("DBG set_binning(1,1) = {:?}", cam.set_binning(1, 1));
        log::info!("DBG max_binning() = {:?}", cam.max_binning());
        log::info!("DBG ccd_size() = {:?}", cam.ccd_size());
        log::info!("DBG set_binning(1,1) again = {:?}", cam.set_binning(1, 1));

        // Step 4: full single shot like the user's Take Shot click
        engine.options.write().unwrap().cam.frame.set_exposure(1.0);
        test_switch_tab(app, "lb_tab_common");
        match engine.start_single_shot() {
            Ok(_) => log::info!("DBG single shot started"),
            Err(err) => log::error!("DBG start_single_shot failed: {err}"),
        }

        for i in 0..15 {
            test_pause_ms(1000);
            let mode = engine.modes().active.kind();
            let empty = engine.preview.image.read().unwrap().is_empty();
            log::info!("DBG shot t={i}s mode={mode:?} image_empty={empty}");
            if mode == ModeKind::Waiting && !empty {
                break;
            }
        }
        test_switch_tab(app, "lb_tab_common");
        test_save_main_window_screenshot(app, ".tmp/ascom_switch_shot.png");
    }
    test_pause_ms(500);
}

pub fn run_scenario(app: &gtk::Application, engine: &Engine) {
    debug_ascom_bin2(app, engine);
}

/// Reproduces the user's Bin=2 bug on the currently selected ASCOM camera:
/// applies a full unbinned frame at bin 2 directly, then runs the whole
/// single-shot pipeline like the Take Shot click.
fn debug_ascom_bin2(app: &gtk::Application, engine: &Engine) {
    use crate::options::Binning;

    // Let startup autoconnect finish
    test_pause_ms(4000);

    let Some(cam) = engine.cur_devices.camera() else {
        log::error!("DBG bin2: camera is None (not activated)");
        return;
    };
    let Ok((w, h)) = cam.ccd_size() else {
        log::error!("DBG bin2: ccd_size failed");
        return;
    };
    log::info!("DBG bin2: cam = {}, ccd_size = {w}x{h}, max_binning = {:?}",
        cam.id(), cam.max_binning());

    // Direct check of the fixed unbinned->binned sub-frame conversion
    log::info!("DBG bin2: set_binning(2,2) = {:?}", cam.set_binning(2, 2));
    log::info!("DBG bin2: set_frame(0,0,{w},{h}) = {:?}", cam.set_frame(0, 0, w, h));

    // Full take_shot pipeline like the user's Take Shot click
    {
        let mut options = engine.options.write().unwrap();
        options.cam.frame.binning = Binning::Bin2;
        options.cam.frame.set_exposure(1.0);
    }
    test_switch_tab(app, "lb_tab_common");
    match engine.start_single_shot() {
        Ok(_) => log::info!("DBG bin2: single shot started"),
        Err(err) => log::error!("DBG bin2: start_single_shot failed: {err}"),
    }

    for i in 0..20 {
        test_pause_ms(1000);
        let mode = engine.modes().active.kind();
        let (iw, ih) = {
            let img = engine.preview.image.read().unwrap();
            (img.width(), img.height())
        };
        if mode == ModeKind::Waiting && iw > 0 {
            log::info!("DBG bin2: shot done t={i}s image={iw}x{ih} (expect {}/2 x {}/2 at bin2)", w, h);
            break;
        }
        log::info!("DBG bin2: shot t={i}s mode={mode:?} image={iw}x{ih}");
    }
    test_save_main_window_screenshot(app, ".tmp/ascom_bin2_shot.png");
    test_pause_ms(500);
}
