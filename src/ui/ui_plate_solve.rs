use std::{rc::Rc, sync::Arc};
use gtk::{glib, prelude::*, glib::clone};
use macros::FromBuilder;

use crate::{
    core::{core::{Core, ModeType}, events::*},
    hal::{DeviceType, HalState, events::HalEvent},
    options::*,
};

use super::{gtk_utils::*, module::*, ui_main::*, utils::*};


pub fn init_ui(
    window:  &gtk::ApplicationWindow,
    main_ui: &Rc<MainUi>,
    core:    &Arc<Core>,
) -> Rc<dyn UiModule> {
    let widgets = Widgets::from_builder_str(include_str!(r"resources/platesolve.ui"));
    let obj = Rc::new(PlateSolveUi {
        widgets,
        window:          window.clone(),
        main_ui:         Rc::clone(main_ui),
        core:            Arc::clone(core),
        delayed_actions: DelayedActions::new(200),
    });

    obj.init_widgets();
    obj.connect_widgets_events();
    obj.connect_delayed_actions_events();

    obj
}

#[derive(Hash, Eq, PartialEq)]
enum DelayedAction {
    CorrectWidgetsProps,
}

impl PlateSolverType {
    pub fn from_active_id(active_id: Option<&str>) -> Self {
        match active_id {
            Some("astrometry.net") => Self::Astrometry,
            _                  => Self::Astrometry,
        }
    }

    pub fn to_active_id(&self) -> Option<&'static str> {
        match self {
            Self::Astrometry => Some("astrometry.net"),
        }
    }
}

#[derive(FromBuilder)]
struct Widgets {
    grd:               gtk::Grid,
    spb_exp:           gtk::SpinButton,
    cbx_gain:          gtk::ComboBoxText,
    cbx_bin:           gtk::ComboBoxText,
    cbx_solver:        gtk::ComboBoxText,
    spb_timeout:       gtk::SpinButton,
    spb_blind_timeout: gtk::SpinButton,
}

struct PlateSolveUi {
    widgets:         Widgets,
    main_ui:         Rc<MainUi>,
    window:          gtk::ApplicationWindow,
    core:            Arc<Core>,
    delayed_actions: DelayedActions<DelayedAction>,
}

impl Drop for PlateSolveUi {
    fn drop(&mut self) {
        log::info!("PlateSolveUi dropped");
    }
}

impl UiModule for PlateSolveUi {
    fn show_options(&self, options: &Options) {
        self.widgets.cbx_gain.set_active_id(Some(options.plate_solver.gain.to_active_id()));
        self.widgets.cbx_bin.set_active_id(options.plate_solver.bin.to_active_id());
        self.widgets.cbx_solver.set_active_id(options.plate_solver.solver.to_active_id());
        self.widgets.spb_timeout.set_value(options.plate_solver.timeout as f64);
        self.widgets.spb_blind_timeout.set_value(options.plate_solver.blind_timeout as f64);
        set_spb_value(&self.widgets.spb_exp, options.plate_solver.exposure);
    }

    fn get_options(&self, options: &mut Options) {
        options.plate_solver.exposure      = self.widgets.spb_exp.value();
        options.plate_solver.gain          = Gain::from_active_id(self.widgets.cbx_gain.active_id().as_deref());
        options.plate_solver.bin           = Binning::from_active_id(self.widgets.cbx_bin.active_id().as_deref());
        options.plate_solver.solver        = PlateSolverType::from_active_id(self.widgets.cbx_solver.active_id().as_deref());
        options.plate_solver.timeout       = self.widgets.spb_timeout.value() as _;
        options.plate_solver.blind_timeout = self.widgets.spb_blind_timeout.value() as _;
    }

    fn panels(&self) -> Vec<Panel> {
        vec![Panel {
            str_id: "platesolving",
            name:   "Plate solving".to_string(),
            widget: self.widgets.grd.clone().upcast(),
            pos:    PanelPosition::Right,
            tab:    TabPage::Main,
            flags:  PanelFlags::empty(),
        }]
    }

    fn on_show_options_first_time(&self) {
        self.correct_widgets_props();
    }

    fn on_app_closing(&self) {
        let mut options = self.core.options().write().unwrap();
        let cur_cam_device = options.cam.device_id.to_string();
        self.store_options_for_camera(&cur_cam_device, &mut options);
        drop(options);
    }

    fn on_event(&self, event: &Event) {
        match event {
            Event::ModeChanged => {
                self.delayed_actions.schedule(DelayedAction::CorrectWidgetsProps);
            }
            Event::CameraDeviceChanged{ prev_camera_id, new_camera_id } => {
                self.handler_camera_changed(prev_camera_id, new_camera_id);
            }
            Event::MountDeviceChanged(_) => {
                self.correct_widgets_props();
            }
            _ => {}
        }

    }

