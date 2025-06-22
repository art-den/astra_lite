use std::{cell::{Cell, RefCell}, path::PathBuf, rc::Rc, sync::{Arc, RwLock}};
use chrono::{DateTime, Local, Utc};
use gtk::{cairo, glib::{self, clone}, prelude::*};
use macros::FromBuilder;
use serde::{Serialize, Deserialize};
use crate::{
    core::{core::*, events::*, frame_processing::*},
    image::{histogram::*, info::*, io::save_image_to_tif_file, preview::*, raw::{CalibrMethods, FrameType}, stars_offset::Offset},
    options::*,
    sky_math::math::radian_to_degree,
    utils::{io_utils::*, log_utils::*}
};
use super::{gtk_utils::*, module::*, ui_main::*, utils::*};

pub fn init_ui(
    window:  &gtk::ApplicationWindow,
    main_ui: &Rc<MainUi>,
    options: &Arc<RwLock<Options>>,
    core:    &Arc<Core>,
) -> Rc<dyn UiModule> {
    let mut ui_options = UiOptions::default();
    exec_and_show_error(Some(window), || {
        load_json_from_config_file(&mut ui_options, PreviewUi::CONF_FN)?;
        Ok(())
    });

    let builder = gtk::Builder::from_string(include_str!(r"resources/preview.ui"));

    let widgets = Widgets {
        common:  CommonWidgets::from_builder(&builder),
        ctrl:    ControlWidgets::from_builder(&builder),
        image:   ImageWidgets::from_builder(&builder),
        info:    InfoWidgets::from_builder(&builder),
        stat:    StatWidgets::from_builder(&builder),
        history: HistoryWidgets::from_builder(&builder),
    };

    let obj = Rc::new(PreviewUi {
        widgets,
        main_ui:            Rc::clone(main_ui),
        window:             window.clone(),
        core:               Arc::clone(core),
        options:            Arc::clone(options),
        ui_options:         RefCell::new(ui_options),
        preview_scroll_pos: RefCell::new(None),
        closed:             Cell::new(false),
        light_history:      RefCell::new(Vec::new()),
        calibr_history:     RefCell::new(Vec::new()),
        flat_info:          RefCell::new(FlatImageInfo::default()),
        is_color_image:     Cell::new(false),
    });

    obj.init_widgets();
    obj.show_ui_options();

    obj.connect_widgets_events();
    obj.connect_core_events();
    obj.connect_img_mouse_scroll_events();

    obj.update_light_history_table();
    obj.update_calibr_history_table();

    obj
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(default)]
struct UiOptions {
    hist_log_y:    bool,
    hist_percents: bool,
    flat_percents: bool,
    hist_width:    i32,
}

impl Default for UiOptions {
    fn default() -> Self {
        Self {
            hist_log_y:     false,
            hist_percents:  true,
            flat_percents:  true,
            hist_width:     -1,
        }
    }
}

enum MainThreadEvent {
    Core(Event),
}

struct LightHistoryItem {
    mode_type:      ModeType,
    time:           Option<DateTime<Utc>>,
    fwhm:           Option<f32>,
    fwhm_angular:   Option<f32>,
    hfd:            Option<f32>,
    bad_fwhm:       bool,
    stars_ovality:  Option<f32>,
    bad_ovality:    bool,
    stars_count:    usize,
    noise:          Option<f32>, // %
    background:     f32, // %
    offset:         Option<Offset>,
    bad_offset:     bool,
    calibr_methods: CalibrMethods,
}

struct CalibrHistoryItem {
    time:           Option<DateTime<Utc>>,
    mode_type:      ModeType,
    frame_type:     FrameType,
    mean:           f32,
    median:         u16,
    std_dev:        f32,
    calibr_methods: CalibrMethods, // for flat files
}

impl PreviewScale {
    pub fn from_active_id(id: Option<&str>) -> PreviewScale {
        match id {
            Some("fit")     => PreviewScale::FitWindow,
            Some("orig")    => PreviewScale::Original,
            Some("p75")     => PreviewScale::P75,
            Some("p50")     => PreviewScale::P50,
            Some("p33")     => PreviewScale::P33,
            Some("p25")     => PreviewScale::P25,
            Some("c_and_c") => PreviewScale::CenterAndCorners,
            _               => PreviewScale::FitWindow,
        }
    }

    pub fn to_active_id(&self) -> Option<&'static str> {
        match self {
            PreviewScale::FitWindow        => Some("fit"),
            PreviewScale::Original         => Some("orig"),
            PreviewScale::P75              => Some("p75"),
            PreviewScale::P50              => Some("p50"),
            PreviewScale::P33              => Some("p33"),
            PreviewScale::P25              => Some("p25"),
            PreviewScale::CenterAndCorners => Some("c_and_c"),
        }
    }
}

impl PreviewSource {
    pub fn from_active_id(active_id: Option<&str>) -> Self {
        match active_id {
            Some("live") => Self::LiveStacking,
            _            => Self::OrigFrame,
        }
    }

    pub fn to_active_id(&self) -> Option<&'static str> {
        match self {
            Self::OrigFrame    => Some("frame"),
            Self::LiveStacking => Some("live"),
        }
    }
}

impl PreviewColorMode {
    pub fn from_active_id(active_id: Option<&str>) -> Self {
        match active_id {
            Some("red")   => Self::Red,
            Some("green") => Self::Green,
            Some("blue")  => Self::Blue,
            _             => Self::Rgb,
        }
    }

    pub fn to_active_id(&self) -> Option<&'static str> {
        match self {
            Self::Rgb   => Some("rgb"),
            Self::Red   => Some("red"),
            Self::Green => Some("green"),
            Self::Blue  => Some("blue"),
        }
    }
}

#[derive(FromBuilder)]
struct CommonWidgets {
    pan_preview:  gtk::Paned,
    pan_preview2: gtk::Paned,
}

#[derive(FromBuilder)]
struct ControlWidgets {
    cb_src:        gtk::ComboBoxText,
    cb_scale:      gtk::ComboBoxText,
    cb_color:      gtk::ComboBoxText,
    chb_rem_grad:  gtk::CheckButton,
    scl_dark:      gtk::Scale,
    scl_gamma:     gtk::Scale,
    scl_highlight: gtk::Scale,
    chb_wb_auto:   gtk::CheckButton,
    l_wb_censor:   gtk::Label,
    l_wb_red:      gtk::Label,
    scl_wb_red:    gtk::Scale,
    l_wb_green:    gtk::Label,
    scl_wb_green:  gtk::Scale,
    l_wb_blue:     gtk::Label,
    scl_wb_blue:   gtk::Scale,
    chb_stars:     gtk::CheckButton,
}

#[derive(FromBuilder)]
struct ImageWidgets {
    sw_img:           gtk::ScrolledWindow,
    eb_img:           gtk::EventBox,
    img_preview:      gtk::Image,
    l_overlay_top:    gtk::Label,
    l_overlay_bottom: gtk::Label,
}

