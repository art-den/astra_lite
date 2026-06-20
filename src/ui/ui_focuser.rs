use core::f64;
use std::{cell::{Cell, RefCell}, rc::Rc, sync::Arc};
use gtk::{glib, gdk, prelude::*, glib::clone};
use macros::FromBuilder;

use crate::{
    core::{core::{Core, ModeType}, events::*, mode_focusing::*},
    hal::{DeviceType, FocuserState, HalState, events::HalEvent},
    options::*,
    ui::plots::*,
    utils::math::{cmp_f64, linear_interpolate},
};

use super::{gtk_utils::*, module::*, ui_main::*, utils::*};

pub fn init_ui(
    window:  &gtk::ApplicationWindow,
    main_ui: &Rc<MainUi>,
    core:    &Arc<Core>,
) -> Rc<dyn UiModule> {
    let widgets = Widgets::from_builder_str(include_str!(r"resources/focuser.ui"));
    let info_widgets = InfoWidgets::new();

    let obj = Rc::new(FocuserUi {
        widgets,
        info_widgets,
        main_ui:         Rc::clone(main_ui),
        window:          window.clone(),
        core:            Arc::clone(core),
        excl:            ExclusiveCaller::new(),
        delayed_actions: DelayedActions::new(500),
        focusing_data:   RefCell::new(None),
        starting_temp:   Cell::new(None),
        step:            Cell::new(10),
        step_large:      Cell::new(100),
        prev_pos_state:  Cell::new(None),
    });

    obj.init_widgets();
    obj.update_devices_list();

    obj.connect_widgets_events();
    obj.connect_delayed_actions_events();

    obj
}

#[derive(FromBuilder)]
struct Widgets {
    bx:              gtk::Box,
    grd:             gtk::Grid,
    cb_list:         gtk::ComboBoxText,
    l_value:         gtk::Label,
    l_temp:          gtk::Label,
    l_temp_diff:     gtk::Label,
    spb_val:         gtk::SpinButton,
    bx_ctrl_btns:    gtk::Box,
    btn_dec_large:   gtk::Button,
    btn_dec:         gtk::Button,
    btn_inc:         gtk::Button,
    btn_inc_large:   gtk::Button,
    chb_temp:        gtk::CheckButton,
    spb_temp:        gtk::SpinButton,
    chb_fwhm:        gtk::CheckButton,
    cb_fwhm:         gtk::ComboBoxText,
    chb_period:      gtk::CheckButton,
    cb_period:       gtk::ComboBoxText,
    spb_measures:    gtk::SpinButton,
    spb_auto_step:   gtk::SpinButton,
    spb_exp:         gtk::SpinButton,
    spb_extra_steps: gtk::SpinButton,
    cbx_gain:        gtk::ComboBoxText,
    da_auto:         gtk::DrawingArea,
}

struct InfoWidgets {
    bx: gtk::Box,
    l_state: gtk::Label,
}

impl InfoWidgets {
    fn new() -> Self {
        let l_state = gtk::Label::builder()
            .visible(true)
            .label("State")
            .use_markup(true)
            .xalign(0.0)
            .halign(gtk::Align::Start)
            .width_chars(10)
            .build();
        let bx = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(5)
            .visible(true)
            .build();
        bx.add(&l_state);
        Self { bx, l_state }
    }
}

struct FocuserUi {
    widgets:         Widgets,
    info_widgets:    InfoWidgets,
    main_ui:         Rc<MainUi>,
    window:          gtk::ApplicationWindow,
    core:            Arc<Core>,
    excl:            ExclusiveCaller,
    delayed_actions: DelayedActions<DelayedAction>,
    focusing_data:   RefCell<Option<FocusingResultData>>,
    starting_temp:   Cell<Option<f64>>,
    step:            Cell<i32>,
    step_large:      Cell<i32>,
    prev_pos_state:  Cell<Option<FocuserState>>,
}

#[derive(Hash, Eq, PartialEq)]
enum DelayedAction {
    ShowCurFocuserTemperature(Option<i32>),
    InitAndShowFocuserValue(Option<i32>),
    ShowFocuserValue(Option<i32>),
    CorrectWidgetProps
}