    fn on_hal_event(&self, event: &HalEvent) {
        match event {
            HalEvent::DeviceConnected(info)|
            HalEvent::DeviceDisconnected(info) => {
                if info.type_.contains(DeviceType::CAMERA) || info.type_.contains(DeviceType::TELESCOPE) {
                    self.delayed_actions.schedule(DelayedAction::CorrectWidgetsProps);
                }
            }
            _ => {},
        }
    }
}

impl PlateSolveUi {
    fn init_widgets(&self) {
        self.widgets.spb_exp.set_range(0.5, 30.0);
        self.widgets.spb_exp.set_digits(1);
        self.widgets.spb_exp.set_increments(0.5, 5.0);

        self.widgets.spb_timeout.set_range(5.0, 120.0);
        self.widgets.spb_timeout.set_digits(0);
        self.widgets.spb_timeout.set_increments(5.0, 20.0);

        self.widgets.spb_blind_timeout.set_range(5.0, 120.0);
        self.widgets.spb_blind_timeout.set_digits(0);
        self.widgets.spb_blind_timeout.set_increments(5.0, 20.0);
    }

    fn connect_widgets_events(self: &Rc<Self>) {
        connect_action_rc(&self.window, self, "capture_platesolve",   Self::handler_action_capture_platesolve);
        connect_action   (&self.window, self, "plate_solve_and_goto", Self::handler_action_plate_solve_and_goto);
    }

    fn connect_delayed_actions_events(self: &Rc<Self>) {
        self.delayed_actions.set_event_handler(
            clone!(@weak self as self_ => move |action| {
                self_.handler_delayed_action(action);
            })
        );
    }

    fn correct_widgets_props(&self) {
        let camera = self.core.camera();
        let mount = self.core.telescope();

        let cam_active = camera
            .as_ref()
            .and_then(|cam| cam.is_active().ok())
            .unwrap_or(false);
        let mnt_active = mount
            .and_then(|cam| cam.is_active().ok())
            .unwrap_or(false);

        let hal_connected = self.core.hal().state() == HalState::Connected;

        let mode = self.core.mode();
        let mode_type = mode.active.get_type();
        let waiting = mode_type == ModeType::Waiting;
        let live_view = mode_type == ModeType::LiveView;
        let single_shot = mode_type == ModeType::SingleShot;

        let plate_solve_sensitive =
            hal_connected &&
            mnt_active && cam_active &&
            (waiting || single_shot || live_view);

        if let Some(camera) = camera {
            let exp_range = camera.exposure_range().ok();
            correct_spinbutton_by_range(&self.widgets.spb_exp, exp_range, 1, Some(1.0));
        }

        self.widgets.grd.set_sensitive(plate_solve_sensitive);
    }

    fn handler_camera_changed(&self, from: &str, to: &str) {
        let mut options = self.core.options().write().unwrap();
        self.get_options(&mut options);
        if !from.is_empty() {
            self.store_options_for_camera(from, &mut options);
        }
        self.restore_options_for_camera(to, &mut options);
        self.show_options(&options);
        drop(options);
        self.correct_widgets_props();
    }

    fn store_options_for_camera(
        &self,
        device:  &str,
        options: &mut Options
    ) {
        if device.is_empty() {
            return;
        }
        let sep_options = options.sep_ps.entry(device.to_string()).or_default();
        sep_options.exposure = options.plate_solver.exposure;
        sep_options.gain = options.plate_solver.gain;
        sep_options.bin = options.plate_solver.bin;
    }

    fn restore_options_for_camera(
        &self,
        device:  &str,
        options: &mut Options
    ) {
        if let Some(sep_options) = options.sep_ps.get(device) {
            options.plate_solver.exposure = sep_options.exposure;
            options.plate_solver.gain = sep_options.gain;
            options.plate_solver.bin = sep_options.bin;
        }
    }

    fn handler_delayed_action(&self, action: &DelayedAction) {
        match action {
            DelayedAction::CorrectWidgetsProps => {
                self.correct_widgets_props();
            }
        }
    }

    fn handler_action_capture_platesolve(self: &Rc<Self>) {
        self.main_ui.get_all_options();

        exec_and_show_error(Some(&self.window), || {
            self.core.start_capture_and_platesolve()?;
            Ok(())
        });
    }

    fn handler_action_plate_solve_and_goto(&self) {
        self.main_ui.get_all_options();

        exec_and_show_error(Some(&self.window), || {
            self.core.start_goto_image()?;
            Ok(())
        });
    }
}
