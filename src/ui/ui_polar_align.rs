use std::{cell::Cell, rc::Rc, sync::Arc};
use gtk::{glib::{self, clone}, pango, prelude::*};
use macros::FromBuilder;
use crate::{
    core::{commands::{Command, ModeCommand, ModeStart}, engine::{Engine, ModeKind}, events::*, mode_polar_align::{PolarAlignCommand, PolarAlignMode, PolarAlignmentEvent}},
    hal::{DeviceType, events::HalEvent},
    options::*,
    sky_math::math::*,
};
use ::indi::degree_to_str_short;
use super::{gtk_utils::{self, *}, module::*, ui_main::*, utils::*};

pub fn init_ui(
    window:  &gtk::ApplicationWindow,
    main_ui: &Rc<MainUi>,
    engine:  &Arc<Engine>,
) -> Rc<dyn UiModule> {
    let widgets = Widgets::from_builder_str(include_str!(r"resources/polar_align.ui"));
    let obj = Rc::new(PolarAlignUi {
        widgets,
        window:             window.clone(),
        main_ui:            Rc::clone(main_ui),
        engine:             Arc::clone(engine),
        delayed_actions:    DelayedActions::new(200),
        filling_speed_list: Cell::new(false),
    });

    obj.init_widgets();
    obj.connect_widgets_events();
    obj.connect_delayed_actions_events();
    obj.update_mount_speed_list();

    obj
}

#[derive(Hash, Eq, PartialEq)]
enum DelayedAction {
    CorrectWidgetsProps,
    UpdateMountSpeedList,
}

impl PolarAlignDir {
    pub fn from_active_id(active_id: Option<&str>) -> Option<Self> {
        match active_id {
            Some("east") => Some(Self::East),
            Some("west") => Some(Self::West),
            _            => None,
        }
    }

    pub fn to_active_id(&self) -> Option<&'static str> {
        match self {
            Self::East  => Some("east"),
            Self::West  => Some("west"),
        }
    }
}

#[derive(FromBuilder)]
struct Widgets {
    bx:               gtk::Box,
    spb_angle:        gtk::SpinButton,
    cbx_dir:          gtk::ComboBoxText,
    cbx_speed:        gtk::ComboBoxText,
    chb_auto_refresh: gtk::CheckButton,
    l_step:           gtk::Label,
    l_alt_err:        gtk::Label,
    l_az_err:         gtk::Label,
    l_alt_err_arr:    gtk::Label,
    l_az_err_arr:     gtk::Label,
    l_tot_err:        gtk::Label,
}

struct PolarAlignUi {
    widgets:            Widgets,
    main_ui:            Rc<MainUi>,
    window:             gtk::ApplicationWindow,
    engine:             Arc<Engine>,
    delayed_actions:    DelayedActions<DelayedAction>,
    filling_speed_list: Cell<bool>,
}

impl Drop for PolarAlignUi {
    fn drop(&mut self) {
        log::info!("PolarAlignUi dropped");
    }
}

impl UiModule for PolarAlignUi {
    fn show_options(&self, options: &Options) {
        self.widgets.spb_angle.set_value(options.polar_align.angle);
        self.widgets.cbx_dir.set_active_id(options.polar_align.direction.to_active_id());
        self.widgets.cbx_speed.set_active_id(options.polar_align.speed.as_deref());
        self.widgets.chb_auto_refresh.set_active(options.polar_align.auto_refresh);
    }

    fn get_options(&self, options: &mut Options) {
        options.polar_align.angle        = self.widgets.spb_angle.value();
        options.polar_align.speed        = self.widgets.cbx_speed.active_id().map(|s| s.to_string());
        options.polar_align.auto_refresh = self.widgets.chb_auto_refresh.is_active();

        options.polar_align.direction = PolarAlignDir::from_active_id(
            self.widgets.cbx_dir.active_id().as_deref()
        ).unwrap_or(PolarAlignDir::West);
    }