impl Drop for FocuserUi {
    fn drop(&mut self) {
        log::info!("FocuserUi dropped");
    }
}

impl UiModule for FocuserUi {
    fn show_options(&self, options: &Options) {
        let widgets = &self.widgets;
        widgets.chb_temp       .set_active   (options.focuser.on_temp_change);
        widgets.spb_temp       .set_value    (options.focuser.max_temp_change);
        widgets.chb_fwhm       .set_active   (options.focuser.on_fwhm_change);
        widgets.cb_fwhm        .set_active_id(Some(options.focuser.max_fwhm_change.to_string()).as_deref());
        widgets.chb_period     .set_active   (options.focuser.periodically);
        widgets.cb_period      .set_active_id(Some(options.focuser.period_minutes.to_string()).as_deref());
        widgets.spb_measures   .set_value    (options.focuser.measures as f64);
        widgets.spb_auto_step  .set_value    (options.focuser.step);
        widgets.cbx_gain       .set_active_id(Some(options.focuser.gain.to_active_id()));
        widgets.spb_extra_steps.set_value    (options.focuser.anti_backlash_steps as f64);

        set_spb_value(&widgets.spb_exp, options.focuser.exposure);
    }

    fn get_options(&self, options: &mut Options) {
        let widgets = &self.widgets;
        options.focuser.on_temp_change      = widgets.chb_temp.is_active();
        options.focuser.max_temp_change     = widgets.spb_temp.value();
        options.focuser.on_fwhm_change      = widgets.chb_fwhm.is_active();
        options.focuser.max_fwhm_change     = widgets.cb_fwhm.active_id().and_then(|v| v.parse().ok()).unwrap_or(20);
        options.focuser.periodically        = widgets.chb_period.is_active();
        options.focuser.period_minutes      = widgets.cb_period.active_id().and_then(|v| v.parse().ok()).unwrap_or(120);
        options.focuser.measures            = widgets.spb_measures.value() as u32;
        options.focuser.step                = widgets.spb_auto_step.value();
        options.focuser.exposure            = widgets.spb_exp.value();
        options.focuser.gain                = Gain::from_active_id(widgets.cbx_gain.active_id().as_deref());
        options.focuser.anti_backlash_steps = widgets.spb_extra_steps.value() as usize;
    }

    fn panels(&self) -> Vec<Panel> {
        vec![
            Panel {
                str_id: "focuser",
                name:   "Focuser".to_string(),
                widget: self.widgets.bx.clone().upcast(),
                pos:    PanelPosition::Right,
                tab:    TabPage::Main,
                flags:  PanelFlags::empty(),
            },
            Panel {
                str_id: "focuser_info",
                name:   "Focuser".to_string(),
                widget: self.info_widgets.bx.clone().upcast(),
                pos:    PanelPosition::Bottom,
                tab:    TabPage::Main,
                flags:  PanelFlags::NO_EXPANDER,
            }
        ]
    }

    fn on_show_options_first_time(&self) {
        self.correct_widgets_props();
    }

    fn on_app_closing(&self) {
        let mut options = self.core.options().write().unwrap();
        let cur_cam_device = options.cam.device_id.clone();
        self.store_options_for_camera(&cur_cam_device, &mut options);
        drop(options);
    }

