use std::{cell::{Cell, RefCell}, rc::Rc, sync::{Arc, RwLock}};
use gtk::{glib, gdk, prelude::*, glib::clone};
use macros::FromBuilder;

use crate::{
    core::{core::{Core, ModeType}, events::*, mode_focusing::*},
    indi,
    options::*,
    ui::plots::*,
    utils::{gtk_utils::{self, *}, math::{cmp_f64, linear_interpolate}},
};

use super::{ui_main::*, utils::*, module::*};

pub fn init_ui(
    window:  &gtk::ApplicationWindow,
    main_ui: &Rc<MainUi>,
    options: &Arc<RwLock<Options>>,
    core:    &Arc<Core>,
    indi:    &Arc<indi::Connection>,
) -> Rc<dyn UiModule> {
    let widgets = Widgets::from_builder_str(include_str!(r"resources/focuser.ui"));

    let obj = Rc::new(FocuserUi {
        widgets,
        main_ui:         Rc::clone(main_ui),
        window:          window.clone(),
        options:         Arc::clone(options),
        core:            Arc::clone(core),
        indi:            Arc::clone(indi),
        closed:          Cell::new(false),
        excl:            ExclusiveCaller::new(),
        indi_evt_conn:   RefCell::new(None),
        delayed_actions: DelayedActions::new(500),
        focusing_data:   RefCell::new(None),
    });

    obj.init_widgets();
    obj.update_devices_list();

    obj.connect_indi_and_core_events();
    obj.connect_widgets_events();
    obj.connect_delayed_actions_events();

    obj
}

#[derive(FromBuilder)]
struct Widgets {
    bx:            gtk::Box,
    grd:           gtk::Grid,
    cb_list:       gtk::ComboBoxText,
    l_value:       gtk::Label,
    spb_val:       gtk::SpinButton,
    chb_temp:      gtk::CheckButton,
    spb_temp:      gtk::SpinButton,
    chb_fwhm:      gtk::CheckButton,
    cb_fwhm:       gtk::ComboBoxText,
    chb_period:    gtk::CheckButton,
    cb_period:     gtk::ComboBoxText,
    spb_measures:  gtk::SpinButton,
    spb_auto_step: gtk::SpinButton,
    spb_exp:       gtk::SpinButton,
    cbx_gain:      gtk::ComboBoxText,
    da_auto:       gtk::DrawingArea,
}

struct FocuserUi {
    widgets:         Widgets,
    main_ui:         Rc<MainUi>,
    window:          gtk::ApplicationWindow,
    options:         Arc<RwLock<Options>>,
    core:            Arc<Core>,
    indi:            Arc<indi::Connection>,
    closed:          Cell<bool>,
    excl:            ExclusiveCaller,
    indi_evt_conn:   RefCell<Option<indi::Subscription>>,
    delayed_actions: DelayedActions<DelayedAction>,
    focusing_data:   RefCell<Option<FocusingResultData>>,
}

impl Drop for FocuserUi {
    fn drop(&mut self) {
        log::info!("FocuserUi dropped");
    }
}

impl UiModule for FocuserUi {
    fn show_options(&self, options: &Options) {
        let widgets = &self.widgets;
        widgets.chb_temp     .set_active   (options.focuser.on_temp_change);
        widgets.spb_temp     .set_value    (options.focuser.max_temp_change);
        widgets.chb_fwhm     .set_active   (options.focuser.on_fwhm_change);
        widgets.cb_fwhm      .set_active_id(Some(options.focuser.max_fwhm_change.to_string()).as_deref());
        widgets.chb_period   .set_active   (options.focuser.periodically);
        widgets.cb_period    .set_active_id(Some(options.focuser.period_minutes.to_string()).as_deref());
        widgets.spb_measures .set_value    (options.focuser.measures as f64);
        widgets.spb_auto_step.set_value    (options.focuser.step);
        widgets.spb_exp      .set_value    (options.focuser.exposure);
        widgets.cbx_gain     .set_active_id(Some(options.focuser.gain.to_active_id()));
    }

