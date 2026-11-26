use std::{rc::Rc, sync::Arc};
use gtk::{prelude::*, glib, glib::clone};
use macros::FromBuilder;

use crate::{
    core::{commands::Command, engine::*, events::*}, guiding::external_guider::*, hal::events::HalEvent, options::*
};

use super::{gtk_utils::*, module::*, ui_main::*, utils::*};

pub fn init_ui(
    window:  &gtk::ApplicationWindow,
    main_ui: &Rc<MainUi>,
    engine:  &Arc<Engine>,
) -> Rc<dyn UiModule> {
    let widgets = Widgets::from_builder_str(include_str!(r"resources/guiding.ui"));
    let info_widgets = InfoWidgets::new();

    let obj = Rc::new(GuidingUi {
        widgets,
        info_widgets,
        window:        window.clone(),
        main_ui:       Rc::clone(main_ui),
        engine:        Arc::clone(engine),
    });

    obj.init_widgets();
    obj.connect_widgets_events();

    obj
}

#[derive(FromBuilder)]
struct Widgets {
    grd:                 gtk::Grid,
    cb_dith_period:       gtk::ComboBoxText,
    rbtn_no_guiding:     gtk::RadioButton,
    rbtn_guide_main_cam: gtk::RadioButton,
    spb_dith_dist:       gtk::SpinButton,
    spb_guid_max_err:    gtk::SpinButton,
    spb_mnt_cal_exp:     gtk::SpinButton,
    cbx_mnt_cal_gain:    gtk::ComboBoxText,
    rbtn_guide_ext:      gtk::RadioButton,
    spb_ext_dith_dist:   gtk::SpinButton,
}

struct InfoWidgets {
    bx:     gtk::Box,
    l_info: gtk::Label,
}

impl InfoWidgets {
    fn new() -> Self {
        let bx = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(5)
            .visible(true)
            .build();
        let l_info = gtk::Label::builder()
            .label("info")
            .use_markup(true)
            .xalign(0.0)
            .halign(gtk::Align::Start)
            .width_chars(10)
            .max_width_chars(30)
            .visible(true)
            .build();
        bx.add(&l_info);
        Self { bx, l_info }
    }
}

struct GuidingUi {
    widgets:      Widgets,
    info_widgets: InfoWidgets,
    main_ui:      Rc<MainUi>,
    window:       gtk::ApplicationWindow,
    engine:       Arc<Engine>,
}

impl Drop for GuidingUi {
    fn drop(&mut self) {
        log::info!("GuidingUi dropped");
    }
}

impl UiModule for GuidingUi {
    fn show_options(&self, options: &Options) {
        match options.guiding.mode {
            GuidingMode::Disabled =>
                self.widgets.rbtn_no_guiding.set_active(true),
            GuidingMode::MainCamera =>
                self.widgets.rbtn_guide_main_cam.set_active(true),
            GuidingMode::External =>
                self.widgets.rbtn_guide_ext.set_active(true),
        }
        self.widgets.cb_dith_period    .set_active_id(Some(options.guiding.dith_period.to_string().as_str()));
        self.widgets.spb_ext_dith_dist.set_value    (options.guiding.ext_guider.dith_dist as f64);
        self.widgets.spb_guid_max_err .set_value    (options.guiding.main_cam.max_error);
        self.widgets.spb_dith_dist    .set_value    (options.guiding.main_cam.dith_dist as f64);
        self.widgets.cbx_mnt_cal_gain .set_active_id(Some(options.guiding.main_cam.calibr_gain.to_active_id()));
        set_spb_value(&self.widgets.spb_mnt_cal_exp, options.guiding.main_cam.calibr_exposure);
    }

    fn get_options(&self, options: &mut Options) {
        options.guiding.mode =
            if self.widgets.rbtn_guide_main_cam.is_active() {
                GuidingMode::MainCamera
            } else if self.widgets.rbtn_guide_ext.is_active() {
                GuidingMode::External
            } else {
                GuidingMode::Disabled
            };

        options.guiding.dith_period              = self.widgets.cb_dith_period.active_id().and_then(|v| v.parse().ok()).unwrap_or(0);
        options.guiding.ext_guider.dith_dist     = self.widgets.spb_ext_dith_dist.value() as i32;

        options.guiding.main_cam.dith_dist       = self.widgets.spb_dith_dist.value() as i32;
        options.guiding.main_cam.calibr_exposure = self.widgets.spb_mnt_cal_exp.value();
        options.guiding.main_cam.calibr_gain     = Gain::from_active_id(self.widgets.cbx_mnt_cal_gain.active_id().as_deref());
        options.guiding.main_cam.max_error       = self.widgets.spb_guid_max_err.value();
    }

