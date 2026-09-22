#![allow(dead_code)]

pub mod utils;
use gtk::traits::{AdjustmentExt, ComboBoxExt, ScrolledWindowExt, SpinButtonExt, ToggleButtonExt};
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

pub fn debug(app: &gtk::Application) {
    debug_preview(app);
}
