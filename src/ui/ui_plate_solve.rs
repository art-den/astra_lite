use std::{rc::Rc, sync::{Arc, RwLock}};
use gtk::{glib, prelude::*, glib::clone};
use macros::FromBuilder;

use crate::{
    core::{core::{Core, ModeType}, events::*},
    indi,
    options::*,
};

use super::{gtk_utils::*, module::*, ui_main::*, utils::*};


pub fn init_ui(
    window:  &gtk::ApplicationWindow,
    main_ui: &Rc<MainUi>,
    options: &Arc<RwLock<Options>>,
    core:    &Arc<Core>,
    indi:    &Arc<indi::Connection>,
) -> Rc<dyn UiModule> {
    let widgets = Widgets::from_builder_str(include_str!(r"resources/platesolve.ui"));
    let obj = Rc::new(PlateSolveUi {
        widgets,
        window:          window.clone(),
        main_ui:         Rc::clone(main_ui),
        options:         Arc::clone(options),
        core:            Arc::clone(core),
        indi:            Arc::clone(indi),
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
    options:         Arc<RwLock<Options>>,
    core:            Arc<Core>,
    indi:            Arc<indi::Connection>,
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
        let mut options = self.options.write().unwrap();
        if let Some(cur_cam_device) = options.cam.device.clone() {
            self.store_options_for_camera(&cur_cam_device, &mut options);
        }
        drop(options);
    }

    fn on_core_event(&self, event: &Event) {
        match event {
            Event::ModeChanged => {
                self.delayed_actions.schedule(DelayedAction::CorrectWidgetsProps);
            }
            Event::CameraDeviceChanged{ from, to } => {
                self.handler_camera_changed(from, to);
            }
            Event::MountDeviceSelected(mount_device) => {
                let options = self.options.read().unwrap();
                let cam_device = options.cam.device.clone();
                drop(options);
                self.correct_widgets_props_impl(mount_device, cam_device.as_ref());
            }
            _ => {}
        }

    }

    fn on_indi_event(&self, event: &indi::Event) {
        match event {
            indi::Event::ConnChange(_)|
            indi::Event::DeviceConnected(_)|
            indi::Event::DeviceDelete(_)|
            indi::Event::NewDevice(_) => {
                self.delayed_actions.schedule(DelayedAction::CorrectWidgetsProps);
            }
            _ => {}
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

    fn correct_widgets_props_impl(&self, mount_device: &str, cam_device: Option<&DeviceAndProp>) {
        let mnt_active = self.indi.is_device_enabled(mount_device).unwrap_or(false);
        let cam_active = cam_device.as_ref()
            .map(|cam_device| self.indi.is_device_enabled(&cam_device.name).unwrap_or(false))
            .unwrap_or(false);
        let indi_connected = self.indi.state() == indi::ConnState::Connected;

        let mode_data = self.core.mode_data();
        let mode_type = mode_data.mode.get_type();
        let waiting = mode_type == ModeType::Waiting;
        let live_view = mode_type == ModeType::LiveView;
        let single_shot = mode_type == ModeType::SingleShot;

        let plate_solve_sensitive =
            indi_connected &&
            mnt_active && cam_active &&
            (waiting || single_shot || live_view);

        if let Some(cam_device) = cam_device {
            let cam_ccd = indi::CamCcd::from_ccd_prop_name(&cam_device.prop);
            let exp_value = self.indi.camera_get_exposure_prop_value(&cam_device.name, cam_ccd);
            correct_spinbutton_by_cam_prop(&self.widgets.spb_exp, &exp_value, 1, Some(1.0));
        }

        self.widgets.grd.set_sensitive(plate_solve_sensitive);
    }

    fn correct_widgets_props(&self) {
        let options = self.options.read().unwrap();
        let mount_device = options.mount.device.clone();
        let cam_device = options.cam.device.clone();
        drop(options);
        self.correct_widgets_props_impl(&mount_device, cam_device.as_ref());
    }

    fn handler_camera_changed(&self, from: &Option<DeviceAndProp>, to: &DeviceAndProp) {
        let mut options = self.options.write().unwrap();
        self.get_options(&mut options);
        if let Some(from) = from {
            self.store_options_for_camera(from, &mut options);
        }
        self.restore_options_for_camera(to, &mut options);
        self.show_options(&options);
        let mount_device = options.mount.device.clone();
        drop(options);
        self.correct_widgets_props_impl(&mount_device, Some(to));
    }

    fn store_options_for_camera(
        &self,
        device:  &DeviceAndProp,
        options: &mut Options
    ) {
        let key = device.to_file_name_part();
        let sep_options = options.sep_ps.entry(key).or_default();
        sep_options.exposure = options.plate_solver.exposure;
        sep_options.gain = options.plate_solver.gain;
        sep_options.bin = options.plate_solver.bin;
    }

    fn restore_options_for_camera(
        &self,
        device:  &DeviceAndProp,
        options: &mut Options
    ) {
        let key = device.to_file_name_part();
        if let Some(sep_options) = options.sep_ps.get(&key) {
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