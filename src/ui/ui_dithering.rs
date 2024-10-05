use std::{cell::{Cell, RefCell}, rc::Rc, sync::{Arc, RwLock}};
use gtk::{glib, prelude::*, glib::clone};
use serde::{Deserialize, Serialize};

use crate::{
    core::core::*, indi, options::*, utils::io_utils::*
};

use super::{gtk_utils, ui_main::*};

pub fn init_ui(
    _app:     &gtk::Application,
    builder:  &gtk::Builder,
    options:  &Arc<RwLock<Options>>,
    core:     &Arc<Core>,
    indi:     &Arc<indi::Connection>,
    handlers: &mut MainUiEventHandlers,
) {
    let window = builder.object::<gtk::ApplicationWindow>("window").unwrap();

    let mut ui_options = UiOptions::default();
    gtk_utils::exec_and_show_error(&window, || {
        load_json_from_config_file(&mut ui_options, DitheringUi::CONF_FN)?;
        Ok(())
    });

    let data = Rc::new(DitheringUi {
        builder:       builder.clone(),
        window,
        options:       Arc::clone(options),
        core:          Arc::clone(core),
        indi:          Arc::clone(indi),
        ui_options:    RefCell::new(ui_options),
        closed:        Cell::new(false),
        indi_evt_conn: RefCell::new(None),
        self_:         RefCell::new(None),
    });

    *data.self_.borrow_mut() = Some(Rc::clone(&data));

    data.init_widgets();
    data.apply_ui_options();
    data.connect_main_ui_events(handlers);
    data.connect_widgets_events();
    data.connect_indi_and_core_events();
    data.correct_widgets_props();
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(default)]
struct UiOptions {
    expanded: bool,
}

impl Default for UiOptions {
    fn default() -> Self {
        Self {
            expanded: false
        }
    }
}

struct DitheringUi {
    builder:       gtk::Builder,
    window:        gtk::ApplicationWindow,
    options:       Arc<RwLock<Options>>,
    core:          Arc<Core>,
    indi:          Arc<indi::Connection>,
    ui_options:    RefCell<UiOptions>,
    closed:        Cell<bool>,
    indi_evt_conn: RefCell<Option<indi::Subscription>>,
    self_:         RefCell<Option<Rc<DitheringUi>>>,
}

impl Drop for DitheringUi {
    fn drop(&mut self) {
        log::info!("DitheringUi dropped");
    }
}

enum MainThreadEvent {
    Core(CoreEvent),
    Indi(indi::Event),
}

impl DitheringUi {
    const CONF_FN: &'static str = "ui_dithering";

    fn init_widgets(&self) {
        let spb_guid_max_err = self.builder.object::<gtk::SpinButton>("spb_guid_max_err").unwrap();
        spb_guid_max_err.set_range(3.0, 50.0);
        spb_guid_max_err.set_digits(0);
        spb_guid_max_err.set_increments(1.0, 10.0);

        let spb_mnt_cal_exp = self.builder.object::<gtk::SpinButton>("spb_mnt_cal_exp").unwrap();
        spb_mnt_cal_exp.set_range(0.5, 10.0);
        spb_mnt_cal_exp.set_digits(1);
        spb_mnt_cal_exp.set_increments(0.5, 5.0);

        let sb_dith_dist = self.builder.object::<gtk::SpinButton>("sb_dith_dist").unwrap();
        sb_dith_dist.set_range(1.0, 300.0);
        sb_dith_dist.set_digits(0);
        sb_dith_dist.set_increments(1.0, 10.0);

        let sb_ext_dith_dist = self.builder.object::<gtk::SpinButton>("sb_ext_dith_dist").unwrap();
        sb_ext_dith_dist.set_range(1.0, 300.0);
        sb_ext_dith_dist.set_digits(0);
        sb_ext_dith_dist.set_increments(1.0, 10.0);
    }

    fn connect_indi_and_core_events(self: &Rc<Self>) {
        let (main_thread_sender, main_thread_receiver) = async_channel::unbounded();

        let sender = main_thread_sender.clone();
        self.core.subscribe_events(move |event| {
            sender.send_blocking(MainThreadEvent::Core(event)).unwrap();
        });

        let sender = main_thread_sender.clone();
        *self.indi_evt_conn.borrow_mut() = Some(self.indi.subscribe_events(move |event| {
            sender.send_blocking(MainThreadEvent::Indi(event)).unwrap();
        }));

        glib::spawn_future_local(clone!(@weak self as self_ => async move {
            while let Ok(event) = main_thread_receiver.recv().await {
                if self_.closed.get() { return; }
                self_.process_event_in_main_thread(event);
            }
        }));
    }