#[derive(FromBuilder)]
struct InfoWidgets {
    scr_img_info:  gtk::ScrolledWindow,
    e_res_info:    gtk::Entry,
    bx_light_info: gtk::Box,
    e_info_exp:    gtk::Entry,
    e_fwhm:        gtk::Entry,
    e_ovality:     gtk::Entry,
    e_stars:       gtk::Entry,
    e_background:  gtk::Entry,
    e_noise:       gtk::Entry,
    bx_flat_info:  gtk::Box,
    l_flat_r:      gtk::Label,
    e_flat_r:      gtk::Entry,
    l_flat_g:      gtk::Label,
    e_flat_g:      gtk::Entry,
    l_flat_b:      gtk::Label,
    e_flat_b:      gtk::Entry,
    l_flat_l:      gtk::Label,
    e_flat_l:      gtk::Entry,
    bx_raw_info:   gtk::Box,
    e_aver:        gtk::Entry,
    e_median:      gtk::Entry,
    e_std_dev:     gtk::Entry,
    chb_flat_percents: gtk::CheckButton,
}

#[derive(FromBuilder)]
struct StatWidgets {
    ch_hist_log_y:    gtk::CheckButton,
    ch_stat_percents: gtk::CheckButton,
    da_histogram:     gtk::DrawingArea,
    l_r_cap:          gtk::Label,
    l_r_mean:         gtk::Label,
    l_r_median:       gtk::Label,
    l_r_dev:          gtk::Label,
    l_g_cap:          gtk::Label,
    l_g_mean:         gtk::Label,
    l_g_median:       gtk::Label,
    l_g_dev:          gtk::Label,
    l_b_cap:          gtk::Label,
    l_b_mean:         gtk::Label,
    l_b_median:       gtk::Label,
    l_b_dev:          gtk::Label,
    l_l_cap:          gtk::Label,
    l_l_mean:         gtk::Label,
    l_l_median:       gtk::Label,
    l_l_dev:          gtk::Label,
}

#[derive(FromBuilder)]
struct HistoryWidgets {
    nb_hist:  gtk::Notebook,
    tv_light: gtk::TreeView,
    tv_calbr: gtk::TreeView,
}

struct Widgets {
    common:  CommonWidgets,
    ctrl:    ControlWidgets,
    image:   ImageWidgets,
    info:    InfoWidgets,
    stat:    StatWidgets,
    history: HistoryWidgets,
}

struct PreviewUi {
    main_ui:            Rc<MainUi>,
    window:             gtk::ApplicationWindow,
    widgets:            Widgets,
    options:            Arc<RwLock<Options>>,
    core:               Arc<Core>,
    ui_options:         RefCell<UiOptions>,
    preview_scroll_pos: RefCell<Option<((f64, f64), (f64, f64))>>,
    light_history:      RefCell<Vec<LightHistoryItem>>,
    calibr_history:     RefCell<Vec<CalibrHistoryItem>>,
    closed:             Cell<bool>,
    flat_info:          RefCell<FlatImageInfo>,
    is_color_image:     Cell<bool>,
}

impl Drop for PreviewUi {
    fn drop(&mut self) {
        log::info!("PreviewUi dropped");
    }
}

impl UiModule for PreviewUi {
    fn show_options(&self, options: &Options) {
        let widgets = &self.widgets;
        widgets.ctrl.cb_src.set_active_id(options.preview.source.to_active_id());
        widgets.ctrl.cb_scale.set_active_id(options.preview.scale.to_active_id());
        widgets.ctrl.cb_color.set_active_id(options.preview.color.to_active_id());
        widgets.ctrl.scl_dark.set_value(options.preview.dark_lvl);
        widgets.ctrl.scl_highlight.set_value(options.preview.light_lvl);
        widgets.ctrl.scl_gamma.set_value(options.preview.gamma);
        widgets.ctrl.chb_rem_grad.set_active(options.preview.remove_grad);
        widgets.ctrl.chb_wb_auto.set_active(options.preview.wb_auto);
        widgets.ctrl.scl_wb_red.set_value(options.preview.wb_red);
        widgets.ctrl.scl_wb_green.set_value(options.preview.wb_green);
        widgets.ctrl.scl_wb_blue.set_value(options.preview.wb_blue);
        widgets.ctrl.chb_stars.set_active(options.preview.stars);
    }

    fn get_options(&self, options: &mut Options) {
        let widgets = &self.widgets;
        options.preview.scale = PreviewScale::from_active_id(
            widgets.ctrl.cb_scale.active_id().as_deref()
        );
        options.preview.source = PreviewSource::from_active_id(
            widgets.ctrl.cb_src.active_id().as_deref()
        );
        options.preview.gamma       = widgets.ctrl.scl_gamma.value();
        options.preview.dark_lvl    = widgets.ctrl.scl_dark.value();
        options.preview.light_lvl   = widgets.ctrl.scl_highlight.value();
        options.preview.remove_grad = widgets.ctrl.chb_rem_grad.is_active();
        options.preview.wb_auto     = widgets.ctrl.chb_wb_auto.is_active();
        options.preview.wb_red      = widgets.ctrl.scl_wb_red.value();
        options.preview.wb_green    = widgets.ctrl.scl_wb_green.value();
        options.preview.wb_blue     = widgets.ctrl.scl_wb_blue.value();
        options.preview.stars       = widgets.ctrl.chb_stars.is_active();
    }

    fn panels(&self) -> Vec<Panel> {
        vec![
            Panel {
                str_id: "preview",
                name:   String::new(),
                widget: self.widgets.common.pan_preview.clone().upcast(),
                pos:    PanelPosition::Center,
                tab:    TabPage::Main,
                flags:  PanelFlags::NO_EXPANDER,
            },
        ]
    }

    fn on_show_options_first_time(&self) {
        self.correct_widgets_props();
    }

    fn on_full_screen(&self, full_screen: bool) {
        let options = self.options.read().unwrap();
        let preview_scale = options.preview.scale;
        drop(options);

        self.widgets.common.pan_preview2.set_visible(!full_screen);
        self.widgets.info.scr_img_info.set_visible(!full_screen);

        if matches!(preview_scale, PreviewScale::FitWindow|PreviewScale::CenterAndCorners) {
            gtk::main_iteration_do(true);
            gtk::main_iteration_do(true);
            gtk::main_iteration_do(true);
            self.create_and_show_preview_image();
        }
    }

    fn on_app_closing(&self) {
        self.closed.set(true);

        _ = self.core.stop_img_process_thread();
        self.core.abort_active_mode();

        self.get_ui_options_from_widgets();

        let ui_options = self.ui_options.borrow();
        _ = save_json_to_config::<UiOptions>(&ui_options, Self::CONF_FN);
        drop(ui_options);
    }
}

impl PreviewUi {
    const CONF_FN: &'static str = "ui_prevuew";