    fn panels(&self) -> Vec<Panel> {
        vec![
            Panel {
                str_id: "guiding",
                name:   "Guiding".to_string(),
                widget: self.widgets.grd.clone().upcast(),
                pos:    PanelPosition::Right,
                tab:    TabPage::Main,
                flags:  PanelFlags::empty(),
            },
            Panel {
                str_id: "guiding_info",
                name:   "Guiding".to_string(),
                widget: self.info_widgets.bx.clone().upcast(),
                pos:    PanelPosition::Bottom,
                tab:    TabPage::Main,
                flags:  PanelFlags::NO_EXPANDER|PanelFlags::INVISIBLE,
            },
        ]
    }

    fn on_show_options_first_time(&self) {
        self.correct_widgets_props();
    }

    fn on_event(&self, event: &Event) {
        match event {
            Event::ModeChanged => {
                self.correct_widgets_props();
            }
            Event::CameraDeviceChanged(_) => {
                let options = self.engine.options.read().unwrap();
                self.show_options(&options);
                drop(options);
                self.correct_widgets_props();
            }
            Event::Guider(evt) => {
                self.process_ext_guider_event(evt);
            }
            Event::NeedRefreshOptionsView => {
                self.correct_widgets_props();
            }
            _ => {}
        }
    }

    fn on_hal_event(&self, event: &HalEvent) {
        match event {
            HalEvent::StateChanged(_) => {
                self.correct_widgets_props();
            }
            _ => {}
        }
    }
}

fn apply_spin_range(spb: &gtk::SpinButton, range: Option<&crate::api::ui_state::SpinRange>) {
    match range {
        Some(r) => correct_spinbutton_by_range(spb, Some(r.start..=r.end), r.digits, r.step),
        None    => correct_spinbutton_by_range(spb, None, 0, None),
    }
}

impl GuidingUi {
    fn init_widgets(&self) {
        self.widgets.spb_guid_max_err.set_range(3.0, 50.0);
        self.widgets.spb_guid_max_err.set_digits(0);
        self.widgets.spb_guid_max_err.set_increments(1.0, 10.0);

        self.widgets.spb_mnt_cal_exp.set_range(0.5, 10.0);
        self.widgets.spb_mnt_cal_exp.set_digits(1);
        self.widgets.spb_mnt_cal_exp.set_increments(0.5, 5.0);

        self.widgets.spb_dith_dist.set_range(1.0, 300.0);
        self.widgets.spb_dith_dist.set_digits(0);
        self.widgets.spb_dith_dist.set_increments(1.0, 10.0);

        self.widgets.spb_ext_dith_dist.set_range(1.0, 300.0);
        self.widgets.spb_ext_dith_dist.set_digits(0);
        self.widgets.spb_ext_dith_dist.set_increments(1.0, 10.0);

        self.show_info_text_disconnected();
    }

    fn connect_widgets_events(self: &Rc<Self>) {
        connect_action(&self.window, self, "start_dither_calibr", Self::handler_action_start_dither_calibr);
        connect_action(&self.window, self, "stop_dither_calibr",  Self::handler_action_stop_dither_calibr);

        let connect_rbtn = |rbtn: &gtk::RadioButton| {
            rbtn.connect_active_notify(clone!(@weak self as self_ => move |rbtn| {
                if rbtn.is_active() {
                    let mode =
                        if self_.widgets.rbtn_guide_main_cam.is_active() {
                            GuidingMode::MainCamera
                        } else if self_.widgets.rbtn_guide_ext.is_active() {
                            GuidingMode::External
                        } else {
                            GuidingMode::Disabled
                        };
                    let patch = OptionPatch::Guiding(GuidingOptionPatch { mode: Some(mode), ..Default::default() });
                    self_.engine.exec_command(Command::ChangeOptions(patch));
                }
            }));
        };

        connect_rbtn(&self.widgets.rbtn_no_guiding);
        connect_rbtn(&self.widgets.rbtn_guide_main_cam);
        connect_rbtn(&self.widgets.rbtn_guide_ext);

        self.widgets.cb_dith_period.connect_active_id_notify(clone!(@weak self as self_ => move |cb| {
            let dith_period = cb.active_id().and_then(|v| v.parse().ok()).unwrap_or(0);
            let patch = OptionPatch::Guiding(GuidingOptionPatch { dith_period: Some(dith_period), ..Default::default() });
            self_.engine.exec_command(Command::ChangeOptions(patch));
        }));

        self.widgets.spb_dith_dist.connect_value_changed(clone!(@weak self as self_ => move |sb| {
            let patch = OptionPatch::Guiding(GuidingOptionPatch { main_cam: Some(MainCamGuidingOptionPatch { dith_dist: Some(sb.value() as i32), ..Default::default() }), ..Default::default() });
            self_.engine.exec_command(Command::ChangeOptions(patch));
        }));

        self.widgets.spb_guid_max_err.connect_value_changed(clone!(@weak self as self_ => move |sb| {
            let patch = OptionPatch::Guiding(GuidingOptionPatch { main_cam: Some(MainCamGuidingOptionPatch { max_error: Some(sb.value()), ..Default::default() }), ..Default::default() });
            self_.engine.exec_command(Command::ChangeOptions(patch));
        }));

        self.widgets.spb_ext_dith_dist.connect_value_changed(clone!(@weak self as self_ => move |sb| {
            let patch = OptionPatch::Guiding(GuidingOptionPatch { ext_guider: Some(ExtGuiderOptionPatch { dith_dist: Some(sb.value() as i32) }), ..Default::default() });
            self_.engine.exec_command(Command::ChangeOptions(patch));
        }));

        self.widgets.spb_mnt_cal_exp.connect_value_changed(clone!(@weak self as self_ => move |sb| {
            let patch = OptionPatch::Guiding(GuidingOptionPatch { main_cam: Some(MainCamGuidingOptionPatch { calibr_exposure: Some(sb.value()), ..Default::default() }), ..Default::default() });
            self_.engine.exec_command(Command::ChangeOptions(patch));
        }));

        self.widgets.cbx_mnt_cal_gain.connect_active_id_notify(clone!(@weak self as self_ => move |cb| {
            let gain = Gain::from_active_id(cb.active_id().as_deref());
            let patch = OptionPatch::Guiding(GuidingOptionPatch { main_cam: Some(MainCamGuidingOptionPatch { calibr_gain: Some(gain), ..Default::default() }), ..Default::default() });
            self_.engine.exec_command(Command::ChangeOptions(patch));
        }));
    }

