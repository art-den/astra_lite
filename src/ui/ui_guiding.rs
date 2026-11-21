use std::{cell::Cell, rc::Rc, sync::Arc};
use gtk::{prelude::*, glib, glib::clone};
use macros::FromBuilder;

use crate::{
    core::{
        api::{Command, GuidingPatch, Query, QueryReply, Reply, StartMode},
        engine::*, events::*,
    }, guiding::external_guider::*, hal::events::HalEvent, options::*,
    utils::log_utils::log_if_error
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
        opts_syncing:  Cell::new(false),
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
    opts_syncing: Cell<bool>,
}

impl Drop for GuidingUi {
    fn drop(&mut self) {
        log::info!("GuidingUi dropped");
    }
}

impl UiModule for GuidingUi {
    fn show_options(&self, options: &Options) {
        self.sync_suppress(|| {
            match options.guiding.mode {
                GuidingMode::Disabled =>
                    self.widgets.rbtn_no_guiding.set_active(true),
                GuidingMode::MainCamera =>
                    self.widgets.rbtn_guide_main_cam.set_active(true),
                GuidingMode::External =>
                    self.widgets.rbtn_guide_ext.set_active(true),
            }
            self.widgets.cb_dith_period    .set_active_id(Some(options.guiding.dith_period.to_string().as_str()));
            self.widgets.spb_ext_dith_dist .set_value    (options.guiding.ext_guider.dith_dist as f64);
            self.widgets.spb_guid_max_err  .set_value    (options.guiding.main_cam.max_error);
            self.widgets.spb_dith_dist     .set_value    (options.guiding.main_cam.dith_dist as f64);
            self.widgets.cbx_mnt_cal_gain  .set_active_id(Some(options.guiding.main_cam.calibr_gain.to_active_id()));
            set_spb_value(&self.widgets.spb_mnt_cal_exp, options.guiding.main_cam.calibr_exposure);
        });
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
                let snap = self.main_ui.snapshot();
                self.show_options(&snap.options);
                self.correct_widgets_props_impl();
            }
            Event::Guider(evt) => {
                self.process_ext_guider_event(evt);
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

impl GuidingUi {
    fn sync_suppress(&self, sync: impl FnOnce()) {
        let prev = self.opts_syncing.replace(true);
        sync();
        self.opts_syncing.set(prev);
    }

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
            rbtn.connect_active_notify(clone!(@weak self as self_ => move |_| {
                if !self_.opts_syncing.get() {
                    self_.commit_guiding_mode();
                }
                self_.correct_widgets_props();
            }));
        };

        connect_rbtn(&self.widgets.rbtn_no_guiding);
        connect_rbtn(&self.widgets.rbtn_guide_main_cam);
        connect_rbtn(&self.widgets.rbtn_guide_ext);

        self.widgets.spb_mnt_cal_exp.connect_value_changed(clone!(@weak self as self_ => move |sb| {
            if self_.opts_syncing.get() { return; }
            log_if_error(
                &self_.engine.exec_command(
                    Command::SetGuidingOptions(GuidingPatch {
                        calibr_exposure: Some(sb.value()),
                        ..Default::default()
                    })
                ),
                "Set guiding options"
            );
        }));

        self.widgets.cbx_mnt_cal_gain.connect_active_id_notify(clone!(@weak self as self_ => move |cb| {
            if self_.opts_syncing.get() { return; }
            log_if_error(
                &self_.engine.exec_command(
                    Command::SetGuidingOptions(GuidingPatch {
                        calibr_gain: Some(Gain::from_active_id(cb.active_id().as_deref())),
                        ..Default::default()
                    })
                ),
                "Set guiding options"
            );
        }));

        self.widgets.cb_dith_period.connect_active_id_notify(clone!(@weak self as self_ => move |cb| {
            if self_.opts_syncing.get() { return; }
            log_if_error(
                &self_.engine.exec_command(
                    Command::SetGuidingOptions(GuidingPatch {
                        dith_period: Some(
                            cb.active_id().and_then(|v| v.parse().ok()).unwrap_or(0)
                        ),
                        ..Default::default()
                    })
                ),
                "Set guiding options"
            );
        }));

        self.widgets.spb_dith_dist.connect_value_changed(clone!(@weak self as self_ => move |sb| {
            if self_.opts_syncing.get() { return; }
            log_if_error(
                &self_.engine.exec_command(
                    Command::SetGuidingOptions(GuidingPatch {
                        main_cam_dith_dist: Some(sb.value() as i32),
                        ..Default::default()
                    })
                ),
                "Set guiding options"
            );
        }));

        self.widgets.spb_ext_dith_dist.connect_value_changed(clone!(@weak self as self_ => move |sb| {
            if self_.opts_syncing.get() { return; }
            log_if_error(
                &self_.engine.exec_command(
                    Command::SetGuidingOptions(GuidingPatch {
                        ext_dith_dist: Some(sb.value() as i32),
                        ..Default::default()
                    })
                ),
                "Set guiding options"
            );
        }));

        self.widgets.spb_guid_max_err.connect_value_changed(clone!(@weak self as self_ => move |sb| {
            if self_.opts_syncing.get() { return; }
            log_if_error(
                &self_.engine.exec_command(
                    Command::SetGuidingOptions(GuidingPatch {
                        main_cam_max_error: Some(sb.value()),
                        ..Default::default()
                    })
                ),
                "Set guiding options"
            );
        }));
    }