    fn process_event_in_main_thread(&self, event: MainThreadEvent) {
        match event {
            MainThreadEvent::Core(CoreEvent::ModeChanged) => {
                self.correct_widgets_props();
            }
            MainThreadEvent::Indi(indi::Event::ConnChange(_)) => {
                self.correct_widgets_props();
            }
            _ => {}
        }
    }

    fn connect_widgets_events(self: &Rc<Self>) {
        gtk_utils::connect_action(&self.window, self, "start_dither_calibr", Self::handler_action_start_dither_calibr);
        gtk_utils::connect_action(&self.window, self, "stop_dither_calibr",  Self::handler_action_stop_dither_calibr);

        let connect_rbtn = |name: &str| {
            let rbtn = self.builder.object::<gtk::RadioButton>(name).unwrap();
            let self_ = Rc::clone(self);
            rbtn.connect_active_notify(move |_| {
                self_.correct_widgets_props();
            });
        };

        connect_rbtn("rbtn_no_guiding");
        connect_rbtn("rbtn_guide_main_cam");
        connect_rbtn("rbtn_guide_ext");
    }

    fn connect_main_ui_events(self: &Rc<Self>, handlers: &mut MainUiEventHandlers) {
        handlers.subscribe(clone!(@weak self as self_ => move |event| {
            match event {
                MainUiEvent::ProgramClosing =>
                    self_.handler_closing(),
                _ => {},
            }
        }));

    }

    fn handler_closing(&self) {
        self.closed.set(true);

        self.get_ui_options_from_widgets();
        let ui_options = self.ui_options.borrow();
        _ = save_json_to_config::<UiOptions>(&ui_options, Self::CONF_FN);
        drop(ui_options);

        if let Some(indi_conn) = self.indi_evt_conn.borrow_mut().take() {
            self.indi.unsubscribe(indi_conn);
        }

        *self.self_.borrow_mut() = None;
    }

    fn correct_widgets_props(&self) {
        let ui = gtk_utils::UiHelper::new_from_builder(&self.builder);

        let mode_data = self.core.mode_data();
        let mode_type = mode_data.mode.get_type();
        drop(mode_data);
        let can_change_mode =
            mode_type == ModeType::Waiting ||
            mode_type == ModeType::SingleShot ||
            mode_type == ModeType::LiveView;
        let dither_calibr = mode_type == ModeType::DitherCalibr;
        let indi_connected = self.indi.state() == indi::ConnState::Connected;

        let disabled = ui.prop_bool("rbtn_no_guiding.active");
        let by_main_cam = ui.prop_bool("rbtn_guide_main_cam.active");
        let by_ext = ui.prop_bool("rbtn_guide_ext.active");

        ui.enable_widgets(false, &[
            ("grd_dither",          indi_connected),
            ("rbtn_no_guiding",     can_change_mode),
            ("rbtn_guide_main_cam", can_change_mode),
            ("rbtn_guide_ext",      can_change_mode),
            ("cb_dith_perod",       !disabled && can_change_mode),
            ("sb_dith_dist",        by_main_cam && can_change_mode),
            ("spb_guid_max_err",    by_main_cam && can_change_mode),
            ("spb_mnt_cal_exp",     by_main_cam && can_change_mode),
            ("sb_ext_dith_dist",    by_ext && can_change_mode),
        ]);

        gtk_utils::enable_actions(&self.window, &[
            ("start_dither_calibr", !dither_calibr && by_main_cam && can_change_mode),
            ("stop_dither_calibr", dither_calibr),
        ]);
    }

    fn apply_ui_options(&self) {
        let ui = gtk_utils::UiHelper::new_from_builder(&self.builder);
        let options = self.ui_options.borrow();
        ui.set_prop_bool("exp_dith.expanded", options.expanded);
    }

    fn get_ui_options_from_widgets(&self) {
        let ui = gtk_utils::UiHelper::new_from_builder(&self.builder);
        let mut options = self.ui_options.borrow_mut();
        options.expanded = ui.prop_bool("exp_dith.expanded");
    }

    fn handler_action_start_dither_calibr(&self) {
        let mut options = self.options.write().unwrap();
        options.read_all(&self.builder);
        drop(options);

        gtk_utils::exec_and_show_error(&self.window, || {
            self.core.start_mount_calibr()?;
            Ok(())
        });
    }

    fn handler_action_stop_dither_calibr(&self) {
        self.core.abort_active_mode();
    }
}