    fn init_widgets(&self) {
        self.widgets.ctrl.l_wb_censor.set_label("");

        self.widgets.ctrl.scl_dark.set_range(0.0, 1.0);
        self.widgets.ctrl.scl_dark.set_increments(0.1, 0.5);
        self.widgets.ctrl.scl_dark.set_round_digits(1);
        self.widgets.ctrl.scl_dark.set_digits(1);

        let (dpimm_x, _) = get_widget_dpmm(&self.window)
            .unwrap_or((DEFAULT_DPMM, DEFAULT_DPMM));
        self.widgets.ctrl.scl_dark.set_width_request((40.0 * dpimm_x) as i32);

        self.widgets.ctrl.scl_highlight.set_range(0.0, 1.0);
        self.widgets.ctrl.scl_highlight.set_increments(0.1, 0.5);
        self.widgets.ctrl.scl_highlight.set_round_digits(1);
        self.widgets.ctrl.scl_highlight.set_digits(1);

        self.widgets.ctrl.scl_gamma.set_range(1.0, 5.0);
        self.widgets.ctrl.scl_gamma.set_digits(1);
        self.widgets.ctrl.scl_gamma.set_increments(0.1, 1.0);
        self.widgets.ctrl.scl_gamma.set_round_digits(1);
        self.widgets.ctrl.scl_gamma.set_digits(1);

        let configure_wb_scale = |scale: &gtk::Scale| {
            scale.set_range(0.5, 2.0);
            scale.set_increments(0.1, 0.5);
            scale.set_round_digits(1);
            scale.set_digits(1);
        };

        configure_wb_scale(&self.widgets.ctrl.scl_wb_red);
        configure_wb_scale(&self.widgets.ctrl.scl_wb_green);
        configure_wb_scale(&self.widgets.ctrl.scl_wb_blue);

        self.widgets.image.l_overlay_top.set_text("");
        self.widgets.image.l_overlay_bottom.set_text("");
    }

    fn connect_widgets_events(self: &Rc<Self>) {
        connect_action   (&self.window, self, "save_image_preview",  Self::handler_action_save_image_preview);
        connect_action   (&self.window, self, "save_image_linear",   Self::handler_action_save_image_linear);
        connect_action   (&self.window, self, "clear_light_history", Self::handler_action_clear_light_history);
        connect_action_rc(&self.window, self, "load_image",          Self::handler_action_open_image);

        self.widgets.stat.ch_hist_log_y.connect_active_notify(
            clone!(@weak self as self_ => move |chb| {
                let Ok(mut ui_options) = self_.ui_options.try_borrow_mut() else { return; };
                ui_options.hist_log_y = chb.is_active();
                self_.repaint_histogram();
            })
        );

        self.widgets.stat.ch_stat_percents.connect_active_notify(
            clone!(@weak self as self_ => move |chb| {
                let Ok(mut ui_options) = self_.ui_options.try_borrow_mut() else { return; };
                ui_options.hist_percents = chb.is_active();
                drop(ui_options);
                self_.show_histogram_stat();
            })
        );

        self.widgets.info.chb_flat_percents.connect_active_notify(
            clone!(@weak self as self_ => move |chb| {
                let Ok(mut ui_options) = self_.ui_options.try_borrow_mut() else { return; };
                ui_options.flat_percents = chb.is_active();
                drop(ui_options);
                self_.show_flat_info();
            })
        );

        self.widgets.ctrl.cb_src.connect_active_id_notify(
            clone!(@weak self as self_ => move |cb| {
                let Ok(mut options) = self_.options.try_write() else { return; };
                let source = PreviewSource::from_active_id(cb.active_id().as_deref());
                options.preview.source = source;
                drop(options);
                self_.create_and_show_preview_image();
                self_.repaint_histogram();
                self_.show_histogram_stat();
                self_.show_image_info();
            })
        );

        self.widgets.image.sw_img.connect_size_allocate(
            clone!(@weak self as self_ => move |_, rect| {
                let Ok(mut options) = self_.options.try_write() else { return; };
                options.preview.widget_width = rect.width() as usize;
                options.preview.widget_height = rect.height() as usize;
            })
        );

        self.widgets.ctrl.cb_scale.connect_active_id_notify(
            clone!(@weak self as self_ => move |cb| {
                let Ok(mut options) = self_.options.try_write() else { return; };
                let scale = PreviewScale::from_active_id(cb.active_id().as_deref());
                options.preview.scale = scale;
                drop(options);
                self_.create_and_show_preview_image();
            })
        );

        self.widgets.ctrl.cb_color.connect_active_id_notify(
            clone!(@weak self as self_ => move |cb| {
                let Ok(mut options) = self_.options.try_write() else { return; };
                let color = PreviewColorMode::from_active_id(cb.active_id().as_deref());
                options.preview.color = color;
                drop(options);
                self_.create_and_show_preview_image();
            })
        );

        self.widgets.ctrl.scl_dark.connect_value_changed(
            clone!(@weak self as self_ => move |scl| {
                let Ok(mut options) = self_.options.try_write() else { return; };
                options.preview.dark_lvl = scl.value();
                drop(options);
                self_.create_and_show_preview_image();
            })
        );

        self.widgets.ctrl.scl_highlight.connect_value_changed(
            clone!(@weak self as self_ => move |scl| {
                let Ok(mut options) = self_.options.try_write() else { return; };
                options.preview.light_lvl = scl.value();
                drop(options);
                self_.create_and_show_preview_image();
            })
        );

        self.widgets.ctrl.scl_gamma.connect_value_changed(
            clone!(@weak self as self_ => move |scl| {
                let Ok(mut options) = self_.options.try_write() else { return; };
                options.preview.gamma = scl.value();
                drop(options);
                self_.create_and_show_preview_image();
            })
        );

        self.widgets.ctrl.chb_rem_grad.connect_active_notify(
            clone!(@weak self as self_ => move |chb| {
                let Ok(mut options) = self_.options.try_write() else { return; };
                options.preview.remove_grad = chb.is_active();
                drop(options);
                self_.create_and_show_preview_image();
            })
        );

        self.widgets.stat.da_histogram.connect_draw(
            clone!(@weak self as self_ => @default-return glib::Propagation::Proceed,
            move |area, cr| {
                exec_and_show_error(Some(&self_.window), || {
                    self_.handler_draw_histogram(area, cr)?;
                    Ok(())
                });
                glib::Propagation::Proceed
            })
        );

        self.widgets.ctrl.chb_wb_auto.connect_active_notify(
            clone!(@weak self as self_ => move |chb| {
                let Ok(mut options) = self_.options.try_write() else { return; };
                options.preview.wb_auto = chb.is_active();
                drop(options);
                self_.create_and_show_preview_image();
            })
        );

        self.widgets.ctrl.scl_wb_red.connect_value_changed(
            clone!(@weak self as self_ => move |scl| {
                let Ok(mut options) = self_.options.try_write() else { return; };
                options.preview.wb_red = scl.value();
                drop(options);
                self_.create_and_show_preview_image();
            })
        );

        self.widgets.ctrl.scl_wb_green.connect_value_changed(
            clone!(@weak self as self_ => move |scl| {
                let Ok(mut options) = self_.options.try_write() else { return; };
                options.preview.wb_green = scl.value();
                drop(options);
                self_.create_and_show_preview_image();
            })
        );

        self.widgets.ctrl.scl_wb_blue.connect_value_changed(
            clone!(@weak self as self_ => move |scl| {
                let Ok(mut options) = self_.options.try_write() else { return; };
                options.preview.wb_blue = scl.value();
                drop(options);
                self_.create_and_show_preview_image();
            })
        );

        self.widgets.ctrl.chb_stars.connect_active_notify(clone!(@weak self as self_ => move |chb| {
            let Ok(mut options) = self_.options.try_write() else { return; };
            options.preview.stars = chb.is_active();
            drop(options);
            self_.create_and_show_preview_image();
        }));
    }