    fn panels(&self) -> Vec<Panel> {
        vec![Panel {
            str_id: "polar_align",
            name:   "Polar alignment".to_string(),
            widget: self.widgets.bx.clone().upcast(),
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
            Event::Progress(_, mode) => {
                if *mode == ModeKind::PolarAlignment {
                    self.delayed_actions.schedule(DelayedAction::CorrectWidgetsProps);
                }
            }
            Event::CameraDeviceChanged(_) => {
                self.correct_widgets_props();
            }
            Event::MountDeviceChanged(_) => {
                self.update_mount_speed_list();
                self.correct_widgets_props();
            }
            Event::PolarAlignment(event) => {
                self.show_polar_alignment_error(event);
            }
            Event::NeedRefreshOptionsView => {
                self.correct_widgets_props();
            }
            _ => {}
        }
    }

    fn on_hal_event(&self, event: &HalEvent) {
        match event {
            HalEvent::DeviceConnected(info)|
            HalEvent::DeviceDisconnected(info) => {
                if info.type_.contains(DeviceType::TELESCOPE) {
                    self.delayed_actions.schedule(DelayedAction::CorrectWidgetsProps);
                }
            }
            HalEvent::TelescopeSlewRateListReady(device_id) => {
                let options = self.engine.options.read().unwrap();
                if options.mount.device == **device_id {
                    self.delayed_actions.schedule(DelayedAction::UpdateMountSpeedList);
                }
            }
            _ => {}
        }
    }
}

impl PolarAlignUi {
    fn init_widgets(&self) {
        self.widgets.spb_angle.set_range(15.0, 60.0);
        self.widgets.spb_angle.set_digits(0);
        self.widgets.spb_angle.set_increments(5.0, 15.0);

        self.widgets.l_alt_err.set_label("---");
        self.widgets.l_az_err.set_label("---");
        self.widgets.l_tot_err.set_label("---");
        self.widgets.l_alt_err_arr.set_label("");
        self.widgets.l_az_err_arr.set_label("");
    }

    fn connect_widgets_events(self: &Rc<Self>) {
        connect_action(&self.window, self, "start_polar_alignment", Self::handler_action_start_polar_align);
        connect_action(&self.window, self, "restart_polar_alignment", Self::handler_action_restart_polar_align);
        connect_action(&self.window, self, "stop_polar_alignment", Self::handler_action_stop_polar_align);
        connect_action(&self.window, self, "pa_manual_refresh", Self::handler_action_manual_refresh);

        self.widgets.spb_angle.connect_value_changed(
            clone!(@weak self as self_ => move |spb| {
                let patch = OptionPatch::PolarAlign(PolarAlignOptionPatch {
                    angle: Some(spb.value()),
                    ..Default::default()
                });
                self_.engine.exec_command(Command::ChangeOptions(patch));
            })
        );

        self.widgets.cbx_dir.connect_active_id_notify(
            clone!(@weak self as self_ => move |cbx| {
                let direction = PolarAlignDir::from_active_id(cbx.active_id().as_deref())
                    .unwrap_or(PolarAlignDir::West);
                let patch = OptionPatch::PolarAlign(PolarAlignOptionPatch {
                    direction: Some(direction),
                    ..Default::default()
                });
                self_.engine.exec_command(Command::ChangeOptions(patch));
            })
        );

        self.widgets.cbx_speed.connect_active_id_notify(
            clone!(@weak self as self_ => move |cbx| {
                if self_.filling_speed_list.get() { return; }
                let speed = cbx.active_id().map(|s| s.to_string());
                let patch = OptionPatch::PolarAlign(PolarAlignOptionPatch {
                    speed: Some(speed),
                    ..Default::default()
                });
                self_.engine.exec_command(Command::ChangeOptions(patch));
            })
        );

        self.widgets.chb_auto_refresh.connect_active_notify(
            clone!(@weak self as self_ => move |chb| {
                let patch = OptionPatch::PolarAlign(PolarAlignOptionPatch {
                    auto_refresh: Some(chb.is_active()),
                    ..Default::default()
                });
                self_.engine.exec_command(Command::ChangeOptions(patch));
            })
        );
    }

    fn correct_widgets_props(&self) {
        let state = crate::api::ui_state::polar_align_panel_state(&self.engine);
        self.apply_widgets_state(&state);
    }

    fn apply_widgets_state(&self, s: &crate::api::ui_state::PolarAlignPanelState) {
        self.widgets.chb_auto_refresh.set_sensitive(s.chb_auto_refresh_sensitive);

        enable_actions(&self.window, &[
            ("start_polar_alignment",   s.actions.start_polar_alignment),
            ("restart_polar_alignment", s.actions.restart_polar_alignment),
            ("stop_polar_alignment",    s.actions.stop_polar_alignment),
            ("pa_manual_refresh",       s.actions.pa_manual_refresh),
        ]);
    }

    fn connect_delayed_actions_events(self: &Rc<Self>) {
        self.delayed_actions.set_event_handler(
            clone!(@weak self as self_ => move |action| {
                self_.handler_delayed_action(action);
            })
        );
    }

    fn handler_delayed_action(&self, action: &DelayedAction) {
        match action {
            DelayedAction::CorrectWidgetsProps => {
                self.correct_widgets_props();
            }
            DelayedAction::UpdateMountSpeedList => {
                self.update_mount_speed_list();
            }
        }
    }