    fn get_options(&self, options: &mut Options) {
        let widgets = &self.widgets;
        options.focuser.on_temp_change  = widgets.chb_temp.is_active();
        options.focuser.max_temp_change = widgets.spb_temp.value();
        options.focuser.on_fwhm_change  = widgets.chb_fwhm.is_active();
        options.focuser.max_fwhm_change = widgets.cb_fwhm.active_id().and_then(|v| v.parse().ok()).unwrap_or(20);
        options.focuser.periodically    = widgets.chb_period.is_active();
        options.focuser.period_minutes  = widgets.cb_period.active_id().and_then(|v| v.parse().ok()).unwrap_or(120);
        options.focuser.measures        = widgets.spb_measures.value() as u32;
        options.focuser.step            = widgets.spb_auto_step.value();
        options.focuser.exposure        = widgets.spb_exp.value();
        options.focuser.gain            = Gain::from_active_id(widgets.cbx_gain.active_id().as_deref());
    }

    fn panels(&self) -> Vec<Panel> {
        vec![Panel {
            str_id: "focuser",
            name:   "Focuser".to_string(),
            widget: self.widgets.bx.clone().upcast(),
            pos:    PanelPosition::Right,
            tab:    PanelTab::Common,
            flags:  PanelFlags::empty(),
        }]
    }

    fn process_event(&self, event: &UiModuleEvent) {
        match event {
            UiModuleEvent::AfterFirstShowOptions => {
                self.correct_widgets_props();
            }
            UiModuleEvent::ProgramClosing => {
                self.handler_closing();
            }
            _ => {}
        }
    }
}

enum MainThreadEvent {
    Core(Event),
    Indi(indi::Event),
}

#[derive(Hash, Eq, PartialEq)]
enum DelayedAction {
    ShowCurFocuserValue,
    UpdateFocPosNew,
    UpdateFocPos,
    CorrectWidgetProps
}