    fn connect_core_events(self: &Rc<Self>) {
        let (main_thread_sender, main_thread_receiver) = async_channel::unbounded();

        let sender = main_thread_sender.clone();
        self.core.event_subscriptions().subscribe(move |event| {
            sender.send_blocking(MainThreadEvent::Core(event)).unwrap();
        });

        glib::spawn_future_local(clone!(@weak self as self_ => async move {
            while let Ok(event) = main_thread_receiver.recv().await {
                if self_.closed.get() { return; }
                self_.process_core_event(event);
            }
        }));
    }

    fn connect_img_mouse_scroll_events(self: &Rc<Self>) {
        self.widgets.image.eb_img.connect_button_press_event(
            clone!(@weak self as self_ => @default-return glib::Propagation::Proceed,
            move |_, evt| {
                if evt.button() == gtk::gdk::ffi::GDK_BUTTON_PRIMARY as u32 {
                    let hadjustment = self_.widgets.image.sw_img.hadjustment();
                    let vadjustment = self_.widgets.image.sw_img.vadjustment();
                    *self_.preview_scroll_pos.borrow_mut() = Some((
                        evt.root(),
                        (hadjustment.value(), vadjustment.value())
                    ));
                }
                glib::Propagation::Proceed
            })
        );

        self.widgets.image.eb_img.connect_button_release_event(
            clone!(@weak self as self_ => @default-return glib::Propagation::Proceed,
            move |_, evt| {
                if evt.button() == gtk::gdk::ffi::GDK_BUTTON_PRIMARY as u32 {
                    *self_.preview_scroll_pos.borrow_mut() = None;
                }
                glib::Propagation::Proceed
            })
        );

        self.widgets.image.eb_img.connect_motion_notify_event(
            clone!(@weak self as self_ => @default-return glib::Propagation::Proceed,
            move |_, evt| {
                const SCROLL_SPEED: f64 = 2.0;
                if let Some((start_mouse_pos, start_scroll_pos)) = *self_.preview_scroll_pos.borrow() {
                    let new_pos = evt.root();
                    let move_x = new_pos.0 - start_mouse_pos.0;
                    let move_y = new_pos.1 - start_mouse_pos.1;
                    let hadjustment = self_.widgets.image.sw_img.hadjustment();
                    hadjustment.set_value(start_scroll_pos.0 - SCROLL_SPEED*move_x);
                    let vadjustment = self_.widgets.image.sw_img.vadjustment();
                    vadjustment.set_value(start_scroll_pos.1 - SCROLL_SPEED*move_y);
                }
                glib::Propagation::Proceed
            })
        );
    }

    fn process_core_event(&self, event: MainThreadEvent) {
        match event {
            MainThreadEvent::Core(Event::FrameProcessing(result)) => {
                self.show_frame_processing_result(result);
            }

            MainThreadEvent::Core(Event::ModeChanged) => {
                self.correct_preview_source();
            }

            MainThreadEvent::Core(Event::ModeContinued) => {
                self.correct_preview_source();
            }

            MainThreadEvent::Core(Event::OverlayMessage { pos, text }) => {
                self.show_overlay_message(pos, &text);
            }

            _ => {},
        }
    }

    fn show_ui_options(&self) {
        let options = self.ui_options.borrow();
        self.widgets.stat.ch_hist_log_y.set_active(options.hist_log_y);
        self.widgets.stat.ch_stat_percents.set_active(options.hist_percents);
        self.widgets.info.chb_flat_percents.set_active(options.flat_percents);
        if options.hist_width != -1 {
            self.widgets.common.pan_preview2.set_position(options.hist_width);
        }
    }

    fn get_ui_options_from_widgets(&self) {
        let mut options = self.ui_options.borrow_mut();
        options.hist_log_y = self.widgets.stat.ch_hist_log_y.is_active();
        options.hist_percents = self.widgets.stat.ch_stat_percents.is_active();
        options.flat_percents = self.widgets.info.chb_flat_percents.is_active();
        options.hist_width = self.widgets.common.pan_preview2.position();
    }

    fn correct_widgets_props(&self) {
        let auto_color_checked = self.widgets.ctrl.chb_wb_auto.is_active();
        let is_color_image = self.is_color_image.get();
        let rgb_enabled = !auto_color_checked && is_color_image;

        self.widgets.ctrl.cb_color.set_sensitive(is_color_image);
        self.widgets.ctrl.chb_wb_auto.set_sensitive(is_color_image);

        self.widgets.ctrl.l_wb_red.set_sensitive(rgb_enabled);
        self.widgets.ctrl.scl_wb_red.set_sensitive(rgb_enabled);
        self.widgets.ctrl.l_wb_green.set_sensitive(rgb_enabled);
        self.widgets.ctrl.scl_wb_green.set_sensitive(rgb_enabled);
        self.widgets.ctrl.l_wb_blue.set_sensitive(rgb_enabled);
        self.widgets.ctrl.scl_wb_blue.set_sensitive(rgb_enabled);
    }

    fn show_overlay_message(&self, pos: OverlayMessgagePos, text: &str) {
        match pos {
            OverlayMessgagePos::Top => {
                self.widgets.image.l_overlay_top.set_text(text);
            }
        }
    }

