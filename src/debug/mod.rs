pub mod utils;
use gtk::traits::{ComboBoxExt, SpinButtonExt};
pub use utils::*;

fn debug_scale_change(app: &gtk::Application) {
    // Connect to INDI
    test_widget_click("btn_conn_indi", app);
    test_pause(3000); // we need to wait long enough for all devices to initialize

    // Switch to tab "Common"
    test_widget_click("lb_tab_common", app);
    test_pause(1000);

    // scale = Fit window
    let cb_scale = widget_by_name::<gtk::ComboBoxText>(app, "cb_scale");
    cb_scale.set_active_id(Some("fit"));

    // Take shot with 2s duration
    let spb_cam_exp = widget_by_name::<gtk::SpinButton>(app, "spb_cam_exp");
    spb_cam_exp.set_value(2.0);
    test_widget_click("btn_take_shot", app);
    test_pause(3000);

    // Save screenshot with scale = Fit window
    test_save_main_window_screenshot(app, ".tmp/scale_fit_window.png");
    test_pause(100);

    // scale = 100%
    cb_scale.set_active_id(Some("orig"));
    test_pause(200);

    // Save screenshot with scale = 100%
    test_save_main_window_screenshot(app, ".tmp/scale_100.png");
    test_pause(1000);
}

pub fn debug(app: &gtk::Application) {
    debug_scale_change(app);
}