impl FocuserUi {
    fn connect_indi_and_core_events(self: &Rc<Self>) {
        let (main_thread_sender, main_thread_receiver) = async_channel::unbounded();
        let sender = main_thread_sender.clone();
        self.core.event_subscriptions().subscribe(move |event| {
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
            MainThreadEvent::Indi(indi::Event::NewDevice(event)) =>
                if event.interface.contains(indi::DriverInterface::FOCUSER) {
                    self.update_devices_list();
                },

            MainThreadEvent::Indi(indi::Event::DeviceConnected(event)) =>
                if event.interface.contains(indi::DriverInterface::FOCUSER) {
                    self.delayed_actions.schedule(DelayedAction::CorrectWidgetProps);
                },

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
                if event.drv_interface.contains(indi::DriverInterface::FOCUSER) {
                    self.update_devices_list();
                    self.delayed_actions.schedule(DelayedAction::CorrectWidgetProps);
                    self.delayed_actions.schedule(DelayedAction::UpdateFocPosNew);
                    self.delayed_actions.schedule(DelayedAction::ShowCurFocuserValue);
                }
            }

            MainThreadEvent::Core(Event::ModeChanged) => {
                self.correct_widgets_props();
            }

            MainThreadEvent::Core(Event::CameraDeviceChanged(cam_device)) => {
                let options = self.options.read().unwrap();
                let focuser_device = options.focuser.device.clone();
                drop(options);
                self.correct_widgets_props_impl(&focuser_device, &Some(cam_device));
            }

            MainThreadEvent::Core(Event::Focusing(FocusingStateEvent::Data(fdata))) => {
                *self.focusing_data.borrow_mut() = Some(fdata);
                self.widgets.da_auto.queue_draw();
            }

            MainThreadEvent::Core(Event::Focusing(FocusingStateEvent::Result { value })) => {
                self.update_focuser_position_after_focusing(value);
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
            ("ABS_FOCUS_POSITION", ..) => {
                self.delayed_actions.schedule(
                    if new_prop { DelayedAction::UpdateFocPosNew }
                    else        { DelayedAction::UpdateFocPos }
                );
                self.delayed_actions.schedule(DelayedAction::ShowCurFocuserValue);
            }
            ("FOCUS_MAX", ..) => {
                self.delayed_actions.schedule(DelayedAction::UpdateFocPosNew);
            }
            _ => {}
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
            self.update_devices_list();
        }
        self.delayed_actions.schedule(DelayedAction::CorrectWidgetProps);
    }

    fn init_widgets(&self) {
        self.widgets.spb_temp.set_range(1.0, 20.0);
        self.widgets.spb_temp.set_digits(0);
        self.widgets.spb_temp.set_increments(1.0, 5.0);

        self.widgets.spb_measures.set_range(7.0, 42.0);
        self.widgets.spb_measures.set_digits(0);
        self.widgets.spb_measures.set_increments(1.0, 10.0);

        self.widgets.spb_auto_step.set_range(1.0, 1_000_000.0);
        self.widgets.spb_auto_step.set_digits(0);
        self.widgets.spb_auto_step.set_increments(100.0, 1000.0);

        self.widgets.spb_exp.set_range(0.1, 60.0);
        self.widgets.spb_exp.set_digits(1);
        self.widgets.spb_exp.set_increments(0.1, 1.0);
    }

    fn connect_widgets_events(self: &Rc<Self>) {
        gtk_utils::connect_action(&self.window, self, "manual_focus",      Self::handler_action_manual_focus);
        gtk_utils::connect_action(&self.window, self, "stop_manual_focus", Self::handler_action_stop_manual_focus);

        self.widgets.spb_val.connect_value_changed(clone!(@weak self as self_ => move |sb| {
            self_.excl.exec(|| {
                let options = self_.options.read().unwrap();
                if options.focuser.device.is_empty() { return; }

                gtk_utils::exec_and_show_error(&self_.window, || {
                    self_.indi.focuser_set_abs_value(&options.focuser.device, sb.value(), true, None)?;
                    Ok(())
                });
            });
        }));

        self.widgets.cb_list.connect_active_notify(clone!(@weak self as self_ => move |cb| {
            let Some(cur_id) = cb.active_id() else { return; };
            let Ok(mut options) = self_.options.try_write() else { return; };
            if options.focuser.device == cur_id.as_str() { return; }
            options.focuser.device = cur_id.to_string();
            drop(options);
            self_.delayed_actions.schedule(DelayedAction::UpdateFocPosNew);
            self_.delayed_actions.schedule(DelayedAction::ShowCurFocuserValue);
            self_.delayed_actions.schedule(DelayedAction::CorrectWidgetProps);
        }));

        self.widgets.chb_temp.connect_active_notify(clone!(@weak self as self_ => move |_| {
            self_.correct_widgets_props();
        }));

        self.widgets.chb_fwhm.connect_active_notify(clone!(@weak self as self_ => move |_| {
            self_.correct_widgets_props();
        }));

        self.widgets.chb_period.connect_active_notify(clone!(@weak self as self_ => move |_| {
            self_.correct_widgets_props();
        }));

        self.widgets.da_auto.connect_draw(
            clone!(@weak self as self_ => @default-return glib::Propagation::Proceed,
            move |da, ctx| {
                _ = self_.draw_focusing_samples(da, ctx);
                glib::Propagation::Proceed
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

    fn correct_widgets_props_impl(&self, focuser_device: &str, cam_device: &Option<DeviceAndProp>) {
        let mode_data = self.core.mode_data();
        let mode_type = mode_data.mode.get_type();
        drop(mode_data);

        if let Some(cam_device) = cam_device {
            let cam_ccd = indi::CamCcd::from_ccd_prop_name(&cam_device.prop);
            let exp_value = self.indi.camera_get_exposure_prop_value(&cam_device.name, cam_ccd);
            correct_spinbutton_by_cam_prop(&self.widgets.spb_exp, &exp_value, 1, Some(1.0));
        }

        let waiting = mode_type == ModeType::Waiting;
        let single_shot = mode_type == ModeType::SingleShot;
        let focusing = mode_type == ModeType::Focusing;
        let can_change_mode = waiting || single_shot;

        let device_enabled = self.indi.is_device_enabled(focuser_device).unwrap_or(false);

        self.widgets.grd.set_sensitive(device_enabled);
        self.widgets.spb_temp.set_sensitive(self.widgets.chb_temp.is_active());
        self.widgets.cb_fwhm.set_sensitive(self.widgets.chb_fwhm.is_active());
        self.widgets.cb_period.set_sensitive(self.widgets.chb_period.is_active());
        self.widgets.spb_val.set_sensitive(!focusing);
        self.widgets.cb_list.set_sensitive(!focusing);

        gtk_utils::enable_actions(&self.window, &[
            ("manual_focus",      !focusing && can_change_mode),
            ("stop_manual_focus", focusing),
        ]);
    }

    fn correct_widgets_props(&self) {
        let options = self.options.read().unwrap();
        let focuser_device = options.focuser.device.clone();
        let cam_device = options.cam.device.clone();
        drop(options);
        self.correct_widgets_props_impl(&focuser_device, &cam_device);
    }

    fn handler_closing(&self) {
        self.closed.set(true);

        if let Some(indi_conn) = self.indi_evt_conn.borrow_mut().take() {
            self.indi.unsubscribe(indi_conn);
        }
    }

    fn update_devices_list(&self) {
        let options = self.options.read().unwrap();
        let cur_focuser = options.focuser.device.clone();
        drop(options);

        let list = self.indi
            .get_devices_list_by_interface(indi::DriverInterface::FOCUSER)
            .iter()
            .map(|dev| dev.name.to_string())
            .collect();

        let connected = self.indi.state() == indi::ConnState::Connected;

        fill_devices_list_into_combobox(
            &list,
            &self.widgets.cb_list,
            if !cur_focuser.is_empty() { Some(cur_focuser.as_str()) } else { None },
            connected,
            |id| {
                let mut options = self.options.write().unwrap();
                options.focuser.device = id.to_string();
            }
        );
    }

    fn update_focuser_position_widget(&self, new_prop: bool) {
        let options = self.options.read().unwrap();
        let foc_device = options.focuser.device.clone();
        drop(options);

        let Ok(prop_info) = self.indi.focuser_get_abs_value_prop_info(&foc_device) else {
            return;
        };
        if new_prop || self.widgets.spb_val.value() == 0.0 {
            self.widgets.spb_val.set_range(0.0, prop_info.max);
            self.widgets.spb_val.set_digits(0);
            let mut step = prop_info.step.unwrap_or(1.0);
            let range = f64::abs(prop_info.max - prop_info.min);
            if step > range / 10.0 {
                step = 10.0;
            }
            self.widgets.spb_val.set_increments(step, step * 10.0);
            self.excl.exec(|| {
                self.widgets.spb_val.set_value(prop_info.value);
            });
        }
    }

    fn update_focuser_position_after_focusing(&self, pos: f64) {
        self.excl.exec(|| {
            self.widgets.spb_val.set_value(pos);
        });
    }

    fn handler_delayed_action(&self, action: &DelayedAction) {
        match action {
            DelayedAction::ShowCurFocuserValue => {
                self.show_cur_focuser_value();
            }
            DelayedAction::CorrectWidgetProps => {
                self.correct_widgets_props();
            }
            DelayedAction::UpdateFocPosNew |
            DelayedAction::UpdateFocPos => {
                self.update_focuser_position_widget(
                    *action == DelayedAction::UpdateFocPosNew
                );
            }
        }
    }

    fn show_cur_focuser_value(&self) {
        let options = self.options.read().unwrap();
        let foc_device = options.focuser.device.clone();
        drop(options);

        if let Ok(value) = self.indi.focuser_get_abs_value(&foc_device) {
            self.widgets.l_value.set_label(&format!("{:.0}", value));
        } else {
            self.widgets.l_value.set_label("---");
        }
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
        self.main_ui.get_all_options();

        gtk_utils::exec_and_show_error(&self.window, || {
            self.core.start_focusing()?;
            Ok(())
        });
    }

    fn handler_action_stop_manual_focus(&self) {
        self.core.abort_active_mode();
    }
}
