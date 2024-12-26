use std::{rc::Rc, sync::*, cell::{RefCell, Cell}, path::PathBuf};
use chrono::{DateTime, Local, Utc};
use gtk::{cairo, glib::{self, clone}, prelude::*};
use serde::{Serialize, Deserialize};
use crate::{
    core::{core::*, events::*, frame_processing::*},
    image::{histogram::*, image::RgbU8Data, info::*, io::save_image_to_tif_file, raw::{CalibrMethods, FrameType}, stars_offset::Offset},
    options::*,
    utils::{gtk_utils::{self, *}, io_utils::*, log_utils::*}
};
use super::{ui_main::*, utils::*};


pub fn init_ui(
    _app:     &gtk::Application,
    builder:  &gtk::Builder,
    main_ui:  &Rc<MainUi>,
    options:  &Arc<RwLock<Options>>,
    core:     &Arc<Core>,
    handlers: &mut MainUiEventHandlers,
) {
    let window = builder.object::<gtk::ApplicationWindow>("window").unwrap();

    let mut ui_options = UiOptions::default();
    gtk_utils::exec_and_show_error(&window, || {
        load_json_from_config_file(&mut ui_options, PreviewUi::CONF_FN)?;
        Ok(())
    });

    let obj = Rc::new(PreviewUi {
        main_ui:            Rc::clone(main_ui),
        builder:            builder.clone(),
        window:             window.clone(),
        core:               Arc::clone(core),
        options:            Arc::clone(options),
        ui_options:         RefCell::new(ui_options),
        preview_scroll_pos: RefCell::new(None),
        closed:             Cell::new(false),
        light_history:      RefCell::new(Vec::new()),
        calibr_history:     RefCell::new(Vec::new()),
        flat_info:          RefCell::new(FlatImageInfo::default()),
        self_:              RefCell::new(None),
    });

    *obj.self_.borrow_mut() = Some(Rc::clone(&obj));

    obj.init_widgets();
    obj.show_ui_options();

    obj.connect_widgets_events();
    obj.connect_core_events();
    obj.connect_main_ui_events(handlers);
    obj.connect_img_mouse_scroll_events();

    obj.update_light_history_table();
    obj.update_calibr_history_table();
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(default)]
struct UiOptions {
    hist_log_y:     bool,
    hist_percents:  bool,
    flat_percents:  bool,
}

impl Default for UiOptions {
    fn default() -> Self {
        Self {
            hist_log_y:     false,
            hist_percents:  true,
            flat_percents:  true,
        }
    }
}

enum MainThreadEvent {
    Core(Event),
}

struct LightHistoryItem {
    mode_type:      ModeType,
    time:           Option<DateTime<Utc>>,
    stars_fwhm:     Option<f32>,
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

struct PreviewUi {
    main_ui:            Rc<MainUi>,
    builder:            gtk::Builder,
    window:             gtk::ApplicationWindow,
    options:            Arc<RwLock<Options>>,
    core:               Arc<Core>,
    ui_options:         RefCell<UiOptions>,
    preview_scroll_pos: RefCell<Option<((f64, f64), (f64, f64))>>,
    light_history:      RefCell<Vec<LightHistoryItem>>,
    calibr_history:     RefCell<Vec<CalibrHistoryItem>>,
    closed:             Cell<bool>,
    flat_info:          RefCell<FlatImageInfo>,
    self_:              RefCell<Option<Rc<PreviewUi>>>,
}

impl Drop for PreviewUi {
    fn drop(&mut self) {
        log::info!("PreviewUi dropped");
    }
}

impl PreviewUi {
    const CONF_FN: &'static str = "ui_prevuew";