    fn show_image_info(&self) {
        let info = match self.options.read().unwrap().preview.source {
            PreviewSource::OrigFrame =>
                self.core.cur_frame().info.read().unwrap(),
            PreviewSource::LiveStacking =>
                self.core.live_stacking().info.read().unwrap(),
        };

        let update_info_panel_vis = |is_light_info: bool, is_flat_info: bool, is_raw_info: bool| {
            self.widgets.info.bx_light_info.set_visible(is_light_info);
            self.widgets.info.bx_flat_info.set_visible(is_flat_info);
            self.widgets.info.bx_raw_info.set_visible(is_raw_info);
        };

        match &*info {
            ResultImageInfo::LightInfo(info) => {
                self.widgets.info.e_info_exp.set_text(&seconds_to_total_time_str(info.image.exposure, true));

                let mut fwhm_hfd_str = String::new();
                if let Some(value) = info.stars.info.fwhm {
                    fwhm_hfd_str += &format!("{:.1}", value);
                }
                if let Some(value) = info.stars.info.fwhm_angular {
                    fwhm_hfd_str += &format!("({:.1}\")", 60.0 * 60.0 * radian_to_degree(value as f64));
                }
                if let Some(value) = info.stars.info.hfd {
                    fwhm_hfd_str += &format!(" / {:.1}", value);
                }

                self.widgets.info.e_fwhm.set_text(&fwhm_hfd_str);

                match info.stars.info.ovality {
                    Some(value) => self.widgets.info.e_ovality.set_text(&format!("{:.1}", value)),
                    None        => self.widgets.info.e_ovality.set_text(""),
                }
                let stars_cnt = info.stars.items.len();
                let overexp_stars = info.stars.items.iter().filter(|s| s.overexposured).count();
                self.widgets.info.e_stars.set_text(&format!("{} ({})", stars_cnt, overexp_stars));
                let bg = 100_f64 * info.image.background as f64 / info.image.max_value as f64;
                self.widgets.info.e_background.set_text(&format!("{:.2}%", bg));
                let noise = 100_f64 * info.image.noise as f64 / info.image.max_value as f64;
                self.widgets.info.e_noise.set_text(&format!("{:.4}%", noise));
                update_info_panel_vis(true, false, false);
            },
            ResultImageInfo::FlatInfo(info) => {
                *self.flat_info.borrow_mut() = info.clone();
                update_info_panel_vis(false, true, false);
                self.show_flat_info();
            },
            ResultImageInfo::RawInfo(info) => {
                let aver_text = format!(
                    "{:.1} ({:.1}%)",
                    info.aver,
                    100.0 * info.aver / info.max_value as f32
                );
                self.widgets.info.e_aver.set_text(&aver_text);
                let median_text = format!(
                    "{} ({:.1}%)",
                    info.median,
                    100.0 * info.median as f64 / info.max_value as f64
                );
                self.widgets.info.e_median.set_text(&median_text);
                let dev_text = format!(
                    "{:.1} ({:.3}%)",
                    info.std_dev,
                    100.0 * info.std_dev / info.max_value as f32
                );
                self.widgets.info.e_std_dev.set_text(&dev_text);
                update_info_panel_vis(false, false, true);
            },
            _ => {
                update_info_panel_vis(false, false, false);
            },
        }
    }

    fn show_flat_info(&self) {
        let info = self.flat_info.borrow();
        let ui_options = self.ui_options.borrow();
        let show_chan = |label: &gtk::Label, entry: &gtk::Entry, item: Option<&FlatInfoChan>| {
            if let Some(item) = item {
                let text =
                    if ui_options.flat_percents {
                        let percent_aver = 100.0 * item.aver / info.max_value as f32;
                        let percent_max = 100.0 * item.max as f64 / info.max_value as f64;
                        format!("{:.1}% / {:.1}%", percent_aver, percent_max)
                    } else {
                        format!("{:.1} / {}", item.aver, item.max)
                    };
                entry.set_text(&text);
            }
            label.set_visible(item.is_some());
            entry.set_visible(item.is_some());
        };

        show_chan(&self.widgets.info.l_flat_r, &self.widgets.info.e_flat_r, info.r.as_ref());
        show_chan(&self.widgets.info.l_flat_g, &self.widgets.info.e_flat_g, info.g.as_ref());
        show_chan(&self.widgets.info.l_flat_b, &self.widgets.info.e_flat_b, info.b.as_ref());
        show_chan(&self.widgets.info.l_flat_l, &self.widgets.info.e_flat_l, info.l.as_ref());
    }

    fn create_and_show_preview_image(&self) {
        let options = self.options.read().unwrap();
        let preview_params = options.preview.preview_params();
        let (image, hist, stars) = match options.preview.source {
            PreviewSource::OrigFrame =>
                (&*self.core.cur_frame().image, &self.core.cur_frame().img_hist, Some(&self.core.cur_frame().stars)),
            PreviewSource::LiveStacking =>
                (&self.core.live_stacking().image, &self.core.live_stacking().hist, None),
        };
        drop(options);
        let image = image.read().unwrap();
        let hist = hist.read().unwrap();
        let stars = stars.as_ref().map(|s| s.read().unwrap());
        let rgb_bytes = get_preview_rgb_data(
            &image,
            &hist,
            &preview_params,
            stars.as_ref().and_then(|s| s.as_ref().map(|s| &**s))
        );
        drop(stars);
        drop(hist);
        drop(image);

        if let Some(rgb_bytes) = rgb_bytes {
            self.show_preview_image(Some(&rgb_bytes), None);
        } else {
            self.show_preview_image(None, None);
        }
        self.correct_widgets_props();
    }

    fn show_preview_image(
        &self,
        rgb_bytes:  Option<&PreviewRgbData>,
        src_params: Option<&PreviewParams>,
    ) {
        let preview_options = self.options.read().unwrap().preview.clone();
        let pp = preview_options.preview_params();
        if src_params.is_some() && src_params != Some(&pp) {
            self.create_and_show_preview_image();
            return;
        }

        let mut is_color_image = false;
        if let Some(rgb_bytes) = rgb_bytes {
            let tmr = TimeLogger::start();
            let bytes = glib::Bytes::from_owned(rgb_bytes.bytes.clone());
            let mut pixbuf = gtk::gdk_pixbuf::Pixbuf::from_bytes(
                &bytes,
                gtk::gdk_pixbuf::Colorspace::Rgb,
                false,
                8,
                rgb_bytes.width as i32,
                rgb_bytes.height as i32,
                (rgb_bytes.width * 3) as i32,
            );
            tmr.log("Pixbuf::from_bytes");

            if !rgb_bytes.sensor_name.is_empty() {
                self.widgets.ctrl.l_wb_censor.set_label(format!("({})", rgb_bytes.sensor_name).as_str());
            } else {
                self.widgets.ctrl.l_wb_censor.set_label("");
            }

            let (img_width, img_height) = pp.get_preview_img_size(
                rgb_bytes.orig_width,
                rgb_bytes.orig_height
            );
            if (img_width != rgb_bytes.width || img_height != rgb_bytes.height)
            && img_width > 42 && img_height > 42 {
                let tmr = TimeLogger::start();
                pixbuf = pixbuf.scale_simple(
                    img_width as _,
                    img_height as _,
                    gtk::gdk_pixbuf::InterpType::Tiles,
                ).unwrap();
                tmr.log("Pixbuf::scale_simple");
            }
            self.widgets.image.img_preview.set_pixbuf(Some(&pixbuf));
            is_color_image = rgb_bytes.is_color_image;
        } else {
            self.widgets.image.img_preview.clear();
            self.widgets.image.img_preview.set_pixbuf(None);
        }

        self.is_color_image.set(is_color_image);

    }

