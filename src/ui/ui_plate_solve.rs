use std::{rc::Rc, sync::Arc};
use gtk::{glib, prelude::*, glib::clone};
use macros::FromBuilder;

use crate::{
    core::{commands::{Command, ModeStart}, engine::Engine, events::*},
    hal::{DeviceType, events::HalEvent},
    options::*,
};

use super::{gtk_utils::*, module::*, ui_main::*, utils::*};


pub fn init_ui(
    window:  &gtk::ApplicationWindow,
    main_ui: &Rc<MainUi>,
    engine:  &Arc<Engine>,
) -> Rc<dyn UiModule> {
    let widgets = Widgets::from_builder_str(include_str!(r"resources/platesolve.ui"));
    let obj = Rc::new(PlateSolveUi {
        widgets,
        window:          window.clone(),
        main_ui:         Rc::clone(main_ui),
        engine:          Arc::clone(engine),
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
    engine:          Arc<Engine>,
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
                let options = self.engine.options.read().unwrap();
                self.show_options(&options);
                drop(options);
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

fn apply_spin_range(spb: &gtk::SpinButton, range: Option<&crate::api::ui_state::SpinRange>) {
    match range {
        Some(r) => correct_spinbutton_by_range(spb, Some(r.start..=r.end), r.digits, r.step),
        None    => correct_spinbutton_by_range(spb, None, 0, None),
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

        self.widgets.spb_exp.connect_value_changed(
            clone!(@weak self as self_ => move |sb| {
                let patch = OptionPatch::PlateSolver(PlateSolverOptionPatch { exposure: Some(sb.value()), ..Default::default() });
                self_.engine.exec_command(Command::ChangeOptions(patch));
            })
        );

        self.widgets.cbx_gain.connect_active_id_notify(
            clone!(@weak self as self_ => move |cb| {
                let gain = Gain::from_active_id(cb.active_id().as_deref());
                let patch = OptionPatch::PlateSolver(PlateSolverOptionPatch { gain: Some(gain), ..Default::default() });
                self_.engine.exec_command(Command::ChangeOptions(patch));
            })
        );

        self.widgets.cbx_bin.connect_active_id_notify(
            clone!(@weak self as self_ => move |cb| {
                let bin = Binning::from_active_id(cb.active_id().as_deref());
                let patch = OptionPatch::PlateSolver(PlateSolverOptionPatch { bin: Some(bin), ..Default::default() });
                self_.engine.exec_command(Command::ChangeOptions(patch));
            })
        );

        self.widgets.cbx_solver.connect_active_id_notify(
            clone!(@weak self as self_ => move |cb| {
                let solver = PlateSolverType::from_active_id(cb.active_id().as_deref());
                let patch = OptionPatch::PlateSolver(PlateSolverOptionPatch { solver: Some(solver), ..Default::default() });
                self_.engine.exec_command(Command::ChangeOptions(patch));
            })
        );

        self.widgets.spb_timeout.connect_value_changed(
            clone!(@weak self as self_ => move |sb| {
                let patch = OptionPatch::PlateSolver(PlateSolverOptionPatch { timeout: Some(sb.value() as _), ..Default::default() });
                self_.engine.exec_command(Command::ChangeOptions(patch));
            })
        );

        self.widgets.spb_blind_timeout.connect_value_changed(
            clone!(@weak self as self_ => move |sb| {
                let patch = OptionPatch::PlateSolver(PlateSolverOptionPatch { blind_timeout: Some(sb.value() as _), ..Default::default() });
                self_.engine.exec_command(Command::ChangeOptions(patch));
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
        let state = crate::api::ui_state::plate_solve_panel_state(&self.engine);
        self.apply_widgets_state(&state);
    }

    fn apply_widgets_state(&self, s: &crate::api::ui_state::PlateSolvePanelState) {
        apply_spin_range(&self.widgets.spb_exp, s.spb_exp_range.as_ref());
        self.widgets.grd.set_sensitive(s.grd_sensitive);
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

        self.engine.exec_command(Command::StartMode(ModeStart::CaptureAndPlatesolve));
    }

    fn handler_action_plate_solve_and_goto(&self) {
        self.main_ui.get_all_options();

        self.engine.exec_command(Command::StartMode(ModeStart::GotoImage));
    }
}