    fn init_widgets(&self) {
        let scl_dark = self.builder.object::<gtk::Scale>("scl_dark").unwrap();
        scl_dark.set_range(0.0, 1.0);
        scl_dark.set_increments(0.01, 0.1);
        scl_dark.set_round_digits(2);
        scl_dark.set_digits(2);

        let (dpimm_x, _) = gtk_utils::get_widget_dpmm(&self.window)
            .unwrap_or((DEFAULT_DPMM, DEFAULT_DPMM));
        scl_dark.set_width_request((40.0 * dpimm_x) as i32);

        let scl_highlight = self.builder.object::<gtk::Scale>("scl_highlight").unwrap();
        scl_highlight.set_range(0.0, 1.0);
        scl_highlight.set_increments(0.01, 0.1);
        scl_highlight.set_round_digits(2);
        scl_highlight.set_digits(2);

        let scl_gamma = self.builder.object::<gtk::Scale>("scl_gamma").unwrap();
        scl_gamma.set_range(1.0, 5.0);
        scl_gamma.set_digits(1);
        scl_gamma.set_increments(0.1, 1.0);
        scl_gamma.set_round_digits(1);
        scl_gamma.set_digits(1);

        let configure_wb_scale = |name: &str| {
            let scale = self.builder.object::<gtk::Scale>(name).unwrap();
            scale.set_range(0.0, 2.0);
            scale.set_increments(0.1, 0.5);
            scale.set_round_digits(1);
            scale.set_digits(1);
        };

        configure_wb_scale("scl_wb_red");
        configure_wb_scale("scl_wb_green");
        configure_wb_scale("scl_wb_blue");
    }

    fn handler_closing(&self) {
        self.closed.set(true);

        _ = self.core.stop_img_process_thread();
        _ = self.core.abort_active_mode();

        self.get_ui_options_from_widgets();

        let ui_options = self.ui_options.borrow();
        _ = save_json_to_config::<UiOptions>(&ui_options, Self::CONF_FN);
        drop(ui_options);

        *self.self_.borrow_mut() = None;
    }

