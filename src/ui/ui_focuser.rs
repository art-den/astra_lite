use std::{cell::{Cell, RefCell}, rc::Rc, sync::{Arc, RwLock}};
use gtk::{glib, gdk, prelude::*, glib::clone};
use serde::{Deserialize, Serialize};

use crate::{
    core::{core::{Core, CoreEvent, ModeType}, mode_focusing::*},
    indi,
    options::*,
    ui::{gtk_utils::DEFAULT_DPMM, plots::*},
    utils::{io_utils::*, math::{cmp_f64, linear_interpolate}},
};

use super::{gtk_utils, ui_common::*, ui_main::*};

pub fn init_ui(
    _app:     &gtk::Application,
    builder:  &gtk::Builder,
    main_ui:  &Rc<MainUi>,
    options:  &Arc<RwLock<Options>>,
    core:     &Arc<Core>,
    indi:     &Arc<indi::Connection>,
    excl:     &Rc<ExclusiveCaller>,
    handlers: &mut MainUiHandlers,
) {
    let window = builder.object::<gtk::ApplicationWindow>("window").unwrap();

    let mut ui_options = UiOptions::default();
    gtk_utils::exec_and_show_error(&window, || {
        load_json_from_config_file(&mut ui_options, FocuserUi::CONF_FN)?;
        Ok(())
    });

    let data = Rc::new(FocuserUi {
        main_ui:         Rc::clone(main_ui),
        builder:         builder.clone(),
        window,
        options:         Arc::clone(options),
        core:            Arc::clone(core),
        indi:            Arc::clone(indi),
        excl:            Rc::clone(excl),
        ui_options:      RefCell::new(ui_options),
        closed:          Cell::new(false),
        indi_evt_conn:   RefCell::new(None),
        delayed_actions: DelayedActions::new(500),
        focusing_data:   RefCell::new(None),
        self_:           RefCell::new(None),
    });

    *data.self_.borrow_mut() = Some(Rc::clone(&data));

    data.init_focuser_widgets();
    data.connect_indi_and_core_events();
    data.connect_widgets_events();
    data.update_devices_list();
    data.apply_ui_options();
    data.correct_widgets_props();

    handlers.push(Box::new(clone!(@weak data => move |event| {
        match event {
            MainUiEvent::ProgramClosing =>
                data.handler_closing(),
            _ => {},
        }

    })));

    data.delayed_actions.set_event_handler(
        clone!(@weak data => move |action| {
            data.handler_delayed_action(action);
        })
    );
}

struct FocuserUi {
    main_ui:         Rc<MainUi>,
    builder:         gtk::Builder,
    window:          gtk::ApplicationWindow,
    options:         Arc<RwLock<Options>>,
    core:            Arc<Core>,
    indi:            Arc<indi::Connection>,
    excl:            Rc<ExclusiveCaller>,
    ui_options:      RefCell<UiOptions>,
    closed:          Cell<bool>,
    indi_evt_conn:   RefCell<Option<indi::Subscription>>,
    delayed_actions: DelayedActions<DelayedActionTypes>,
    focusing_data:   RefCell<Option<FocusingResultData>>,
    self_:           RefCell<Option<Rc<FocuserUi>>>,
}

impl Drop for FocuserUi {
    fn drop(&mut self) {
        log::info!("FocuserUi dropped");
    }
}

enum MainThreadEvent {
    Core(CoreEvent),
    Indi(indi::Event),
}

#[derive(Hash, Eq, PartialEq)]
enum DelayedActionTypes {
    UpdateFocList,
    UpdateFocPosNew,
    UpdateFocPos,
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(default)]
struct UiOptions {
    expanded: bool,
}

impl Default for UiOptions {
    fn default() -> Self {
        Self {
            expanded: false,
        }
    }
}

