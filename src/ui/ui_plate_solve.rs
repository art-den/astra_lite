use std::{cell::Cell, rc::Rc, sync::Arc};
use gtk::{glib, prelude::*, glib::clone};
use macros::FromBuilder;

use crate::{
    core::{api::{Command, PlateSolverPatch, StartMode}, engine::Engine, events::*},
    hal::{DeviceType, events::HalEvent},
    options::*,
    utils::log_utils::log_if_error,
};

use super::{gtk_utils::*, module::*, ui_main::*, utils::*};


pub fn init_ui(
    window:    &gtk::ApplicationWindow,
    main_ui:   &Rc<MainUi>,
    engine:    &Arc<Engine>,
) -> Rc<dyn UiModule> {
    let widgets = Widgets::from_builder_str(include_str!(r"resources/platesolve.ui"));
    let obj = Rc::new(PlateSolveUi {
        widgets,
        main_ui:         Rc::clone(main_ui),
        window:          window.clone(),
        engine:          Arc::clone(engine),
        delayed_actions: DelayedActions::new(200),
        opts_syncing:    Cell::new(false),
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
    engine:          Arc<Engine>,
    delayed_actions: DelayedActions<DelayedAction>,
    opts_syncing:    Cell<bool>,
}

impl Drop for PlateSolveUi {
    fn drop(&mut self) {
        log::info!("PlateSolveUi dropped");
    }
}

impl UiModule for PlateSolveUi {
    fn show_options(&self, options: &Options) {
        self.sync_suppress(|| {
            self.widgets.cbx_gain.set_active_id(Some(options.plate_solver.gain.to_active_id()));
            self.widgets.cbx_bin.set_active_id(options.plate_solver.bin.to_active_id());
            self.widgets.cbx_solver.set_active_id(options.plate_solver.solver.to_active_id());
            self.widgets.spb_timeout.set_value(options.plate_solver.timeout as f64);
            self.widgets.spb_blind_timeout.set_value(options.plate_solver.blind_timeout as f64);
            set_spb_value(&self.widgets.spb_exp, options.plate_solver.exposure);
        });
    }

    fn panels(&self) -> Vec<Panel> {
        vec![Panel {
            str_id: "plate_solving",
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

    fn on_event(&self, event: &Event) {
        match event {
            Event::ModeChanged => {
                self.delayed_actions.schedule(DelayedAction::CorrectWidgetsProps);
            }
            Event::CameraDeviceChanged(_) => {
                self.show_options(&self.main_ui.snapshot().options);
                self.correct_widgets_props();
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
    fn sync_suppress(&self, sync: impl FnOnce()) {
        let prev = self.opts_syncing.replace(true);
        sync();
        self.opts_syncing.set(prev);
    }

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

        self.widgets.spb_exp.connect_value_changed(
            clone!(@weak self as self_ => move |sb| {
                if self_.opts_syncing.get() { return; }
                log_if_error(
                    &self_.engine.exec_command(
                        Command::SetPlateSolverOptions(PlateSolverPatch {
                            exposure: Some(sb.value()),
                            ..Default::default()
                        })
                    ),
                    "Set plate solver options"
                );
            })
        );

        self.widgets.cbx_gain.connect_active_id_notify(
            clone!(@weak self as self_ => move |cb| {
                if self_.opts_syncing.get() { return; }
                log_if_error(
                    &self_.engine.exec_command(
                        Command::SetPlateSolverOptions(PlateSolverPatch {
                            gain: Some(Gain::from_active_id(cb.active_id().as_deref())),
                            ..Default::default()
                        })
                    ),
                    "Set plate solver options"
                );
            })
        );

        self.widgets.cbx_bin.connect_active_id_notify(
            clone!(@weak self as self_ => move |cb| {
                if self_.opts_syncing.get() { return; }
                log_if_error(
                    &self_.engine.exec_command(
                        Command::SetPlateSolverOptions(PlateSolverPatch {
                            bin: Some(Binning::from_active_id(cb.active_id().as_deref())),
                            ..Default::default()
                        })
                    ),
                    "Set plate solver options"
                );
            })
        );

        self.widgets.cbx_solver.connect_active_id_notify(
            clone!(@weak self as self_ => move |cb| {
                if self_.opts_syncing.get() { return; }
                log_if_error(
                    &self_.engine.exec_command(
                        Command::SetPlateSolverOptions(PlateSolverPatch {
                            solver: Some(PlateSolverType::from_active_id(cb.active_id().as_deref())),
                            ..Default::default()
                        })
                    ),
                    "Set plate solver options"
                );
            })
        );

        self.widgets.spb_timeout.connect_value_changed(
            clone!(@weak self as self_ => move |sb| {
                if self_.opts_syncing.get() { return; }
                log_if_error(
                    &self_.engine.exec_command(
                        Command::SetPlateSolverOptions(PlateSolverPatch {
                            timeout: Some(sb.value() as _),
                            ..Default::default()
                        })
                    ),
                    "Set plate solver options"
                );
            })
        );

        self.widgets.spb_blind_timeout.connect_value_changed(
            clone!(@weak self as self_ => move |sb| {
                if self_.opts_syncing.get() { return; }
                log_if_error(
                    &self_.engine.exec_command(
                        Command::SetPlateSolverOptions(PlateSolverPatch {
                            blind_timeout: Some(sb.value() as _),
                            ..Default::default()
                        })
                    ),
                    "Set plate solver options"
                );
            })
        );
    }

    fn connect_delayed_actions_events(self: &Rc<Self>) {
        self.delayed_actions.set_event_handler(
            clone!(@weak self as self_ => move |action| {
                self_.handler_delayed_action(action);
            })
        );
    }

    fn correct_widgets_props(&self) {
        let snap = self.main_ui.snapshot();

        let cam_active = snap.devices.camera.as_ref().is_some_and(|c| c.is_active);
        let mnt_active = snap.devices.mount.as_ref().is_some_and(|m| m.is_active);

        let plate_solve_sensitive =
            mnt_active && cam_active && snap.permissions.can_change_mode;

        // Exception (follow-up `Query::CameraCaps` cache): live camera
        // capability read for the platesolve exposure spinbutton
        if let Some(camera) = self.engine.cur_devices.camera() {
            let exp_range = camera.exposure_range().ok();
            correct_spinbutton_by_range(&self.widgets.spb_exp, exp_range, 1, Some(1.0));
        }

        self.widgets.grd.set_sensitive(plate_solve_sensitive);
    }

    fn handler_delayed_action(&self, action: &DelayedAction) {
        match action {
            DelayedAction::CorrectWidgetsProps => {
                self.correct_widgets_props();
            }
        }
    }

    fn handler_action_capture_platesolve(self: &Rc<Self>) {
        exec_and_show_error(Some(&self.window), || {
            self.engine.exec_command(Command::Start(StartMode::CaptureAndPlatesolve))?;
            Ok(())
        });
    }

    fn handler_action_plate_solve_and_goto(&self) {
        exec_and_show_error(Some(&self.window), || {
            self.engine.exec_command(Command::Start(StartMode::GotoImage))?;
            Ok(())
        });
    }
}