    fn connect_widgets_events(self: &Rc<Self>) {
        gtk_utils::connect_action   (&self.window, self, "save_image_preview",  Self::handler_action_save_image_preview);
        gtk_utils::connect_action   (&self.window, self, "save_image_linear",   Self::handler_action_save_image_linear);
        gtk_utils::connect_action   (&self.window, self, "clear_light_history", Self::handler_action_clear_light_history);

        let ch_hist_logy = self.builder.object::<gtk::CheckButton>("ch_hist_logy").unwrap();
        ch_hist_logy.connect_active_notify(clone!(@weak self as self_ => move |chb| {
            let Ok(mut ui_options) = self_.ui_options.try_borrow_mut() else { return; };
            ui_options.hist_log_y = chb.is_active();
            self_.repaint_histogram();
        }));

        let ch_stat_percents = self.builder.object::<gtk::CheckButton>("ch_stat_percents").unwrap();
        ch_stat_percents.connect_active_notify(clone!(@weak self as self_ => move |chb| {
            let Ok(mut ui_options) = self_.ui_options.try_borrow_mut() else { return; };
            ui_options.hist_percents = chb.is_active();
            drop(ui_options);
            self_.show_histogram_stat();
        }));

        let chb_flat_percents = self.builder.object::<gtk::CheckButton>("chb_flat_percents").unwrap();
        chb_flat_percents.connect_active_notify(clone!(@weak self as self_ => move |chb| {
            let Ok(mut ui_options) = self_.ui_options.try_borrow_mut() else { return; };
            ui_options.flat_percents = chb.is_active();
            drop(ui_options);
            self_.show_flat_info();
        }));

        let cb_preview_src = self.builder.object::<gtk::ComboBoxText>("cb_preview_src").unwrap();
        cb_preview_src.connect_active_id_notify(clone!(@weak self as self_ => move |cb| {
            let Ok(mut options) = self_.options.try_write() else { return; };
            let source = PreviewSource::from_active_id(cb.active_id().as_deref());
            options.preview.source = source;
            drop(options);
            self_.create_and_show_preview_image();
            self_.repaint_histogram();
            self_.show_histogram_stat();
            self_.show_image_info();
        }));

        let sw_preview_img = self.builder.object::<gtk::Widget>("sw_preview_img").unwrap();
        sw_preview_img.connect_size_allocate(clone!(@weak self as self_ => move |_, rect| {
            let Ok(mut options) = self_.options.try_write() else { return; };
            options.preview.widget_width = rect.width() as usize;
            options.preview.widget_height = rect.height() as usize;
        }));

        let cb_preview_scale = self.builder.object::<gtk::ComboBoxText>("cb_preview_scale").unwrap();
        cb_preview_scale.connect_active_id_notify(clone!(@weak self as self_ => move |cb| {
            let Ok(mut options) = self_.options.try_write() else { return; };
            let scale = PreviewScale::from_active_id(cb.active_id().as_deref());
            options.preview.scale = scale;
            drop(options);
            self_.create_and_show_preview_image();
        }));

        let cb_preview_color = self.builder.object::<gtk::ComboBoxText>("cb_preview_color").unwrap();
        cb_preview_color.connect_active_id_notify(clone!(@weak self as self_ => move |cb| {
            let Ok(mut options) = self_.options.try_write() else { return; };
            let color = PreviewColor::from_active_id(cb.active_id().as_deref());
            options.preview.color = color;
            drop(options);
            self_.create_and_show_preview_image();
        }));

        let scl_dark = self.builder.object::<gtk::Scale>("scl_dark").unwrap();
        scl_dark.connect_value_changed(clone!(@weak self as self_ => move |scl| {
            let Ok(mut options) = self_.options.try_write() else { return; };
            options.preview.dark_lvl = scl.value();
            drop(options);
            self_.create_and_show_preview_image();
        }));

        let scl_highlight = self.builder.object::<gtk::Scale>("scl_highlight").unwrap();
        scl_highlight.connect_value_changed(clone!(@weak self as self_ => move |scl| {
            let Ok(mut options) = self_.options.try_write() else { return; };
            options.preview.light_lvl = scl.value();
            drop(options);
            self_.create_and_show_preview_image();
        }));

        let scl_gamma = self.builder.object::<gtk::Scale>("scl_gamma").unwrap();
        scl_gamma.connect_value_changed(clone!(@weak self as self_ => move |scl| {
            let Ok(mut options) = self_.options.try_write() else { return; };
            options.preview.gamma = scl.value();
            drop(options);
            self_.create_and_show_preview_image();
        }));

        let chb_rem_grad = self.builder.object::<gtk::CheckButton>("chb_rem_grad").unwrap();
        chb_rem_grad.connect_active_notify(clone!(@weak self as self_ => move |chb| {
            let Ok(mut options) = self_.options.try_write() else { return; };
            options.preview.remove_grad = chb.is_active();
            drop(options);
            self_.create_and_show_preview_image();
        }));

        let da_histogram = self.builder.object::<gtk::DrawingArea>("da_histogram").unwrap();
        da_histogram.connect_draw(
            clone!(@weak self as self_ => @default-return glib::Propagation::Proceed,
            move |area, cr| {
                gtk_utils::exec_and_show_error(&self_.window, || {
                    self_.handler_draw_histogram(area, cr)?;
                    Ok(())
                });
                glib::Propagation::Proceed
            })
        );

        let chb_wb_auto = self.builder.object::<gtk::CheckButton>("chb_wb_auto").unwrap();
        chb_wb_auto.connect_active_notify(clone!(@weak self as self_ => move |chb| {
            let Ok(mut options) = self_.options.try_write() else { return; };
            options.preview.wb_auto = chb.is_active();
            drop(options);
            self_.correct_widgets_props();
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

    fn connect_main_ui_events(self: &Rc<Self>, handlers: &mut MainUiEventHandlers) {
        handlers.subscribe(clone!(@weak self as self_ => move |event| {
            match event {
                UiEvent::FullScreen(full_screen) =>
                    self_.set_full_screen_mode(full_screen),

                UiEvent::ProgramClosing =>
                    self_.handler_closing(),

                UiEvent::OptionsHasShown =>
                    self_.correct_widgets_props(),

                _ => {}
            }
        }));
    }

    fn connect_img_mouse_scroll_events(self: &Rc<Self>) {
        let eb_preview_img = self.builder.object::<gtk::EventBox>("eb_preview_img").unwrap();
        let sw_preview_img = self.builder.object::<gtk::ScrolledWindow>("sw_preview_img").unwrap();

        eb_preview_img.connect_button_press_event(
            clone!(@weak self as self_, @weak sw_preview_img => @default-return glib::Propagation::Proceed,
            move |_, evt| {
                if evt.button() == gtk::gdk::ffi::GDK_BUTTON_PRIMARY as u32 {
                    let hadjustment = sw_preview_img.hadjustment();
                    let vadjustment = sw_preview_img.vadjustment();
                    *self_.preview_scroll_pos.borrow_mut() = Some((
                        evt.root(),
                        (hadjustment.value(), vadjustment.value())
                    ));
                }
                glib::Propagation::Proceed
            })
        );

        eb_preview_img.connect_button_release_event(
            clone!(@weak self as self_ => @default-return glib::Propagation::Proceed,
            move |_, evt| {
                if evt.button() == gtk::gdk::ffi::GDK_BUTTON_PRIMARY as u32 {
                    *self_.preview_scroll_pos.borrow_mut() = None;
                }
                glib::Propagation::Proceed
            })
        );

        eb_preview_img.connect_motion_notify_event(
            clone!(@weak self as self_, @weak sw_preview_img => @default-return glib::Propagation::Proceed,
            move |_, evt| {
                const SCROLL_SPEED: f64 = 2.0;
                if let Some((start_mouse_pos, start_scroll_pos)) = *self_.preview_scroll_pos.borrow() {
                    let new_pos = evt.root();
                    let move_x = new_pos.0 - start_mouse_pos.0;
                    let move_y = new_pos.1 - start_mouse_pos.1;
                    let hadjustment = sw_preview_img.hadjustment();
                    hadjustment.set_value(start_scroll_pos.0 - SCROLL_SPEED*move_x);
                    let vadjustment = sw_preview_img.vadjustment();
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

            _ => {},
        }
    }

    fn show_ui_options(&self) {
        let ui = gtk_utils::UiHelper::new_from_builder(&self.builder);
        let options = self.ui_options.borrow();

        ui.set_prop_bool("ch_hist_logy.active",      options.hist_log_y);
        ui.set_prop_bool("ch_stat_percents.active",  options.hist_percents);
        ui.set_prop_bool("chb_flat_percents.active", options.flat_percents);
    }

    fn get_ui_options_from_widgets(&self) {
        let ui = gtk_utils::UiHelper::new_from_builder(&self.builder);
        let mut options = self.ui_options.borrow_mut();

        options.hist_log_y    = ui.prop_bool("ch_hist_logy.active");
        options.hist_percents = ui.prop_bool("ch_stat_percents.active");
        options.flat_percents = ui.prop_bool("chb_flat_percents.active");
    }

    fn correct_widgets_props(&self) {
        let ui = gtk_utils::UiHelper::new_from_builder(&self.builder);

        let auto_color_checked = ui.prop_bool("chb_wb_auto.active");

        ui.enable_widgets(false, &[
            ("l_wb_red",     !auto_color_checked),
            ("scl_wb_red",   !auto_color_checked),
            ("l_wb_green",   !auto_color_checked),
            ("scl_wb_green", !auto_color_checked),
            ("l_wb_blue",    !auto_color_checked),
            ("scl_wb_blue",  !auto_color_checked),
        ]);
    }

    fn show_image_info(&self) {
        let info = match self.options.read().unwrap().preview.source {
            PreviewSource::OrigFrame =>
                self.core.cur_frame().info.read().unwrap(),
            PreviewSource::LiveStacking =>
                self.core.live_stacking().info.read().unwrap(),
        };

        let ui = gtk_utils::UiHelper::new_from_builder(&self.builder);
        let update_info_panel_vis = |is_light_info: bool, is_flat_info: bool, is_raw_info: bool| {
            ui.show_widgets(&[
                ("bx_light_info", is_light_info),
                ("bx_flat_info",  is_flat_info),
                ("bx_raw_info",   is_raw_info),
            ]);
        };

        match &*info {
            ResultImageInfo::LightInfo(info) => {
                ui.set_prop_str("e_info_exp.text", Some(&seconds_to_total_time_str(info.exposure, true)));
                match info.stars.fwhm {
                    Some(value) => ui.set_prop_str("e_fwhm.text", Some(&format!("{:.1}", value))),
                    None        => ui.set_prop_str("e_fwhm.text", Some("")),
                }
                match info.stars.ovality {
                    Some(value) => ui.set_prop_str("e_ovality.text", Some(&format!("{:.1}", value))),
                    None        => ui.set_prop_str("e_ovality.text", Some("")),
                }
                let stars_cnt = info.stars.items.len();
                let overexp_stars = info.stars.items.iter().filter(|s| s.overexposured).count();
                ui.set_prop_str("e_stars.text", Some(&format!("{} ({})", stars_cnt, overexp_stars)));
                let bg = 100_f64 * info.background as f64 / info.max_value as f64;
                ui.set_prop_str("e_background.text", Some(&format!("{:.2}%", bg)));
                let noise = 100_f64 * info.noise as f64 / info.max_value as f64;
                ui.set_prop_str("e_noise.text", Some(&format!("{:.4}%", noise)));
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
                ui.set_prop_str("e_aver.text", Some(&aver_text));
                let median_text = format!(
                    "{} ({:.1}%)",
                    info.median,
                    100.0 * info.median as f64 / info.max_value as f64
                );
                ui.set_prop_str("e_median.text", Some(&median_text));
                let dev_text = format!(
                    "{:.1} ({:.3}%)",
                    info.std_dev,
                    100.0 * info.std_dev / info.max_value as f32
                );
                ui.set_prop_str("e_std_dev.text", Some(&dev_text));
                update_info_panel_vis(false, false, true);
            },
            _ => {
                update_info_panel_vis(false, false, false);
            },
        }
    }

    fn show_flat_info(&self) {
        let info = self.flat_info.borrow();
        let ui = gtk_utils::UiHelper::new_from_builder(&self.builder);
        let ui_options = self.ui_options.borrow();
        let show_chan = |label_id, entry_id, item: Option<&FlatInfoChan>| {
            if let Some(item) = item {
                let text =
                    if ui_options.flat_percents {
                        let percent_aver = 100.0 * item.aver / info.max_value as f32;
                        let percent_max = 100.0 * item.max as f64 / info.max_value as f64;
                        format!("{:.1}% / {:.1}%", percent_aver, percent_max)
                    } else {
                        format!("{:.1} / {}", item.aver, item.max)
                    };
                ui.set_prop_str_ex(entry_id, "text", Some(&text));
            }
            ui.show_widgets(&[
                (label_id, item.is_some()),
                (entry_id, item.is_some()),
            ]);
        };

        show_chan("l_flat_r", "e_flat_r", info.r.as_ref());
        show_chan("l_flat_g", "e_flat_g", info.g.as_ref());
        show_chan("l_flat_b", "e_flat_b", info.b.as_ref());
        show_chan("l_flat_l", "e_flat_l", info.l.as_ref());
    }

    fn create_and_show_preview_image(&self) {
        let options = self.options.read().unwrap();
        let preview_params = options.preview.preview_params();
        let (image, hist) = match options.preview.source {
            PreviewSource::OrigFrame =>
                (&*self.core.cur_frame().image, &self.core.cur_frame().img_hist),
            PreviewSource::LiveStacking =>
                (&self.core.live_stacking().image, &self.core.live_stacking().hist),
        };
        drop(options);
        let image = image.read().unwrap();
        let hist = hist.read().unwrap();
        let rgb_bytes = get_rgb_data_from_preview_image(
            &image,
            &hist,
            &preview_params
        );
        if let Some(rgb_bytes) = rgb_bytes {
            self.show_preview_image(Some(&rgb_bytes), None);
        } else {
            self.show_preview_image(None, None);
        }
    }

    fn show_preview_image(
        &self,
        rgb_bytes:  Option<&RgbU8Data>,
        src_params: Option<&PreviewParams>,
    ) {
        let preview_options = self.options.read().unwrap().preview.clone();
        let pp = preview_options.preview_params();
        if src_params.is_some() && src_params != Some(&pp) {
            self.create_and_show_preview_image();
            return;
        }

        let img_preview = self.builder.object::<gtk::Image>("img_preview").unwrap();

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

            let (img_width, img_height) = pp.img_size.get_preview_img_size(
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
            img_preview.set_pixbuf(Some(&pixbuf));
            is_color_image = rgb_bytes.is_color_image;
        } else {
            img_preview.clear();
            img_preview.set_pixbuf(None);
        }

        let ui = gtk_utils::UiHelper::new_from_builder(&self.builder);
        ui.enable_widgets(
            false,
            &[("cb_preview_color", is_color_image)]
        );
    }

    fn handler_action_save_image_preview(&self) {
        gtk_utils::exec_and_show_error(&self.window, || {
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
                Utc::now().format("%Y-%m-%d_%H-%M-%S").to_string()
            );
            let Some(file_name) = gtk_utils::select_file_name_to_save(
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
            let rgb_data = get_rgb_data_from_preview_image(&image, &hist, &preview_params);
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
        gtk_utils::exec_and_show_error(&self.window, || {
            let options = self.options.read().unwrap();
            let preview_source = options.preview.source.clone();
            drop(options);
            let ask_to_select_name = |fn_prefix: &str| -> Option<PathBuf> {
                let def_file_name = format!(
                    "{}_{}.tif",
                    fn_prefix,
                    Utc::now().format("%Y-%m-%d_%H-%M-%S").to_string()
                );
                gtk_utils::select_file_name_to_save(
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
        let ui = gtk_utils::UiHelper::new_from_builder(&self.builder);

        let show_resolution_info = |width, height| {
            ui.set_prop_str(
                "e_res_info.text",
                Some(&format!("{} x {}", width, height))
            );
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
                show_resolution_info(img.image_width, img.image_height);
            }
            FrameProcessResultData::PreviewLiveRes(img)
            if is_mode_current(true) => {
                self.show_preview_image(Some(&img.rgb_data), Some(&img.params));
                show_resolution_info(img.image_width, img.image_height);
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
                        time:           raw_frame_info.time.clone(),
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
                    time:           info.time.clone(),
                    stars_fwhm:     info.stars.fwhm,
                    bad_fwhm:       !info.stars.fwhm_is_ok,
                    stars_ovality:  info.stars.ovality,
                    bad_ovality:    !info.stars.ovality_is_ok,
                    background:     info.bg_percent,
                    noise:          info.raw_noise.map(|n| 100.0 * n / info.max_value as f32),
                    stars_count:    info.stars.items.len(),
                    offset:         info.stars_offset.clone(),
                    bad_offset:     !info.offset_is_ok,
                    calibr_methods: info.calibr_methods.clone(),
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
            FrameProcessResultData::MasterSaved { frame_type: FrameType::Flats, file_name } => {
                ui.set_fch_path("fch_master_flat", Some(&file_name));
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
        let ui = gtk_utils::UiHelper::new_from_builder(&self.builder);
        let max = hist.max as f32;
        let show_chan_data = |chan: &Option<HistogramChan>, l_cap, l_mean, l_median, l_dev| {
            if let Some(chan) = chan.as_ref() {
                let median = chan.median();
                if ui_options.hist_percents {
                    ui.set_prop_str_ex(
                        l_mean, "label",
                        Some(&format!("{:.1}%", 100.0 * chan.mean / max))
                    );
                    ui.set_prop_str_ex(
                        l_median, "label",
                        Some(&format!("{:.1}%", 100.0 * median as f32 / max))
                    );
                    ui.set_prop_str_ex(
                        l_dev, "label",
                        Some(&format!("{:.1}%", 100.0 * chan.std_dev / max))
                    );
                } else {
                    ui.set_prop_str_ex(
                        l_mean, "label",
                        Some(&format!("{:.1}", chan.mean))
                    );
                    ui.set_prop_str_ex(
                        l_median, "label",
                        Some(&format!("{:.1}", median))
                    );
                    ui.set_prop_str_ex(
                        l_dev, "label",
                        Some(&format!("{:.1}", chan.std_dev))
                    );
                }
            }
            ui.show_widgets(&[
                (l_cap,    chan.is_some()),
                (l_mean,   chan.is_some()),
                (l_median, chan.is_some()),
                (l_dev,    chan.is_some()),
            ]);
        };
        show_chan_data(&hist.r, "l_hist_r_cap", "l_hist_r_mean", "l_hist_r_median", "l_hist_r_dev");
        show_chan_data(&hist.g, "l_hist_g_cap", "l_hist_g_mean", "l_hist_g_median", "l_hist_g_dev");
        show_chan_data(&hist.b, "l_hist_b_cap", "l_hist_b_mean", "l_hist_b_median", "l_hist_b_dev");
        show_chan_data(&hist.l, "l_hist_l_cap", "l_hist_l_mean", "l_hist_l_median", "l_hist_l_dev");
    }

    fn repaint_histogram(&self) {
        let da_histogram = self.builder.object::<gtk::DrawingArea>("da_histogram").unwrap();
        da_histogram.queue_draw();
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
        let (_, dpmm_y) = gtk_utils::get_widget_dpmm(area)
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
        let nb_hist = self.builder.object::<gtk::Notebook>("nb_hist").unwrap();
        if nb_hist.current_page() == Some(Self::HIST_TAB_PLOTS) {
            return;
        }
        nb_hist.set_current_page(Some(tab_index));
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
            _                           => "???",
        }
    }

    fn update_light_history_table(&self) {
        let tree: gtk::TreeView = self.builder.object("tv_light_history").unwrap();
        let model = match tree.model() {
            Some(model) => {
                model.downcast::<gtk::ListStore>().unwrap()
            },
            None => {
                gtk_utils::init_list_store_model_for_treeview(&tree, &[
                    /* 0 */  ("Mode",       String::static_type(), "text"),
                    /* 1 */  ("Time",       String::static_type(), "text"),
                    /* 2 */  ("FWHM",       String::static_type(), "markup"),
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
        if gtk_utils::get_model_row_count(model.upcast_ref()) > items.len() {
            model.clear();
        }
        let models_row_cnt = gtk_utils::get_model_row_count(model.upcast_ref());
        let to_index = items.len();
        let make_bad_str = |s: &str| -> String {
            format!(r##"<span color="#FF4040"><b>{}</b></span>"##, s)
        };
        for item in &items[models_row_cnt..to_index] {
            let mode_type_str = Self::mode_type_to_history_str(item.mode_type);
            let local_time_str =
                if let Some(time) = item.time.clone() {
                    let local_time: DateTime<Local> = DateTime::from(time);
                    local_time.format("%H:%M:%S").to_string()
                } else {
                    String::new()
                };
            let mut fwhm_str = item.stars_fwhm
                .map(|v| format!("{:.1}", v))
                .unwrap_or_else(String::new);
            if item.bad_fwhm {
                fwhm_str = make_bad_str(&fwhm_str);
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
                    format!("{:.1}", offset.angle)
                } else {
                    make_bad_str("???")
                };
                (x_str, y_str, angle_str)
            } else {
                (String::new(), String::new(), String::new())
            };
            let calibr_str = Self::calibr_method_to_str(item.calibr_methods);
            let last_is_selected =
                gtk_utils::get_list_view_selected_row(&tree).map(|v| v+1) ==
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
        let tree: gtk::TreeView = self.builder.object("tv_calbr_history").unwrap();
        let model = match tree.model() {
            Some(model) => {
                model.downcast::<gtk::ListStore>().unwrap()
            },
            None => {
                gtk_utils::init_list_store_model_for_treeview(&tree, &[
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
        if gtk_utils::get_model_row_count(model.upcast_ref()) > items.len() {
            model.clear();
        }
        let models_row_cnt = gtk_utils::get_model_row_count(model.upcast_ref());
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
                gtk_utils::get_list_view_selected_row(&tree).map(|v| v+1) ==
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
        let nb_hist = self.builder.object::<gtk::Notebook>("nb_hist").unwrap();

        let cur_page = nb_hist.current_page();
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

    fn set_full_screen_mode(&self, _full_screen: bool) {
        let options = self.options.read().unwrap();
        if options.preview.scale == PreviewScale::FitWindow {
            drop(options);
            gtk::main_iteration_do(true);
            gtk::main_iteration_do(true);
            gtk::main_iteration_do(true);
            self.create_and_show_preview_image();
        }
    }
}