impl FocuserUi {
    const CONF_FN: &'static str = "ui_focuser";

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
            MainThreadEvent::Indi(indi::Event::ConnChange(conn_state)) =>
                self.process_indi_conn_state_event(conn_state),
            MainThreadEvent::Indi(indi::Event::PropChange(event_data)) => {
                match &event_data.change {
                    indi::PropChange::New(value) =>
                        self.process_indi_prop_change(
                            &event_data.device_name,
                            &event_data.prop_name,
                            &value.elem_name,
                            true,
                            None,
                            None,
                            &value.prop_value
                        ),
                    indi::PropChange::Change{ value, prev_state, new_state } =>
                        self.process_indi_prop_change(
                            &event_data.device_name,
                            &event_data.prop_name,
                            &value.elem_name,
                            false,
                            Some(prev_state),
                            Some(new_state),
                            &value.prop_value
                        ),
                    indi::PropChange::Delete => {}
                };
            }

            MainThreadEvent::Indi(indi::Event::DeviceDelete(event)) => {
                self.update_devices_list_and_props_by_drv_interface(event.drv_interface);
            }

            MainThreadEvent::Core(CoreEvent::ModeChanged) => {
                self.correct_widgets_props();
            }

            MainThreadEvent::Core(CoreEvent::Focusing(FocusingStateEvent::Data(fdata))) => {
                *self.focusing_data.borrow_mut() = Some(fdata);
                let da_focusing = self.builder.object::<gtk::DrawingArea>("da_focusing").unwrap();
                da_focusing.queue_draw();
            }

            MainThreadEvent::Core(CoreEvent::Focusing(FocusingStateEvent::Result { value })) => {
                self.excl.exec(|| {
                    self.update_focuser_position_after_focusing(value);
                });
            }