    fn update_mount_speed_list(&self) {
        // Programmatic list refill must not clobber the saved speed via the active_id_notify subscription.
        self.filling_speed_list.set(true);

        let state = crate::api::ui_state::polar_align_panel_state(&self.engine);
        let cur_speed = self.engine.options.read().unwrap().polar_align.speed.clone();
        let list = &state.slew_speed_list;
        self.widgets.cbx_speed.remove_all();
        if !list.is_empty() {
            self.widgets.cbx_speed.append(None, "---");
        }
        for item in list {
            self.widgets.cbx_speed.append(Some(item.id.as_str()), item.label.as_str());
        }
        if let Some(speed) = cur_speed.as_deref() {
            self.widgets.cbx_speed.set_active_id(Some(speed));
            if self.widgets.cbx_speed.active().is_none() {
                self.widgets.cbx_speed.append(Some(speed), speed);
                self.widgets.cbx_speed.set_active_id(Some(speed));
            }
        }

        self.filling_speed_list.set(false);
    }

    fn handler_action_start_polar_align(&self) {
        self.main_ui.get_all_options();

        // Pre-start mode check

        let check_result = PolarAlignMode::check_before_start(
            &self.engine.hal,
            &self.engine.options
        );

        match check_result {
            Err(err) => {
                gtk_utils::show_message(
                    Some(&self.window),
                    "Error",
                    &err.to_string(),
                    gtk::MessageType::Error
                );
                return;
            }
            Ok(mut warn_text) => {
                if !warn_text.is_empty() {
                    warn_text += "\n\nContinue?";
                    let dialog = gtk::MessageDialog::builder()
                        .title("Warning")
                        .text(&warn_text)
                        .modal(true)
                        .message_type(gtk::MessageType::Warning)
                        .buttons(gtk::ButtonsType::YesNo)
                        .transient_for(&self.window)
                        .build();
                    let dialog_result = dialog.run();
                    dialog.close();
                    if dialog_result != gtk::ResponseType::Yes {
                        return;
                    }
                }
            }
        }

        // Start mode

        self.engine.exec_command(Command::StartMode(ModeStart::PolarAlign));
    }

    fn handler_action_restart_polar_align(&self) {
        self.main_ui.get_all_options();
        exec_and_show_error(Some(&self.window), || {
            self.engine.exec_mode_command(ModeCommand::PolarAlign(PolarAlignCommand::Restart))?;
            Ok(())
        });
    }

    fn handler_action_stop_polar_align(&self) {
        self.engine.exec_command(Command::AbortActiveMode);
    }

    fn handler_action_manual_refresh(&self) {
        gtk_utils::exec_and_show_error(Some(&self.window), || {
            self.engine.exec_mode_command(ModeCommand::PolarAlign(PolarAlignCommand::ManualRefresh))?;
            Ok(())
        });
    }

    fn show_polar_alignment_error(&self, event: &PolarAlignmentEvent) {
        match event {
            PolarAlignmentEvent::Error { horiz, total, step } => {
                self.widgets.l_step.set_label(&format!("({})", step));

                let alt_err_str = degree_to_str_short(radian_to_degree(horiz.alt));
                self.widgets.l_alt_err.set_label(&alt_err_str);

                let az_err_str = degree_to_str_short(radian_to_degree(horiz.az));
                self.widgets.l_az_err.set_label(&az_err_str);

                let total_err_str = degree_to_str_short(radian_to_degree(*total));
                self.widgets.l_tot_err.set_label(&total_err_str);

                let alt_err_arrow = if horiz.alt < 0.0 { "↑" } else { "↓" };
                let az_err_arrow = if horiz.az < 0.0 { "→" } else { "←" };
                self.widgets.l_alt_err_arr.set_label(alt_err_arrow);
                self.widgets.l_az_err_arr.set_label(az_err_arrow);

                let set_all_label_size = |label: &gtk::Label, err: f64| {
                    let err_minutes = f64::abs(radian_to_degree(err) * 60.0);
                    let scale = if err_minutes > 60.0 {
                        5
                    } else if err_minutes > 2.0 {
                        3
                    } else {
                        1
                    };

                    let alt_attrs = pango::AttrList::new();
                    let attr_alt_size = pango::AttrSize::new(scale * 10 * pango::SCALE);
                    alt_attrs.insert(attr_alt_size);

                    label.set_attributes(Some(&alt_attrs));
                };

                set_all_label_size(&self.widgets.l_alt_err_arr, horiz.alt);
                set_all_label_size(&self.widgets.l_az_err_arr, horiz.az);
            }
            PolarAlignmentEvent::Empty => {
                self.widgets.l_step.set_label("");
                self.widgets.l_alt_err.set_label("---");
                self.widgets.l_az_err.set_label("---");
                self.widgets.l_tot_err.set_label("---");

                self.widgets.l_alt_err_arr.set_text("");
                self.widgets.l_az_err_arr.set_text("");
            }
        }
    }
}