    fn on_hal_event(&self, event: &HalEvent) {
        match event {
            HalEvent::StateChanged(state) => {
                if *state == HalState::Disconnected {
                    self.update_devices_list();
                    self.delayed_actions.schedule(DelayedAction::CorrectWidgetProps);
                }
            }
            HalEvent::DeviceConnected(dev_info) => {
                if dev_info.type_.contains(DeviceType::FOCUSER) {
                    self.update_devices_list();
                    self.delayed_actions.schedule(DelayedAction::CorrectWidgetProps);
                }
            }
            HalEvent::DeviceDisconnected(dev_info) => {
                if dev_info.type_.contains(DeviceType::FOCUSER) {
                    self.update_devices_list();
                    self.delayed_actions.schedule(DelayedAction::CorrectWidgetProps);
                    self.delayed_actions.schedule(DelayedAction::ShowFocuserValue(None));
                    self.delayed_actions.schedule(DelayedAction::ShowCurFocuserTemperature(None));
                }
            }
            HalEvent::FocuserStateChanged { device_id, .. } => {
                let options = self.core.options().read().unwrap();
                if **device_id == options.focuser.device {
                    self.show_info();
                }
            }
            HalEvent::FocuserAbsValueCanBeControlled { device_id, abs_value } => {
                let options = self.core.options().read().unwrap();
                if **device_id == options.focuser.device {
                    self.delayed_actions.schedule(DelayedAction::InitAndShowFocuserValue(
                        Some(*abs_value as i32)
                    ));
                }
            }
            HalEvent::FocuserAbsValueChanged { device_id, abs_value } => {
                let options = self.core.options().read().unwrap();
                if **device_id == options.focuser.device {
                    self.delayed_actions.schedule(DelayedAction::ShowFocuserValue(
                        Some(*abs_value as i32)
                    ));
                }
            }
            HalEvent::FocuserTemperatureChanged { device_id, temperature } => {
                let options = self.core.options().read().unwrap();
                if **device_id == options.focuser.device {
                    self.delayed_actions.schedule(DelayedAction::ShowCurFocuserTemperature(
                        Some((10.0 * *temperature) as i32)
                    ));
                }
            }
            _ => {}
        }
    }

    fn on_event(&self, event: &Event) {
        match event {
            Event::ModeChanged => {
                self.correct_widgets_props();
            }
            Event::CameraDeviceChanged{prev_camera_id, new_camera_id} => {
                self.handler_camera_changed(prev_camera_id, new_camera_id);
            }
            Event::Focusing(fevent) => {
                match fevent {
                    FocuserEvent::Data(fdata) => {
                        *self.focusing_data.borrow_mut() = Some(fdata.clone());
                        self.widgets.da_auto.queue_draw();
                    }
                    FocuserEvent::Result { value } => {
                        self.update_focuser_position_after_focusing(*value);
                    }
                    FocuserEvent::StartingTemperature(starting_temp) => {
                        self.starting_temp.set(Some(*starting_temp));
                        self.delayed_actions.schedule(DelayedAction::ShowCurFocuserTemperature(None));
                    }
                }
            }
            Event::FocuserDeviceChanged(new_device_name) => {
                if self.widgets.cb_list.active_id().as_deref() != Some(new_device_name.as_str()) {
                    self.widgets.cb_list.set_active_id(Some(new_device_name.as_str()));
                }
            }
            _ => {}
        }
    }
}

impl FocuserUi {
    fn init_widgets(&self) {
        self.widgets.spb_temp.set_range(0.1, 10.0);
        self.widgets.spb_temp.set_digits(1);
        self.widgets.spb_temp.set_increments(0.1, 1.0);

        self.widgets.spb_measures.set_range(7.0, 42.0);
        self.widgets.spb_measures.set_digits(0);
        self.widgets.spb_measures.set_increments(1.0, 10.0);

        self.widgets.spb_auto_step.set_range(1.0, 1_000_000.0);
        self.widgets.spb_auto_step.set_digits(0);
        self.widgets.spb_auto_step.set_increments(100.0, 1000.0);

        self.widgets.spb_exp.set_range(0.1, 60.0);
        self.widgets.spb_exp.set_digits(1);
        self.widgets.spb_exp.set_increments(0.1, 1.0);

        self.widgets.spb_extra_steps.set_range(0.0, 10_000.0);
        self.widgets.spb_extra_steps.set_digits(0);
        self.widgets.spb_extra_steps.set_increments(25.0, 100.0);
    }