            _ => {}
        }
    }

    fn process_indi_prop_change(
        &self,
        _device_name: &str,
        prop_name:    &str,
        elem_name:    &str,
        new_prop:     bool,
        _prev_state:  Option<&indi::PropState>,
        _new_state:   Option<&indi::PropState>,
        value:        &indi::PropValue,
    ) {
        match (prop_name, elem_name, value) {
            ("DRIVER_INFO", "DRIVER_INTERFACE", _) => {
                let flag_bits = value.to_i32().unwrap_or(0);
                let flags = indi::DriverInterface::from_bits_truncate(flag_bits as u32);
                if flags.contains(indi::DriverInterface::FOCUSER) {
                    self.delayed_actions.schedule(DelayedActionTypes::UpdateFocList);
                }
            }
            ("ABS_FOCUS_POSITION", ..) => {
                self.show_cur_focuser_value();
                self.delayed_actions.schedule(
                    if new_prop { DelayedActionTypes::UpdateFocPosNew }
                    else        { DelayedActionTypes::UpdateFocPos }
                );
            }
            ("FOCUS_MAX", ..) => {
                self.delayed_actions.schedule(DelayedActionTypes::UpdateFocPosNew);
            }
            _ => {}
        }
    }

    fn update_devices_list_and_props_by_drv_interface(
        &self,
        drv_interface: indi::DriverInterface,
    ) {
        if drv_interface.contains(indi::DriverInterface::FOCUSER) {
            self.delayed_actions.schedule(DelayedActionTypes::UpdateFocList);
        }
    }

    fn process_indi_conn_state_event(
        &self,
        conn_state: indi::ConnState
    ) {
        let update_devices_list =
            conn_state == indi::ConnState::Disconnected ||
            conn_state == indi::ConnState::Disconnecting;
        if update_devices_list {
            self.excl.exec(|| {
                self.update_devices_list();
            });
        }
        self.correct_widgets_props();
    }

    fn init_focuser_widgets(&self) {
        let spb_foc_temp = self.builder.object::<gtk::SpinButton>("spb_foc_temp").unwrap();
        spb_foc_temp.set_range(1.0, 20.0);
        spb_foc_temp.set_digits(0);
        spb_foc_temp.set_increments(1.0, 5.0);

        let spb_foc_measures = self.builder.object::<gtk::SpinButton>("spb_foc_measures").unwrap();
        spb_foc_measures.set_range(7.0, 42.0);
        spb_foc_measures.set_digits(0);
        spb_foc_measures.set_increments(1.0, 10.0);

        let spb_foc_auto_step = self.builder.object::<gtk::SpinButton>("spb_foc_auto_step").unwrap();
        spb_foc_auto_step.set_range(1.0, 1_000_000.0);
        spb_foc_auto_step.set_digits(0);
        spb_foc_auto_step.set_increments(100.0, 1000.0);

        let spb_foc_exp = self.builder.object::<gtk::SpinButton>("spb_foc_exp").unwrap();
        spb_foc_exp.set_range(0.5, 60.0);
        spb_foc_exp.set_digits(1);
        spb_foc_exp.set_increments(0.5, 5.0);
    }

    fn correct_widgets_props(&self) {
        let ui = gtk_utils::UiHelper::new_from_builder(&self.builder);
        let mode_data = self.core.mode_data();
        let mode_type = mode_data.mode.get_type();
        drop(mode_data);

        let waiting = mode_type == ModeType::Waiting;
        let single_shot = mode_type == ModeType::SingleShot;
        let focusing = mode_type == ModeType::Focusing;
        let can_change_mode = waiting || single_shot;

        let device = ui.prop_string("cb_foc_list.active-id");
        let device_enabled = self.indi
            .is_device_enabled(device.as_deref().unwrap_or(""))
            .unwrap_or(false);

        ui.enable_widgets(false, &[
            ("grd_foc",       device_enabled && (waiting || focusing || single_shot)),
            ("spb_foc_temp",  ui.prop_bool("chb_foc_temp.active")),
            ("cb_foc_fwhm",   ui.prop_bool("chb_foc_fwhm.active")),
            ("cb_foc_period", ui.prop_bool("chb_foc_period.active")),
        ]);

        gtk_utils::enable_actions(&self.window, &[
            ("manual_focus",      !focusing && can_change_mode),
            ("stop_manual_focus", focusing),
        ]);
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

    fn update_devices_list(&self) {
        let ui = gtk_utils::UiHelper::new_from_builder(&self.builder);
        let dev_list = self.indi.get_devices_list();
        let focusers = dev_list
            .iter()
            .filter(|device|
                device.interface.contains(indi::DriverInterface::FOCUSER)
            );
        let cb_foc_list: gtk::ComboBoxText =
            self.builder.object("cb_foc_list").unwrap();
        let last_active_id = cb_foc_list.active_id().map(|s| s.to_string());
        cb_foc_list.remove_all();
        for camera in focusers {
            cb_foc_list.append(Some(&camera.name), &camera.name);
        }
        let focusers_count = gtk_utils::combobox_items_count(&cb_foc_list);
        if focusers_count == 1 {
            cb_foc_list.set_active(Some(0));
        } else if focusers_count > 1 {
            let options = self.options.read().unwrap();
            if last_active_id.is_some() {
                cb_foc_list.set_active_id(last_active_id.as_deref());
            } else if !options.focuser.device.is_empty() {
                cb_foc_list.set_active_id(Some(options.focuser.device.as_str()));
            }
            if cb_foc_list.active_id().is_none() {
                cb_foc_list.set_active(Some(0));
            }
        }
        let connected = self.indi.state() == indi::ConnState::Connected;
        ui.enable_widgets(false, &[
            ("cb_foc_list", connected && focusers_count > 1),
        ]);
        self.options.write().unwrap().focuser.device =
            cb_foc_list.active_id().map(|s| s.to_string()).unwrap_or_else(String::new);
    }

    fn update_focuser_position_widget(&self, new_prop: bool) {
        let ui = gtk_utils::UiHelper::new_from_builder(&self.builder);
        let Some(foc_device) = ui.prop_string("cb_foc_list.active-id") else {
            return;
        };
        let Ok(prop_info) = self.indi.focuser_get_abs_value_prop_info(&foc_device) else {
            return;
        };
        let spb_foc_val = self.builder.object::<gtk::SpinButton>("spb_foc_val").unwrap();
        if new_prop || spb_foc_val.value() == 0.0 {
            spb_foc_val.set_range(0.0, prop_info.max);
            spb_foc_val.set_digits(0);
            let step = prop_info.step.unwrap_or(1.0);
            spb_foc_val.set_increments(step, step * 10.0);
            let Ok(value) = self.indi.focuser_get_abs_value(&foc_device) else {
                return;
            };
            spb_foc_val.set_value(value);
        }
    }

    fn update_focuser_position_after_focusing(&self, pos: f64) {
        let spb_foc_val = self.builder.object::<gtk::SpinButton>("spb_foc_val").unwrap();
        spb_foc_val.set_value(pos);
    }

    fn handler_delayed_action(&self, action: &DelayedActionTypes) {
        match action {
            DelayedActionTypes::UpdateFocList => {
                self.excl.exec(|| {
                    self.update_devices_list();
                    self.correct_widgets_props();
                });
            }
            DelayedActionTypes::UpdateFocPosNew |
            DelayedActionTypes::UpdateFocPos => {
                self.excl.exec(|| {
                    self.update_focuser_position_widget(
                        *action == DelayedActionTypes::UpdateFocPosNew
                    );
                });
                self.show_cur_focuser_value();
            }
        }
    }

    fn show_cur_focuser_value(&self) {
        let ui = gtk_utils::UiHelper::new_from_builder(&self.builder);
        let Some(foc_device) = ui.prop_string("cb_foc_list.active-id") else {
            return;
        };
        let Ok(value) = self.indi.focuser_get_abs_value(&foc_device) else {
            return;
        };
        let l_foc_value = self.builder.object::<gtk::Label>("l_foc_value").unwrap();
        l_foc_value.set_label(&format!("{:.0}", value));
    }

    fn connect_widgets_events(self: &Rc<Self>) {
        gtk_utils::connect_action(&self.window, self, "manual_focus",      Self::handler_action_manual_focus);
        gtk_utils::connect_action(&self.window, self, "stop_manual_focus", Self::handler_action_stop_manual_focus);

        let bldr = &self.builder;
        let ui = gtk_utils::UiHelper::new_from_builder(&self.builder);
        let spb_foc_val = bldr.object::<gtk::SpinButton>("spb_foc_val").unwrap();
        spb_foc_val.connect_value_changed(clone!(@weak self as self_ => move |sb| {
            self_.excl.exec(|| {
                let Some(foc_device) = ui.prop_string("cb_foc_list.active-id") else {
                    return;
                };
                gtk_utils::exec_and_show_error(&self_.window, || {
                    self_.indi.focuser_set_abs_value(&foc_device, sb.value(), true, None)?;
                    Ok(())
                })
            });
        }));

        let chb_foc_temp = bldr.object::<gtk::CheckButton>("chb_foc_temp").unwrap();
        chb_foc_temp.connect_active_notify(clone!(@weak self as self_ => move |_| {
            self_.correct_widgets_props();
        }));

        let chb_foc_fwhm = bldr.object::<gtk::CheckButton>("chb_foc_fwhm").unwrap();
        chb_foc_fwhm.connect_active_notify(clone!(@weak self as self_ => move |_| {
            self_.correct_widgets_props();
        }));

        let chb_foc_period = bldr.object::<gtk::CheckButton>("chb_foc_period").unwrap();
        chb_foc_period.connect_active_notify(clone!(@weak self as self_ => move |_| {
            self_.correct_widgets_props();
        }));

        let da_focusing = self.builder.object::<gtk::DrawingArea>("da_focusing").unwrap();
        da_focusing.connect_draw(
            clone!(@weak self as self_ => @default-return glib::Propagation::Proceed,
            move |da, ctx| {
                _ = self_.draw_focusing_samples(da, ctx);
                glib::Propagation::Proceed
            })
        );
    }

    fn draw_focusing_samples(
        &self,
        da:   &gtk::DrawingArea,
        ctx:  &gdk::cairo::Context
    ) -> anyhow::Result<()> {
        let focusing_data = self.focusing_data.borrow();
        let Some(ref focusing_data) = *focusing_data else {
            return Ok(());
        };
        const PARABOLA_POINTS: usize = 101;
        let get_plot_points_cnt = |plot_idx: usize| {
            match plot_idx {
                0 => focusing_data.samples.len(),
                1 => if focusing_data.coeffs.is_some() { PARABOLA_POINTS } else { 0 },
                2 => if focusing_data.result.is_some() && focusing_data.coeffs.is_some() { 1 } else { 0 },
                _ => unreachable!(),
            }
        };
        let get_plot_style = |plot_idx| -> PlotLineStyle {
            match plot_idx {
                0 => PlotLineStyle {
                    line_width: 2.0,
                    line_color: gdk::RGBA::new(0.0, 0.3, 1.0, 1.0),
                    point_style: PlotPointStyle::Round(8.0),
                },
                1 => PlotLineStyle {
                    line_width: 1.0,
                    line_color: gdk::RGBA::new(0.0, 1.0, 0.0, 1.0),
                    point_style: PlotPointStyle::None,
                },
                2 => PlotLineStyle {
                    line_width: 1.0,
                    line_color: gdk::RGBA::new(0.0, 1.0, 0.0, 1.0),
                    point_style: PlotPointStyle::Round(10.0),
                },
                _ => unreachable!(),
            }
        };
        let min_pos = focusing_data.samples.iter().map(|s| s.focus_pos).min_by(cmp_f64).unwrap_or(0.0);
        let max_pos = focusing_data.samples.iter().map(|s| s.focus_pos).max_by(cmp_f64).unwrap_or(0.0);
        let get_plot_point = |plot_idx: usize, point_idx: usize| -> (f64, f64) {
            match plot_idx {
                0 => {
                    let sample = &focusing_data.samples[point_idx];
                    (sample.focus_pos, sample.stars_fwhm as f64)
                }
                1 => {
                    if let Some(coeffs) = &focusing_data.coeffs {
                        let x = linear_interpolate(
                            point_idx as f64,
                            0.0,
                            PARABOLA_POINTS as f64,
                            min_pos,
                            max_pos,
                        );
                        let y = coeffs.calc(x);
                        (x, y)
                    } else {
                        unreachable!();
                    }
                }
                2 => {
                    if let (Some(coeffs), Some(x)) = (&focusing_data.coeffs, &focusing_data.result) {
                        let y = coeffs.calc(*x);
                        (*x, y)
                    } else {
                        unreachable!();
                    }
                }
                _ => unreachable!()
            }
        };
        let mut plots = Plots {
            plot_count: 3,
            get_plot_points_cnt: Box::new(get_plot_points_cnt),
            get_plot_style: Box::new(get_plot_style),
            get_plot_point: Box::new(get_plot_point),
            area: PlotAreaStyle::default(),
            left_axis: AxisStyle::default(),
            bottom_axis: AxisStyle::default(),
        };
        plots.left_axis.dec_digits = 2;
        plots.bottom_axis.dec_digits = 0;

        let font_size_pt = 8.0;
        let (_, dpmm_y) = gtk_utils::get_widget_dpmm(da)
            .unwrap_or((DEFAULT_DPMM, DEFAULT_DPMM));
        let font_size_px = gtk_utils::font_size_to_pixels(gtk_utils::FontSize::Pt(font_size_pt), dpmm_y);
        ctx.set_font_size(font_size_px);

        draw_plots(&plots, da, ctx)?;
        Ok(())
    }

    fn handler_action_manual_focus(&self) {
        let mut options = self.options.write().unwrap();
        options.read_all(&self.builder);
        drop(options);

        gtk_utils::exec_and_show_error(&self.window, || {
            self.core.start_focusing()?;
            Ok(())
        });
    }

    fn handler_action_stop_manual_focus(&self) {
        self.core.abort_active_mode();
    }

    fn apply_ui_options(&self) {
        let ui = gtk_utils::UiHelper::new_from_builder(&self.builder);
        let options = self.ui_options.borrow();
        ui.set_prop_bool("exp_foc.expanded", options.expanded);
    }

    fn get_ui_options_from_widgets(&self) {
        let ui = gtk_utils::UiHelper::new_from_builder(&self.builder);
        let mut options = self.ui_options.borrow_mut();
        options.expanded = ui.prop_bool("exp_foc.expanded");
    }


}