    fn correct_widgets_props(&self) {
        let state = crate::api::ui_state::guiding_panel_state(&self.engine);
        self.apply_widgets_state(&state);
    }

    fn apply_widgets_state(&self, s: &crate::api::ui_state::GuidingPanelState) {
        apply_spin_range(&self.widgets.spb_mnt_cal_exp, s.spb_mnt_cal_exp_range.as_ref());

        self.widgets.grd.set_sensitive(s.grd_sensitive);
        self.widgets.rbtn_no_guiding.set_sensitive(s.rbtn_no_guiding_sensitive);
        self.widgets.rbtn_guide_main_cam.set_sensitive(s.rbtn_guide_main_cam_sensitive);
        self.widgets.rbtn_guide_ext.set_sensitive(s.rbtn_guide_ext_sensitive);
        self.widgets.cb_dith_period.set_sensitive(s.cb_dith_period_sensitive);
        self.widgets.spb_dith_dist.set_sensitive(s.spb_dith_dist_sensitive);
        self.widgets.spb_guid_max_err.set_sensitive(s.spb_guid_max_err_sensitive);
        self.widgets.spb_mnt_cal_exp.set_sensitive(s.spb_mnt_cal_exp_sensitive);
        self.widgets.spb_ext_dith_dist.set_sensitive(s.spb_ext_dith_dist_sensitive);

        enable_actions(&self.window, &[
            ("start_dither_calibr", s.actions.start_dither_calibr),
            ("stop_dither_calibr",  s.actions.stop_dither_calibr),
        ]);

        self.main_ui.set_module_panel_visible(
            self.info_widgets.bx.upcast_ref(),
            s.module_panel_visible
        );
    }

    fn handler_action_start_dither_calibr(&self) {
        self.main_ui.get_all_options();

        exec_and_show_error(Some(&self.window), || {
            self.engine.start_mount_calibr()?;
            Ok(())
        });
    }

    fn handler_action_stop_dither_calibr(&self) {
        self.engine.abort_active_mode();
    }

    fn process_ext_guider_event(&self, evt: &ExtGuiderEvent) {
        match evt {
            ExtGuiderEvent::State(state) =>
                self.show_ext_guider_state(state),
            ExtGuiderEvent::Error(err) =>
                self.show_ext_guider_error(err),
            ExtGuiderEvent::DitheringFinishedWithErr(err) =>
                self.show_ext_guider_error(err),
            ExtGuiderEvent::Connected => {
                if let Some(state) = self.engine.ext_guider.state() {
                    self.show_ext_guider_state(&state);
                }
            }
            ExtGuiderEvent::Disconnected =>
                self.show_info_text_disconnected(),
            _ => {}
        }
    }

    fn show_info_text_disconnected(&self) {
        self.show_info_text("Disconnected", Some(get_err_color_str()));
    }

    fn show_ext_guider_error(&self, err: &str) {
        self.show_info_text(err, Some(get_err_color_str()));
    }

    fn show_ext_guider_state(&self, state: &ExtGuiderState) {
        let color = match state {
            ExtGuiderState::Guiding =>
                Some(get_ok_color_str()),
            ExtGuiderState::Stopped|
            ExtGuiderState::Looping =>
                Some(get_warn_color_str()),
            _ =>
                None,
        };
        self.show_info_text(&format!("{:?}", state), color);
    }

    fn show_info_text(&self, text: &str, color: Option<&str>) {
        let mut text_str = format!("<b>{}</b>", text);
        if let Some(color) = color {
            text_str = format!("<span foreground='{}'>{}</span>", color, text_str);
        }
        self.info_widgets.l_info.set_label(&text_str);
        self.info_widgets.l_info.set_tooltip_text(Some(text));
    }
}