    fn connect_widgets_events(self: &Rc<Self>) {
        connect_action(&self.window, self, "manual_focus",      Self::handler_action_manual_focus);
        connect_action(&self.window, self, "stop_manual_focus", Self::handler_action_stop_manual_focus);

        self.widgets.cb_list.connect_active_notify(clone!(@weak self as self_ => move |cb| {
            let Some(new_device_name) = cb.active_id() else { return; };
            let Ok(mut options) = self_.core.options().try_write() else { return; };
            if options.focuser.device == new_device_name.as_str() { return; }
            options.focuser.device = new_device_name.to_string();
            drop(options);

            self_.core.events().send(Event::FocuserDeviceChanged(new_device_name.to_string()));

            self_.delayed_actions.schedule(DelayedAction::InitAndShowFocuserValue(None));
            self_.delayed_actions.schedule(DelayedAction::CorrectWidgetProps);
            self_.delayed_actions.schedule(DelayedAction::ShowCurFocuserTemperature(None));
        }));

        self.widgets.spb_val.connect_value_changed(clone!(@weak self as self_ => move |sb| {
            self_.excl.exec(|| {
                let Some(focuser) = self_.core.focuser() else { return; };
                exec_and_show_error(Some(&self_.window), || {
                    focuser.set_abs_position(sb.value())?;
                    Ok(())
                });
            });
        }));

        self.widgets.btn_dec_large.connect_clicked(clone!(@weak self as self_ => move |_| {
            self_.update_focuser_value(-self_.step_large.get());
        }));

        self.widgets.btn_dec.connect_clicked(clone!(@weak self as self_ => move |_| {
            self_.update_focuser_value(-self_.step.get());
        }));

        self.widgets.btn_inc.connect_clicked(clone!(@weak self as self_ => move |_| {
            self_.update_focuser_value(self_.step.get());
        }));

        self.widgets.btn_inc_large.connect_clicked(clone!(@weak self as self_ => move |_| {
            self_.update_focuser_value(self_.step_large.get());
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

    fn correct_widgets_props_impl(&self, cam_device: &str) {
        let mode = self.core.mode();
        let mode_type = mode.active.get_type();
        drop(mode);

        if let Ok(camera) = self.core.hal().camera(cam_device) {
            let exp_range = camera.exposure_range().ok();
            correct_spinbutton_by_range(&self.widgets.spb_exp, exp_range, 1, Some(1.0));
        }

        let waiting = mode_type == ModeType::Waiting;
        let live_view = mode_type == ModeType::LiveView;
        let single_shot = mode_type == ModeType::SingleShot;
        let focusing = mode_type == ModeType::Focusing;
        let can_change_mode = waiting || live_view || single_shot;

        let device_enabled = self.core.focuser()
            .and_then(|f| f.is_active().ok())
            .unwrap_or(false);

        self.widgets.grd.set_sensitive(device_enabled);
        self.widgets.spb_temp.set_sensitive(self.widgets.chb_temp.is_active());
        self.widgets.cb_fwhm.set_sensitive(self.widgets.chb_fwhm.is_active());
        self.widgets.cb_period.set_sensitive(self.widgets.chb_period.is_active());
        self.widgets.spb_val.set_sensitive(!focusing);
        self.widgets.cb_list.set_sensitive(!focusing);
        self.widgets.bx_ctrl_btns.set_sensitive(!focusing);

        enable_actions(&self.window, &[
            ("manual_focus",      !focusing && can_change_mode),
            ("stop_manual_focus", focusing),
        ]);

        self.main_ui.set_module_panel_visible(self.info_widgets.bx.upcast_ref(), device_enabled);
        self.show_info();
    }

    fn correct_widgets_props(&self) {
        let options = self.core.options().read().unwrap();
        let cam_device = options.cam.device_id.clone();
        drop(options);
        self.correct_widgets_props_impl(&cam_device);
    }

    fn handler_camera_changed(&self, prev_device_id: &str, new_device_id: &str) {
        let mut options = self.core.options().write().unwrap();
        self.get_options(&mut options);
        if !prev_device_id.is_empty() {
            self.store_options_for_camera(prev_device_id, &mut options);
        }
        self.restore_options_for_camera(new_device_id, &mut options);
        self.show_options(&options);
        drop(options);
        self.correct_widgets_props_impl(new_device_id);
    }

    fn store_options_for_camera(
        &self,
        device:  &str,
        options: &mut Options
    ) {
        if device.is_empty() {
            return;
        }
        let sep_options = options.sep_focuser.entry(device.to_string()).or_default();
        sep_options.exposure = options.focuser.exposure;
        sep_options.gain = options.focuser.gain;
    }

    fn restore_options_for_camera(
        &self,
        device:  &str,
        options: &mut Options
    ) {
        if let Some(sep_options) = options.sep_focuser.get(device) {
            options.focuser.exposure = sep_options.exposure;
            options.focuser.gain = sep_options.gain;
        }
    }

    fn update_devices_list(&self) {
        let options = self.core.options().read().unwrap();
        let cur_focuser = options.focuser.device.clone();
        drop(options);

        let hal = self.core.hal();
        let Ok(focusers) = hal.devices(DeviceType::FOCUSER) else { return; };
        let focusers_ids_and_names = focusers
            .into_iter()
            .map(|dev| (dev.id, dev.name))
            .collect::<Vec<_>>();

        let devices_connected = hal.state() == HalState::Connected;

        fill_devices_list_into_combobox(
            &focusers_ids_and_names,
            &self.widgets.cb_list,
            if !cur_focuser.is_empty() { Some(cur_focuser.as_str()) } else { None },
            devices_connected,
            |id| {
                let mut options = self.core.options().write().unwrap();
                options.focuser.device = id.to_string();
            }
        );
    }

    fn show_cur_focuser_value(&self, value: Option<i32>, force_configure_widget: bool) {
        let mut ok = false;
        if let Some(focuser) = self.core.focuser() {
            let abs_position = value.unwrap_or_else(|| focuser.abs_position().unwrap_or(0.0) as i32 );
            if force_configure_widget || self.widgets.spb_val.value() == 0.0 {
                println!("Init focuser widget");
                if let Ok(range) = focuser.abs_position_range() {
                    self.widgets.spb_val.set_range(*range.start(), *range.end());
                    self.widgets.spb_val.set_digits(0);
                    let len = range.end() - range.start();
                    let step = if len >= 100_000.0 {
                        100
                    } else if len >= 10_000.0 {
                        10
                    } else {
                        1
                    };
                    let large_step = step * 10;
                    self.step.set(step);
                    self.step_large.set(large_step);
                    self.widgets.spb_val.set_increments(step as f64, large_step as f64);
                    self.widgets.spb_val.set_tooltip_text(Some(&format!("{} .. {}", range.start(), range.end())));
                    self.widgets.btn_dec_large.set_tooltip_text(Some(&format!("- {}", large_step)));
                    self.widgets.btn_dec.set_tooltip_text(Some(&format!("- {}", step)));
                    self.widgets.btn_inc.set_tooltip_text(Some(&format!("+ {}", step)));
                    self.widgets.btn_inc_large.set_tooltip_text(Some(&format!("+ {}", large_step)));
                }
                self.excl.exec(|| {
                    self.widgets.spb_val.set_value(abs_position as f64);
                    ok = true;
                });
            }
            self.widgets.l_value.set_label(&abs_position.to_string());
        }
    }

    fn update_focuser_position_after_focusing(&self, pos: f64) {
        self.excl.exec(|| {
            self.widgets.spb_val.set_value(pos);
        });
    }

    fn handler_delayed_action(&self, action: &DelayedAction) {
        match action {
            DelayedAction::InitAndShowFocuserValue(value) => {
                self.show_cur_focuser_value(*value, true);
            }
            DelayedAction::ShowFocuserValue(value) => {
                self.show_cur_focuser_value(*value, false);
            }
            DelayedAction::ShowCurFocuserTemperature(value_x10) => {
                self.show_focuser_temperature(*value_x10);
            }
            DelayedAction::CorrectWidgetProps => {
                self.correct_widgets_props();
                self.show_info();
            }
        }
    }

    fn show_focuser_temperature(&self, value_x10: Option<i32>) {
        let temperature = value_x10
            .map(|v| v as f64 / 10.0)
            .unwrap_or_else(|| {
                self.core.focuser()
                    .and_then(|f| f.temperature().ok())
                    .unwrap_or(f64::NAN)
            });
        let temp_str =
            if !temperature.is_nan() { &format!("{:.1}°", temperature) } else { "---" };
        self.widgets.l_temp.set_label(temp_str);

        let mut diff_str = String::new();
        if let Some(starting_temp) = self.starting_temp.get() && !temperature.is_nan() {
            let diff = temperature - starting_temp;
            diff_str = format!("({:+.1}°)", diff);
        }
        self.widgets.l_temp_diff.set_label(&diff_str);
    }

    fn draw_focusing_samples(
        &self,
        da:  &gtk::DrawingArea,
        ctx: &gdk::cairo::Context
    ) -> eyre::Result<()> {
        let focusing_data = self.focusing_data.borrow();
        let Some(ref fd) = *focusing_data else {
            return Ok(());
        };
        const PARABOLA_POINTS: usize = 101;
        let get_plot_points_cnt = |plot_idx: usize| {
            match plot_idx {
                0 => fd.samples.len(),
                1 => if fd.coeffs.is_some() { PARABOLA_POINTS } else { 0 },
                2 => if fd.result.is_some() && fd.coeffs.is_some() { 1 } else { 0 },
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
        let min_pos = fd.samples.iter().map(|s| s.position).min_by(cmp_f64).unwrap_or(0.0);
        let max_pos = fd.samples.iter().map(|s| s.position).max_by(cmp_f64).unwrap_or(0.0);
        let get_plot_point = |plot_idx: usize, point_idx: usize| -> (f64, f64) {
            match plot_idx {
                0 => {
                    let sample = &fd.samples[point_idx];
                    (sample.position, sample.hfd as f64)
                }
                1 => {
                    if let Some(coeffs) = &fd.coeffs {
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
                    if let (Some(coeffs), Some(x))
                    = (&fd.coeffs, &fd.result) {
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
        let (_, dpmm_y) = get_widget_dpmm(da)
            .unwrap_or((DEFAULT_DPMM, DEFAULT_DPMM));
        let font_size_px = font_size_to_pixels(FontSize::Pt(font_size_pt), dpmm_y);
        ctx.set_font_size(font_size_px);

        draw_plots(&plots, da, ctx)?;
        Ok(())
    }

    fn handler_action_manual_focus(&self) {
        self.main_ui.get_all_options();

        exec_and_show_error(Some(&self.window), || {
            self.core.start_focusing()?;
            Ok(())
        });
    }

    fn handler_action_stop_manual_focus(&self) {
        self.core.abort_active_mode();
    }

    fn update_focuser_value(&self, offset: i32) {
        self.excl.exec(|| {
            exec_and_show_error(Some(&self.window), || {
                let Some(focuser) = self.core.focuser() else { return Ok(()); };
                let mut value = focuser.abs_position()?;
                let range = focuser.abs_position_range()?;
                value += offset as f64;
                let clamped_value = value.clamp(*range.start(), *range.end());
                if clamped_value == value {
                    return Ok(());
                }
                focuser.set_abs_position(value)?;
                self.widgets.spb_val.set_value(value);
                Ok(())
            });
        });
    }

    fn show_info(&self) {
        let mut info_shown = false;
        if let Some(focuser) = self.core.focuser() {
            let focuser_state = focuser.state().ok();
            enum InfoState { Work, Err }

            if self.prev_pos_state.get() != focuser_state {
                self.prev_pos_state.set(focuser_state);
                let (text, info_state) = match focuser_state {
                    Some(FocuserState::Stopped) => ("Stopped", None),
                    Some(FocuserState::Error)   => ("Error", Some(InfoState::Err)),
                    Some(FocuserState::Moving)  => ("Moving", Some(InfoState::Work)),
                    None                        => ("Disabled", None),
                };

                let mut text = format!("<b>{}</b>", text);
                match info_state {
                    Some(InfoState::Err) =>
                        text = format!("<span foreground='{}'>{}</span>", get_err_color_str(), text),
                    Some(InfoState::Work) =>
                        text = format!("<span foreground='{}'>{}</span>", get_warn_color_str(), text),
                    _ => {},
                }

                self.info_widgets.l_state.set_label(&text);
                info_shown = true;
            }
        }
        if !info_shown {
            self.info_widgets.l_state.set_label("");
        }
    }
}