    /// Commits the current radio button selection as `guiding.mode`
    fn commit_guiding_mode(&self) {
        let mode = if self.widgets.rbtn_guide_main_cam.is_active() {
            GuidingMode::MainCamera
        } else if self.widgets.rbtn_guide_ext.is_active() {
            GuidingMode::External
        } else {
            GuidingMode::Disabled
        };
        log_if_error(
            &self.engine.exec_command(Command::SetGuidingOptions(GuidingPatch {
                mode: Some(mode),
                ..Default::default()
            })),
            "Set guiding mode"
        );
    }

    fn correct_widgets_props_impl(&self) {
        let perms = &self.main_ui.snapshot().permissions;
        let can_change_mode = perms.can_change_mode;
        let dither_calibr = perms.dither_calibr;

        let disabled = self.widgets.rbtn_no_guiding.is_active();
        let by_main_cam = self.widgets.rbtn_guide_main_cam.is_active();
        let by_ext = self.widgets.rbtn_guide_ext.is_active();

        // Camera exposure range via the query API; no camera => leave the
        // spinbutton uncorrected (old `camera_or_err` skip semantics)
        if let Ok(Reply::Query(q)) = self.engine.exec_command(Command::Query(Query::CameraCaps))
            && let QueryReply::CameraCaps(caps) = *q
        {
            let exp_range = caps.exposure_range.map(|r| r.start..=r.end);
            correct_spinbutton_by_range(&self.widgets.spb_mnt_cal_exp, exp_range, 1, Some(1.0));
        }

        self.widgets.grd.set_sensitive(true);
        self.widgets.rbtn_no_guiding.set_sensitive(can_change_mode);
        self.widgets.rbtn_guide_main_cam.set_sensitive(can_change_mode);
        self.widgets.rbtn_guide_ext.set_sensitive(can_change_mode);
        self.widgets.cb_dith_period.set_sensitive(!disabled && can_change_mode);
        self.widgets.spb_dith_dist.set_sensitive(by_main_cam && can_change_mode);
        self.widgets.spb_guid_max_err.set_sensitive(by_main_cam && can_change_mode);
        self.widgets.spb_mnt_cal_exp.set_sensitive(by_main_cam && can_change_mode);
        self.widgets.spb_ext_dith_dist.set_sensitive(by_ext && can_change_mode);

        enable_actions(&self.window, &[
            ("start_dither_calibr", !dither_calibr && by_main_cam && can_change_mode),
            ("stop_dither_calibr", dither_calibr),
        ]);

        self.main_ui.set_module_panel_visible(
            self.info_widgets.bx.upcast_ref(),
            self.widgets.rbtn_guide_ext.is_active()
        );
    }

    fn correct_widgets_props(&self) {
        self.correct_widgets_props_impl();
    }

    fn handler_action_start_dither_calibr(&self) {
        exec_and_show_error(Some(&self.window), || {
            self.engine.exec_command(Command::Start(StartMode::MountCalibr))?;
            Ok(())
        });
    }

    fn handler_action_stop_dither_calibr(&self) {
        log_if_error(
            &self.engine.exec_command(Command::StopActiveMode),
            "Stop active mode"
        );
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
                // Guider state from the snapshot (refreshed before fan-out)
                if let Some(state) = self.main_ui.snapshot().devices.guider {
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