    fn handler_action_save_image_preview(&self) {
        exec_and_show_error(Some(&self.window), || {
            let options = self.options.read().unwrap();
            let (image, hist, fn_prefix) = match options.preview.source {
                PreviewSource::OrigFrame =>
                    (&*self.core.cur_frame().image, &self.core.cur_frame().img_hist, "preview"),
                PreviewSource::LiveStacking =>
                    (&self.core.live_stacking().image, &self.core.live_stacking().hist, "live"),
            };
            if image.read().unwrap().is_empty() { return Ok(()); }
            let mut preview_options = options.preview.clone();
            preview_options.scale = PreviewScale::Original;
            drop(options);
            let def_file_name = format!(
                "{}_{}.jpg",
                fn_prefix,
                Utc::now().format("%Y-%m-%d_%H-%M-%S")
            );
            let Some(file_name) = select_file_name_to_save(
                &self.window,
                "Enter file name to save preview image as jpeg",
                "Jpeg images", "*.jpg",
                "jpg",
                &def_file_name,
            ) else {
                return Ok(());
            };
            let image = image.read().unwrap();
            let hist = hist.read().unwrap();
            let preview_params = preview_options.preview_params();
            let rgb_data = get_preview_rgb_data(&image, &hist, &preview_params, None);
            let Some(rgb_data) = rgb_data else { anyhow::bail!("wrong RGB fata"); };
            let bytes = glib::Bytes::from_owned(rgb_data.bytes);
            let pixbuf = gtk::gdk_pixbuf::Pixbuf::from_bytes(
                &bytes,
                gtk::gdk_pixbuf::Colorspace::Rgb,
                false,
                8,
                rgb_data.width as i32,
                rgb_data.height as i32,
                (rgb_data.width * 3) as i32,
            );
            pixbuf.savev(file_name, "jpeg", &[("quality", "90")])?;
            Ok(())
        });
    }

    fn handler_action_save_image_linear(&self) {
        exec_and_show_error(Some(&self.window), || {
            let options = self.options.read().unwrap();
            let preview_source = options.preview.source.clone();
            drop(options);
            let ask_to_select_name = |fn_prefix: &str| -> Option<PathBuf> {
                let def_file_name = format!(
                    "{}_{}.tif",
                    fn_prefix,
                    Utc::now().format("%Y-%m-%d_%H-%M-%S")
                );
                select_file_name_to_save(
                    &self.window,
                    "Enter file name to save preview image as tiff",
                    "Tiff images", "*.tif",
                    "tif",
                    &def_file_name,
                )
            };
            match preview_source {
                PreviewSource::OrigFrame => {
                    let image = &self.core.cur_frame().image;
                    if image.read().unwrap().is_empty() {
                        return Ok(());
                    }
                    let Some(file_name) = ask_to_select_name("preview") else {
                        return Ok(())
                    };
                    let image = image.read().unwrap();
                    save_image_to_tif_file(&image, &file_name)?;
                }
                PreviewSource::LiveStacking => {
                    let stacker = &self.core.live_stacking().stacker;
                    if stacker.read().unwrap().is_empty() {
                        return Ok(());
                    }
                    let Some(file_name) = ask_to_select_name("live") else {
                        return Ok(())
                    };
                    stacker.read().unwrap().save_to_tiff(&file_name)?;
                }
            }
            Ok(())
        });
    }

    fn show_frame_processing_result(
        &self,
        result: FrameProcessResult
    ) {
        let options = self.options.read().unwrap();
        if !result.camera.name.is_empty()
        && options.cam.device != Some(result.camera) {
            return;
        }
        let live_stacking_preview = options.preview.source == PreviewSource::LiveStacking;
        drop(options);

        let show_resolution_info = |width, height| {
            self.widgets.info.e_res_info.set_text(&format!("{} x {}", width, height));
        };

        let is_mode_current = |live_result: bool| {
            live_result == live_stacking_preview
        };

        match result.data {
            FrameProcessResultData::ShotProcessingFinished {
                processing_time, blob_dl_time, ..
            } => {
                let perf_str = format!(
                    "Download time = {:.2}s, img. process time = {:.2}s",
                    blob_dl_time, processing_time
                );
                self.main_ui.set_perf_string(perf_str);
            }
            FrameProcessResultData::PreviewFrame(img)
            if is_mode_current(false) => {
                self.show_preview_image(Some(&img.rgb_data), Some(&img.params));
                self.correct_widgets_props();

                show_resolution_info(img.rgb_data.orig_width, img.rgb_data.orig_height);
            }
            FrameProcessResultData::PreviewLiveRes(img)
            if is_mode_current(true) => {
                self.show_preview_image(Some(&img.rgb_data), Some(&img.params));
                self.correct_widgets_props();

                show_resolution_info(img.rgb_data.orig_width, img.rgb_data.orig_height);
            }
            FrameProcessResultData::HistorgamRaw(_)
            if is_mode_current(false) => {
                self.repaint_histogram();
                self.show_histogram_stat();
            }
            FrameProcessResultData::RawFrameInfo(raw_frame_info)
            if is_mode_current(false) => {
                if raw_frame_info.frame_type != FrameType::Lights {
                    let history_item = CalibrHistoryItem {
                        time:           raw_frame_info.time,
                        mode_type:      result.mode_type,
                        frame_type:     raw_frame_info.frame_type,
                        mean:           raw_frame_info.mean,
                        median:         raw_frame_info.median,
                        std_dev:        raw_frame_info.std_dev,
                        calibr_methods: raw_frame_info.calubr_methods,
                    };
                    self.calibr_history.borrow_mut().push(history_item);
                    self.update_calibr_history_table();
                    self.set_hist_tab_active(Self::HIST_TAB_CALIBR);
                }
            }
            FrameProcessResultData::HistogramLiveRes
            if is_mode_current(true) => {
                self.repaint_histogram();
                self.show_histogram_stat();
            }
            FrameProcessResultData::LightFrameInfo(info) => {
                let history_item = LightHistoryItem {
                    mode_type:      result.mode_type,
                    time:           info.image.time,
                    fwhm:           info.stars.info.fwhm,
                    hfd:            info.stars.info.hfd,
                    fwhm_angular:   info.stars.info.fwhm_angular,
                    bad_fwhm:       !info.stars.info.fwhm_is_ok,
                    stars_ovality:  info.stars.info.ovality,
                    bad_ovality:    !info.stars.info.ovality_is_ok,
                    background:     info.image.bg_percent,
                    noise:          info.image.raw_noise.map(|n| 100.0 * n / info.image.max_value as f32),
                    stars_count:    info.stars.items.len(),
                    offset:         info.stars.offset.clone(),
                    bad_offset:     !info.stars.offset_is_ok,
                    calibr_methods: info.image.calibr_methods,
                };
                self.light_history.borrow_mut().push(history_item);
                self.update_light_history_table();
                self.set_hist_tab_active(Self::HIST_TAB_LIGHT);
            }
            FrameProcessResultData::FrameInfo
            if is_mode_current(false) => {
                self.show_image_info();
            }
            FrameProcessResultData::FrameInfoLiveRes
            if is_mode_current(true) => {
                self.show_image_info();
            }
            _ => {}
        }
    }

    fn show_histogram_stat(&self) {
        let options = self.options.read().unwrap();
        let hist = match options.preview.source {
            PreviewSource::OrigFrame =>
                self.core.cur_frame().raw_hist.read().unwrap(),
            PreviewSource::LiveStacking =>
                self.core.live_stacking().hist.read().unwrap(),
        };
        drop(options);
        let ui_options = self.ui_options.borrow();
        let max = hist.max as f32;
        let show_chan_data = |
            chan:     &Option<HistogramChan>,
            l_cap:    &gtk::Label,
            l_mean:   &gtk::Label,
            l_median: &gtk::Label,
            l_dev:    &gtk::Label
        | {
            if let Some(chan) = chan.as_ref() {
                let median = chan.median();
                if ui_options.hist_percents {
                    l_mean.set_label(&format!("{:.1}%", 100.0 * chan.mean / max));
                    l_median.set_label(&format!("{:.1}%", 100.0 * median as f32 / max));
                    l_dev.set_label(&format!("{:.1}%", 100.0 * chan.std_dev / max));
                } else {
                    l_mean.set_label(&format!("{:.1}", chan.mean));
                    l_median.set_label(&format!("{:.1}", median));
                    l_dev.set_label(&format!("{:.1}", chan.std_dev));
                }
            }
            l_cap.set_visible(chan.is_some());
            l_mean.set_visible(chan.is_some());
            l_median.set_visible(chan.is_some());
            l_dev.set_visible(chan.is_some());
        };

        show_chan_data(
            &hist.r,
            &self.widgets.stat.l_r_cap,
            &self.widgets.stat.l_r_mean,
            &self.widgets.stat.l_r_median,
            &self.widgets.stat.l_r_dev
        );

        show_chan_data(
            &hist.g,
            &self.widgets.stat.l_g_cap,
            &self.widgets.stat.l_g_mean,
            &self.widgets.stat.l_g_median,
            &self.widgets.stat.l_g_dev
        );

        show_chan_data(
            &hist.b,
            &self.widgets.stat.l_b_cap,
            &self.widgets.stat.l_b_mean,
            &self.widgets.stat.l_b_median,
            &self.widgets.stat.l_b_dev
        );

        show_chan_data(
            &hist.l,
            &self.widgets.stat.l_l_cap,
            &self.widgets.stat.l_l_mean,
            &self.widgets.stat.l_l_median,
            &self.widgets.stat.l_l_dev
        );
    }

    fn repaint_histogram(&self) {
        self.widgets.stat.da_histogram.queue_draw();
    }

    fn handler_draw_histogram(
        &self,
        area: &gtk::DrawingArea,
        cr:   &cairo::Context
    ) ->anyhow::Result<()> {
        let options = self.options.read().unwrap();
        let hist = match options.preview.source {
            PreviewSource::OrigFrame =>
                self.core.cur_frame().raw_hist.read().unwrap(),
            PreviewSource::LiveStacking =>
                self.core.live_stacking().hist.read().unwrap(),
        };
        drop(options);

        let font_size_pt = 8.0;
        let (_, dpmm_y) = get_widget_dpmm(area)
            .unwrap_or((DEFAULT_DPMM, DEFAULT_DPMM));
        let font_size_px = font_size_to_pixels(FontSize::Pt(font_size_pt), dpmm_y);
        cr.set_font_size(font_size_px);

        let ui_options = self.ui_options.borrow();
        draw_histogram(
            &hist,
            area,
            cr,
            area.allocated_width(),
            area.allocated_height(),
            ui_options.hist_log_y,
        )?;
        Ok(())
    }

    const HIST_TAB_LIGHT: u32 = 0;
    const HIST_TAB_CALIBR: u32 = 1;
    const HIST_TAB_PLOTS: u32 = 2;

    fn set_hist_tab_active(&self, tab_index: u32) {
        if self.widgets.history.nb_hist.current_page() == Some(Self::HIST_TAB_PLOTS) {
            return;
        }
        self.widgets.history.nb_hist.set_current_page(Some(tab_index));
    }

    fn mode_type_to_history_str(mode_type: ModeType) -> &'static str {
        match mode_type {
            ModeType::OpeningImgFile    => "File",
            ModeType::SingleShot        => "S",
            ModeType::LiveView          => "LV",
            ModeType::SavingRawFrames   => "RAW",
            ModeType::LiveStacking      => "LS",
            ModeType::Focusing          => "F",
            ModeType::DitherCalibr      => "MC",
            ModeType::Goto|
            ModeType::CapturePlatesolve => "PS",
            ModeType::DefectPixels      => "Pix",
            ModeType::MasterDark|
            ModeType::MasterBias        => "Master",
            ModeType::PolarAlignment    => "PA",
            _                           => "???",
        }
    }

    fn update_light_history_table(&self) {
        let tree = &self.widgets.history.tv_light;
        let model = match tree.model() {
            Some(model) => {
                model.downcast::<gtk::ListStore>().unwrap()
            },
            None => {
                init_list_store_model_for_treeview(tree, &[
                    /* 0 */  ("Mode",       String::static_type(), "text"),
                    /* 1 */  ("Time",       String::static_type(), "text"),
                    /* 2 */  ("FWHM / HFD", String::static_type(), "markup"),
                    /* 3 */  ("Ovality",    String::static_type(), "markup"),
                    /* 4 */  ("Stars",      u32::static_type(),    "text"),
                    /* 5 */  ("Noise",      String::static_type(), "text"),
                    /* 6 */  ("Background", String::static_type(), "text"),
                    /* 7 */  ("Calibr.",    String::static_type(), "text"),
                    /* 8 */  ("Offs.X",     String::static_type(), "markup"),
                    /* 9 */  ("Offs.Y",     String::static_type(), "markup"),
                    /* 10 */ ("Rot.",       String::static_type(), "markup"),
                ])
            },
        };
        let items = self.light_history.borrow();
        if get_model_row_count(model.upcast_ref()) > items.len() {
            model.clear();
        }
        let models_row_cnt = get_model_row_count(model.upcast_ref());
        let to_index = items.len();
        let make_bad_str = |s: &str| -> String {
            format!(r##"<span color="#FF4040"><b>{}</b></span>"##, s)
        };
        for item in &items[models_row_cnt..to_index] {
            let mode_type_str = Self::mode_type_to_history_str(item.mode_type);
            let local_time_str =
                if let Some(time) = item.time {
                    let local_time: DateTime<Local> = DateTime::from(time);
                    local_time.format("%H:%M:%S").to_string()
                } else {
                    String::new()
                };
            let mut fwhm_str = item.fwhm
                .map(|v| format!("{:.1}", v))
                .unwrap_or_else(String::new);
            if item.bad_fwhm {
                fwhm_str = make_bad_str(&fwhm_str);
            }
            if let Some(fwhm_angular) = item.fwhm_angular {
                let fwhm_in_degrees = radian_to_degree(fwhm_angular as f64);
                let fwhm_in_seconds = fwhm_in_degrees * (60.0 * 60.0);
                fwhm_str += &format!("({:.1}\")", fwhm_in_seconds);
            }
            if let Some(hfd) = item.hfd {
                fwhm_str += &format!(" / {:.1}", hfd);
            }

            let mut ovality_str = item.stars_ovality
                .map(|v| format!("{:.1}", v))
                .unwrap_or_else(String::new);
            if item.bad_ovality {
                ovality_str = make_bad_str(&ovality_str);
            }
            let stars_cnt = item.stars_count as u32;
            let noise_str = item.noise
                .map(|v| format!("{:.3}%", v))
                .unwrap_or_else(|| "???".to_string());
            let bg_str = format!("{:.1}%", item.background);

            let (x_str, y_str, angle_str) = if let Some(offset) = &item.offset {
                let x_str = if !item.bad_offset {
                    format!("{:.1}", offset.x)
                } else {
                    make_bad_str("???")
                };
                let y_str = if !item.bad_offset {
                    format!("{:.1}", offset.y)
                } else {
                    make_bad_str("???")
                };
                let angle_str = if !item.bad_offset {
                    format!("{:.1}", radian_to_degree(offset.angle))
                } else {
                    make_bad_str("???")
                };
                (x_str, y_str, angle_str)
            } else {
                (String::new(), String::new(), String::new())
            };
            let calibr_str = Self::calibr_method_to_str(item.calibr_methods);
            let last_is_selected =
                get_list_view_selected_row(tree).map(|v| v+1) ==
                Some(models_row_cnt as i32);
            let last_iter = model.insert_with_values(None, &[
                (0, &mode_type_str),
                (1, &local_time_str),
                (2, &fwhm_str),
                (3, &ovality_str),
                (4, &stars_cnt),
                (5, &noise_str),
                (6, &bg_str),
                (7, &calibr_str),
                (8, &x_str),
                (9, &y_str),
                (10, &angle_str),
            ]);
            if last_is_selected || models_row_cnt == 0 {
                // Select and scroll to last row
                tree.selection().select_iter(&last_iter);
                if let [path] = tree.selection().selected_rows().0.as_slice() {
                    tree.set_cursor(path, Option::<&gtk::TreeViewColumn>::None, false);
                }
            }
        }
    }

    fn calibr_method_to_str(cm: CalibrMethods) -> String {
        let mut result = String::new();
        if cm.contains(CalibrMethods::BY_DARK) {
            result += "D";
        }
        if cm.contains(CalibrMethods::BY_BIAS) {
            result += "B";
        }
        if cm.contains(CalibrMethods::DEFECTIVE_PIXELS) {
            result += "P";
        }
        if cm.contains(CalibrMethods::BY_FLAT) {
            result += "F";
        }
        if cm.contains(CalibrMethods::HOT_PIXELS_SEARCH) {
            result += "S";
        }
        result
    }

    fn update_calibr_history_table(&self) {
        let tree = &self.widgets.history.tv_calbr;
        let model = match tree.model() {
            Some(model) => {
                model.downcast::<gtk::ListStore>().unwrap()
            },
            None => {
                init_list_store_model_for_treeview(tree, &[
                    /* 0 */  ("Time",     String::static_type(), "text"),
                    /* 1 */  ("Mode",     String::static_type(), "text"),
                    /* 2 */  ("Type",     String::static_type(), "text"),
                    /* 3 */  ("Mean",     String::static_type(), "text"),
                    /* 4 */  ("Median",   String::static_type(), "text"),
                    /* 5 */  ("Std.dev.", String::static_type(), "text"),
                    /* 6 */  ("Calibr.",  String::static_type(), "text"),
                ])
            },
        };
        let items = self.calibr_history.borrow();
        if get_model_row_count(model.upcast_ref()) > items.len() {
            model.clear();
        }
        let models_row_cnt = get_model_row_count(model.upcast_ref());
        let to_index = items.len();
        for item in &items[models_row_cnt..to_index] {
            let local_time_str =
                if let Some(time) = &item.time {
                    let local_time: DateTime<Local> = DateTime::from(*time);
                    local_time.format("%H:%M:%S").to_string()
                } else {
                    String::new()
                };

            let mode_type_str = Self::mode_type_to_history_str(item.mode_type);
            let type_str = item.frame_type.to_str();
            let mean_str = format!("{:.1}", item.mean);
            let median_str = format!("{}", item.median);
            let dev_str = format!("{:.1}", item.std_dev);
            let calibr_str = Self::calibr_method_to_str(item.calibr_methods);

            let last_is_selected =
                get_list_view_selected_row(tree).map(|v| v+1) ==
                Some(models_row_cnt as i32);

            let last_iter = model.insert_with_values(None, &[
                (0, &local_time_str),
                (1, &mode_type_str),
                (2, &type_str),
                (3, &mean_str),
                (4, &median_str),
                (5, &dev_str),
                (6, &calibr_str),
            ]);
            if last_is_selected || models_row_cnt == 0 {
                // Select and scroll to last row
                tree.selection().select_iter(&last_iter);
                if let [path] = tree.selection().selected_rows().0.as_slice() {
                    tree.set_cursor(path, Option::<&gtk::TreeViewColumn>::None, false);
                }
            }
        }
    }

    fn handler_action_clear_light_history(&self) {
        let cur_page = self.widgets.history.nb_hist.current_page();
        match cur_page {
            Some(Self::HIST_TAB_LIGHT) => {
                self.light_history.borrow_mut().clear();
                self.update_light_history_table();
            }

            Some(Self::HIST_TAB_CALIBR) => {
                self.calibr_history.borrow_mut().clear();
                self.update_calibr_history_table();
            }

            _ => {},
        }
    }

    fn handler_action_open_image(self: &Rc<Self>) {
        let fc = gtk::FileChooserDialog::builder()
            .action(gtk::FileChooserAction::Open)
            .title("Select image file to open")
            .modal(true)
            .transient_for(&self.window)
            .build();
        add_ok_and_cancel_buttons(
            fc.upcast_ref::<gtk::Dialog>(),
            "_Open",   gtk::ResponseType::Accept,
            "_Cancel", gtk::ResponseType::Cancel
        );
        fc.connect_response(clone!(@weak self as self_ => move |file_chooser, response| {
            if response == gtk::ResponseType::Accept {
                exec_and_show_error(Some(&self_.window), || {
                    let Some(file_name) = file_chooser.file() else { return Ok(()); };
                    let Some(file_name) = file_name.path() else { return Ok(()); };
                    self_.main_ui.get_all_options();
                    self_.core.open_image_from_file(&file_name)?;
                    Ok(())
                });
            }
            file_chooser.close();
        }));
        fc.show();
    }

    fn correct_preview_source(&self) {
        let mode_type = self.core.mode_data().mode.get_type();
        let cb_preview_src_aid = match mode_type {
            ModeType::LiveStacking => "live",
            ModeType::Waiting      => return,
            _                      => "frame",
        };
        self.widgets.ctrl.cb_src.set_active_id(Some(cb_preview_src_aid));
    }
}