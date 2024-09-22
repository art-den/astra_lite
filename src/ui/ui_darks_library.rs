use std::{cell::RefCell, rc::Rc, sync::{Arc, RwLock}};
use gtk::{glib::clone, prelude::*, glib};
use itertools::Itertools;
use serde::{Deserialize, Serialize};
use crate::{
    core::{
        core::*, mode_darks_library::*
    },
    utils::io_utils::*,
    image::info::seconds_to_total_time_str,
    options::*,
};

use super::gtk_utils;

#[derive(Serialize, Deserialize, Debug)]
#[serde(default)]
struct BinningOptions {
    used:   bool,
    bin1x1: bool,
    bin2x2: bool,
    bin4x4: bool,
}

impl Default for BinningOptions {
    fn default() -> Self {
        Self {
            used:   false,
            bin1x1: true,
            bin2x2: false,
            bin4x4: false
        }
    }
}

impl BinningOptions {
    fn get_binnings(&self) -> Vec<Binning> {
        let mut result = Vec::new();
        if self.used {
            if self.bin1x1 { result.push(Binning::Orig); }
            if self.bin2x2 { result.push(Binning::Bin2); }
            if self.bin4x4 { result.push(Binning::Bin4); }
        }
        result
    }
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(default)]
struct CropOptions {
    used:    bool,
    crop100: bool,
    crop75:  bool,
    crop50:  bool,
    crop33:  bool,
    crop25:  bool,
}

impl Default for CropOptions {
    fn default() -> Self {
        Self {
            used:    false,
            crop100: true,
            crop75:  false,
            crop50:  false,
            crop33:  false,
            crop25:  false
        }
    }
}

impl CropOptions {
    fn get_crops(&self) -> Vec<Crop> {
        let mut result = Vec::new();
        if self.used {
            if self.crop100 { result.push(Crop::None); }
            if self.crop75 { result.push(Crop::P75); }
            if self.crop50 { result.push(Crop::P50); }
            if self.crop33 { result.push(Crop::P33); }
            if self.crop25 { result.push(Crop::P25); }
        }
        result
    }
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(default)]
struct DarkLibOptionsItem {
    used:   bool,
    values: Vec<f64>,
}

impl Default for DarkLibOptionsItem {
    fn default() -> Self {
        Self {
            used:   false,
            values: Vec::new(),
        }
    }
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(default)]
struct DarkLibOptions {
    frames_count: usize,
    temperature:  DarkLibOptionsItem,
    exposure:     DarkLibOptionsItem,
    gain:         DarkLibOptionsItem,
    offset:       DarkLibOptionsItem,
    binning:      BinningOptions,
    crop:         CropOptions,
}

impl Default for DarkLibOptions {
    fn default() -> Self {
        Self {
            frames_count: 30,
            temperature:  DarkLibOptionsItem::default(),
            exposure:     DarkLibOptionsItem::default(),
            gain:         DarkLibOptionsItem::default(),
            offset:       DarkLibOptionsItem::default(),
            binning:      BinningOptions::default(),
            crop:         CropOptions::default(),
        }
    }
}

impl DarkLibOptions {
    fn create_program(&self, cam_opts: &CamOptions) -> Vec<DarkCreationProgramItem> {
        let mut result = Vec::new();

        let mut temperatures = Vec::new();
        if self.temperature.used && !self.temperature.values.is_empty() {
            for t in &self.temperature.values {
                temperatures.push(Some(*t));
            }
        } else if cam_opts.ctrl.enable_cooler {
            temperatures.push(Some(cam_opts.ctrl.temperature));
        } else {
            temperatures.push(None);
        }

        let get = |item: &DarkLibOptionsItem, default: f64| -> Vec<f64> {
            let mut values = Vec::new();
            if item.used && !item.values.is_empty() {
                values.extend_from_slice(&item.values);
            } else {
                values.push(default);
            }
            values
        };

        let exposures = get(&self.exposure, cam_opts.frame.exp_main);
        let gains = get(&self.gain, cam_opts.frame.gain);
        let offsets = get(&self.offset, cam_opts.frame.offset as f64);
        let mut binnings = self.binning.get_binnings();
        if binnings.is_empty() { binnings.push( cam_opts.frame.binning); }
        let mut crops = self.crop.get_crops();
        if crops.is_empty() { crops.push(cam_opts.frame.crop); }

        for t in &temperatures {
            for exp in &exposures {
                for gain in &gains {
                    for offset in &offsets {
                        for bin in &binnings {
                            for crop in &crops {
                                let item = DarkCreationProgramItem {
                                    count:       self.frames_count,
                                    temperature: *t,
                                    exposure:    *exp,
                                    gain:        *gain,
                                    offset:      *offset as i32,
                                    binning:     *bin,
                                    crop:        *crop,
                                };
                                result.push(item);
                            }
                        }
                    }
                }
            }
        }

        return result;
    }
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(default)]
struct DefectPixelsOptions {
    frames_count:     usize,
    temperature_used: bool,
    temperature:      f64,
    exposure_used:    bool,
    exposure:         f64,
    gain_used:        bool,
    gain:             f64,
    offset_used:      bool,
    offset:           f64,
    binning:          BinningOptions,
    crop:             CropOptions,
}

impl Default for DefectPixelsOptions {
    fn default() -> Self {
        Self {
            frames_count:     18,
            temperature_used: false,
            temperature:      5.0,
            exposure_used:    false,
            exposure:         30.0,
            gain_used:        false,
            gain:             100.0,
            offset_used:      false,
            offset:           100.0,
            binning:          BinningOptions::default(),
            crop:             CropOptions::default(),
        }
    }
}

impl DefectPixelsOptions {
    fn create_program(&self, cam_opts: &CamOptions) -> Vec<DarkCreationProgramItem> {
        let mut result = Vec::new();

        let mut binnings = self.binning.get_binnings();
        if binnings.is_empty() { binnings.push( cam_opts.frame.binning); }
        let mut crops = self.crop.get_crops();
        if crops.is_empty() { crops.push(cam_opts.frame.crop); }

        let temperature = if self.temperature_used {
            Some(self.temperature)
        } else if cam_opts.ctrl.enable_cooler {
            Some(cam_opts.ctrl.temperature)
        } else {
            None
        };
        let exposure = if self.exposure_used { self.exposure } else { cam_opts.frame.exp_main };
        let gain = if self.gain_used { self.gain } else {  cam_opts.frame.gain };
        let offset = if self.offset_used { self.offset as i32 } else {  cam_opts.frame.offset };

        for bin in &binnings {
            for crop in &crops {
                let item = DarkCreationProgramItem {
                    count:   self.frames_count,
                    binning: *bin,
                    crop:    *crop,
                    temperature,
                    exposure,
                    gain,
                    offset,
                };
                result.push(item);
            }
        }

        return result
    }
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(default)]
struct UiOptions {
    dark_lib:      DarkLibOptions,
    defect_pixels: DefectPixelsOptions,
}

impl Default for UiOptions {
    fn default() -> Self {
        Self {
            dark_lib:      DarkLibOptions::default(),
            defect_pixels: DefectPixelsOptions::default(),
        }
    }
}

pub struct DarksLibraryDialog {
    builder:           gtk::Builder,
    dialog:            gtk::Dialog,
    core:              Arc<Core>,
    options:           Arc<RwLock<Options>>,
    ui_options:        RefCell<UiOptions>,
    core_subscription: RefCell<Option<Subscription>>,
}

impl Drop for DarksLibraryDialog {
    fn drop(&mut self) {
        log::info!("DarksLibraryDialog dropped");

        if let Some(subscription) = self.core_subscription.borrow_mut().take() {
            self.core.unsubscribe_events(subscription);
        }
    }
}

impl DarksLibraryDialog {
    const CONF_FN: &str = "ui_darks_lib";

    pub fn new(
        core:          &Arc<Core>,
        options:       &Arc<RwLock<Options>>,
        transient_for: &gtk::Window
    ) -> Rc<Self> {
        let builder = gtk::Builder::from_string(include_str!("resources/darks_library.ui"));
        let dialog = builder.object::<gtk::Dialog>("dialog").unwrap();
        dialog.set_transient_for(Some(transient_for));
        let result = Rc::new(Self {
            core:              Arc::clone(core),
            options:           Arc::clone(options),
            ui_options:        RefCell::new(UiOptions::default()),
            core_subscription: RefCell::new(None),
            builder,
            dialog,
        });

        result.init_widgets();
        result.load_options();
        result.show_options();
        result.correct_widgets_enable_state();
        result.connect_events();
        result.show_info();

        result
    }

    pub fn exec(self: &Rc<Self>) {
        self.dialog.connect_response(move |dlg, _| {
            dlg.close();
        });

        self.dialog.show();
    }

    fn init_widgets(&self) {
        let init_spinbutton = |name, min, max, digits, inc, inc_page| {
            let spb = self.builder.object::<gtk::SpinButton>(name).unwrap();
            spb.set_range(min, max);
            spb.set_digits(digits);
            spb.set_increments(inc, inc_page);
        };

        init_spinbutton("spb_dark_cnt", 3.0, 1000.0, 0, 3.0, 30.0);
        init_spinbutton("spb_def_cnt", 3.0, 1000.0, 0, 3.0, 30.0);
        init_spinbutton("spb_def_temp", -50.0, 50.0, 0, 1.0, 10.0);
        init_spinbutton("spb_def_exp", 1.0, 1000.0, 0, 1.0, 10.0);
        init_spinbutton("spb_def_gain", 0.0, 100_000.0, 0, 10.0, 100.0);
        init_spinbutton("spb_def_offs", 0.0, 10_000.0, 0, 10.0, 100.0);
    }

    fn load_options(&self) {
        let mut ui_options = self.ui_options.borrow_mut();
        gtk_utils::exec_and_show_error(&self.dialog, || {
            load_json_from_config_file(&mut *ui_options, Self::CONF_FN)?;
            Ok(())
        });
    }

    fn save_options(&self) {
        let ui_options = self.ui_options.borrow();
        gtk_utils::exec_and_show_error(&self.dialog, || {
            save_json_to_config(&*ui_options, Self::CONF_FN)?;
            Ok(())
        });
    }

    fn show_options(&self) {
        let ui = gtk_utils::UiHelper::new_from_builder(&self.builder);
        let ui_options = self.ui_options.borrow();

        // Dark library

        let show_item = |chb_name, entry_name, item: &DarkLibOptionsItem| {
            let chb = self.builder.object::<gtk::CheckButton>(chb_name).unwrap();
            chb.set_active(item.used);

            let entry = self.builder.object::<gtk::Entry>(entry_name).unwrap();
            let text = item.values
                .iter()
                .map(|v| format!("{:.0}", v))
                .join(" ");
            entry.set_text(&text);
        };
        ui.set_prop_f64("spb_dark_cnt.value", ui_options.dark_lib.frames_count as f64);
        show_item("chb_dark_temp", "e_dark_temp", &ui_options.dark_lib.temperature);
        show_item("chb_dark_exp", "e_dark_exp", &ui_options.dark_lib.exposure);
        show_item("chb_dark_gain", "e_dark_gain", &ui_options.dark_lib.gain);
        show_item("chb_dark_offset", "e_dark_offset", &ui_options.dark_lib.offset);
        ui.set_prop_bool("chb_dark_bin.active", ui_options.dark_lib.binning.used);
        ui.set_prop_bool("chb_dark_bin1x1.active", ui_options.dark_lib.binning.bin1x1);
        ui.set_prop_bool("chb_dark_bin2x2.active", ui_options.dark_lib.binning.bin2x2);
        ui.set_prop_bool("chb_dark_bin4x4.active", ui_options.dark_lib.binning.bin4x4);
        ui.set_prop_bool("chb_dark_crop.active", ui_options.dark_lib.crop.used);
        ui.set_prop_bool("chb_dark_crop100.active", ui_options.dark_lib.crop.crop100);
        ui.set_prop_bool("chb_dark_crop75.active", ui_options.dark_lib.crop.crop75);
        ui.set_prop_bool("chb_dark_crop50.active", ui_options.dark_lib.crop.crop50);
        ui.set_prop_bool("chb_dark_crop33.active", ui_options.dark_lib.crop.crop33);
        ui.set_prop_bool("chb_dark_crop25.active", ui_options.dark_lib.crop.crop25);

        // Defect pixels

        ui.set_prop_f64("spb_def_cnt.value", ui_options.defect_pixels.frames_count as f64);
        ui.set_prop_bool("chb_def_temp.active", ui_options.defect_pixels.temperature_used);
        ui.set_prop_f64("spb_def_temp.value", ui_options.defect_pixels.temperature);
        ui.set_prop_bool("chb_def_exp.active", ui_options.defect_pixels.exposure_used);
        ui.set_prop_f64("spb_def_exp.value", ui_options.defect_pixels.exposure);
        ui.set_prop_bool("chb_def_gain.active", ui_options.defect_pixels.gain_used);
        ui.set_prop_f64("spb_def_gain.value", ui_options.defect_pixels.gain);
        ui.set_prop_bool("chb_def_offs.active", ui_options.defect_pixels.offset_used);
        ui.set_prop_f64("spb_def_offs.value", ui_options.defect_pixels.offset);
        ui.set_prop_bool("chb_def_bin.active", ui_options.defect_pixels.binning.used);
        ui.set_prop_bool("chb_def_bin1x1.active", ui_options.defect_pixels.binning.bin1x1);
        ui.set_prop_bool("chb_def_bin2x2.active", ui_options.defect_pixels.binning.bin2x2);
        ui.set_prop_bool("chb_def_bin4x4.active", ui_options.defect_pixels.binning.bin4x4);
        ui.set_prop_bool("chb_def_crop.active", ui_options.defect_pixels.crop.used);
        ui.set_prop_bool("chb_def_crop100.active", ui_options.defect_pixels.crop.crop100);
        ui.set_prop_bool("chb_def_crop75.active", ui_options.defect_pixels.crop.crop75);
        ui.set_prop_bool("chb_def_crop50.active", ui_options.defect_pixels.crop.crop50);
        ui.set_prop_bool("chb_def_crop33.active", ui_options.defect_pixels.crop.crop33);
        ui.set_prop_bool("chb_def_crop25.active", ui_options.defect_pixels.crop.crop25);
    }

    fn get_options(&self) {
        let ui = gtk_utils::UiHelper::new_from_builder(&self.builder);
        let mut ui_options = self.ui_options.borrow_mut();

        // Dark library

        let get_item = |chb_name, entry_name| -> DarkLibOptionsItem {
            let chb = self.builder.object::<gtk::CheckButton>(chb_name).unwrap();
            let entry = self.builder.object::<gtk::Entry>(entry_name).unwrap();
            let values: Vec<f64> = entry.text()
                .split(" ")
                .map(|t| t.parse::<f64>())
                .filter_map(|v| v.ok())
                .collect();
            DarkLibOptionsItem {
                used: chb.is_active(),
                values
            }
        };
        ui_options.dark_lib.frames_count = ui.prop_f64("spb_dark_cnt.value") as usize;
        ui_options.dark_lib.temperature = get_item("chb_dark_temp", "e_dark_temp");
        ui_options.dark_lib.exposure = get_item("chb_dark_exp", "e_dark_exp");
        ui_options.dark_lib.gain = get_item("chb_dark_gain", "e_dark_gain");
        ui_options.dark_lib.offset = get_item("chb_dark_offset", "e_dark_offset");

        ui_options.dark_lib.binning.used = ui.prop_bool("chb_dark_bin.active");
        ui_options.dark_lib.binning.bin1x1 = ui.prop_bool("chb_dark_bin1x1.active");
        ui_options.dark_lib.binning.bin2x2 = ui.prop_bool("chb_dark_bin2x2.active");
        ui_options.dark_lib.binning.bin4x4 = ui.prop_bool("chb_dark_bin4x4.active");
        ui_options.dark_lib.crop.used = ui.prop_bool("chb_dark_crop.active");
        ui_options.dark_lib.crop.crop100 = ui.prop_bool("chb_dark_crop100.active");
        ui_options.dark_lib.crop.crop75 = ui.prop_bool("chb_dark_crop75.active");
        ui_options.dark_lib.crop.crop50 = ui.prop_bool("chb_dark_crop50.active");
        ui_options.dark_lib.crop.crop33 = ui.prop_bool("chb_dark_crop33.active");
        ui_options.dark_lib.crop.crop25 = ui.prop_bool("chb_dark_crop25.active");

        // Defect pixels

        ui_options.defect_pixels.frames_count = ui.prop_f64("spb_def_cnt.value") as usize;
        ui_options.defect_pixels.temperature_used = ui.prop_bool("chb_def_temp.active");
        ui_options.defect_pixels.temperature = ui.prop_f64("spb_def_temp.value");
        ui_options.defect_pixels.exposure_used = ui.prop_bool("chb_def_exp.active");
        ui_options.defect_pixels.exposure = ui.prop_f64("spb_def_exp.value");
        ui_options.defect_pixels.gain_used = ui.prop_bool("chb_def_gain.active");
        ui_options.defect_pixels.gain = ui.prop_f64("spb_def_gain.value");
        ui_options.defect_pixels.offset_used = ui.prop_bool("chb_def_offs.active");
        ui_options.defect_pixels.offset = ui.prop_f64("spb_def_offs.value");
        ui_options.defect_pixels.binning.used = ui.prop_bool("chb_def_bin.active");
        ui_options.defect_pixels.binning.bin1x1 = ui.prop_bool("chb_def_bin1x1.active");
        ui_options.defect_pixels.binning.bin2x2 = ui.prop_bool("chb_def_bin2x2.active");
        ui_options.defect_pixels.binning.bin4x4 = ui.prop_bool("chb_def_bin4x4.active");
        ui_options.defect_pixels.crop.used = ui.prop_bool("chb_def_crop.active");
        ui_options.defect_pixels.crop.crop100 = ui.prop_bool("chb_def_crop100.active");
        ui_options.defect_pixels.crop.crop75 = ui.prop_bool("chb_def_crop75.active");
        ui_options.defect_pixels.crop.crop50 = ui.prop_bool("chb_def_crop50.active");
        ui_options.defect_pixels.crop.crop33 = ui.prop_bool("chb_def_crop33.active");
        ui_options.defect_pixels.crop.crop25 = ui.prop_bool("chb_def_crop25.active");

        // make frames count is multiple of 3

        let multiple_of_3 = |v| { ((v + 2) / 3) * 3 };
        ui_options.dark_lib.frames_count = multiple_of_3(ui_options.dark_lib.frames_count);
        ui_options.defect_pixels.frames_count = multiple_of_3(ui_options.defect_pixels.frames_count);
    }

    fn connect_events(self: &Rc<Self>) {
        let connect_checkbtn = |name| {
            let checkbox = self.builder.object::<gtk::CheckButton>(name).unwrap();
            checkbox.connect_active_notify(clone!(@strong self as self_ => move |_| {
                self_.correct_widgets_enable_state();
                self_.get_options();
                self_.show_info();
            }));
        };

        let connect_spinbtn = |name| {
            let spb = self.builder.object::<gtk::SpinButton>(name).unwrap();
            spb.connect_value_changed(clone!(@strong self as self_ => move |_| {
                self_.get_options();
                self_.show_info();
            }));
        };

        let connect_entry = |name| {
            let spb = self.builder.object::<gtk::Entry>(name).unwrap();
            spb.connect_text_notify(clone!(@strong self as self_ => move |_| {
                self_.get_options();
                self_.show_info();
            }));
        };

        connect_spinbtn ("spb_dark_cnt");
        connect_checkbtn("chb_dark_temp");
        connect_entry   ("e_dark_temp");
        connect_checkbtn("chb_dark_exp");
        connect_entry   ("e_dark_exp");
        connect_checkbtn("chb_dark_gain");
        connect_entry   ("e_dark_gain");
        connect_checkbtn("chb_dark_offset");
        connect_entry   ("e_dark_offset");
        connect_checkbtn("chb_dark_bin");
        connect_checkbtn("chb_dark_bin1x1");
        connect_checkbtn("chb_dark_bin2x2");
        connect_checkbtn("chb_dark_bin4x4");
        connect_checkbtn("chb_dark_crop");
        connect_checkbtn("chb_dark_crop100");
        connect_checkbtn("chb_dark_crop75");
        connect_checkbtn("chb_dark_crop50");
        connect_checkbtn("chb_dark_crop33");
        connect_checkbtn("chb_dark_crop25");

        connect_spinbtn ("spb_def_cnt");
        connect_checkbtn("chb_def_temp");
        connect_spinbtn ("spb_def_temp");
        connect_checkbtn("chb_def_exp");
        connect_spinbtn ("spb_def_exp");
        connect_checkbtn("chb_def_gain");
        connect_spinbtn ("spb_def_gain");
        connect_checkbtn("chb_def_offs");
        connect_spinbtn ("spb_def_offs");
        connect_checkbtn("chb_def_bin");
        connect_checkbtn("chb_def_bin1x1");
        connect_checkbtn("chb_def_bin2x2");
        connect_checkbtn("chb_def_bin4x4");
        connect_checkbtn("chb_def_crop");
        connect_checkbtn("chb_def_crop100");
        connect_checkbtn("chb_def_crop75");
        connect_checkbtn("chb_def_crop50");
        connect_checkbtn("chb_def_crop33");
        connect_checkbtn("chb_def_crop25");

        let btn_create_dark_files = self.builder.object::<gtk::Button>("btn_create_dark_files").unwrap();
        btn_create_dark_files.connect_clicked(clone!(@strong self as self_ => move |_| {
            self_.handler_btn_create_master_darks();
        }));

        let btn_stop_dark_files = self.builder.object::<gtk::Button>("btn_stop_dark_files").unwrap();
        btn_stop_dark_files.connect_clicked(clone!(@strong self as self_ => move |_| {
            self_.handler_btn_stop();
        }));

        let btn_create_defect_pixels_files = self.builder.object::<gtk::Button>("btn_create_defect_pixels_files").unwrap();
        btn_create_defect_pixels_files.connect_clicked(clone!(@strong self as self_ => move |_| {
            self_.get_options();
            self_.save_options();
            self_.handler_btn_create_defect_pixels_files();
        }));

        let btn_stop_defect_pixels_files = self.builder.object::<gtk::Button>("btn_stop_defect_pixels_files").unwrap();
        btn_stop_defect_pixels_files.connect_clicked(clone!(@strong self as self_ => move |_| {
            self_.handler_btn_stop();
        }));

        // core events

        let (sender, receiver) = async_channel::unbounded();
        let subscription = self.core.subscribe_events(move |evt| {
            sender.send_blocking(evt).unwrap();
        });
        glib::spawn_future_local(clone!(@weak self as self_ => async move {
            while let Ok(event) = receiver.recv().await {
                self_.process_core_event(event);
            }
        }));
        *self.core_subscription.borrow_mut() = Some(subscription);
    }

    fn correct_widgets_enable_state(&self) {
        let ui = gtk_utils::UiHelper::new_from_builder(&self.builder);

        let mode = self.core.mode_data().mode.get_type();

        let is_waiting = mode == ModeType::Waiting;
        let saving_darks =
            mode == ModeType::SavingMasterDark ||
            mode == ModeType::CreatingDarks;

        let saving_defect_pixels =
            mode == ModeType::SavingDefectPixels ||
            mode == ModeType::CreatingDefectPixels;

        ui.enable_widgets(false, &[
            ("e_dark_temp", ui.prop_bool("chb_dark_temp.active")),
            ("e_dark_exp", ui.prop_bool("chb_dark_exp.active")),
            ("e_dark_gain", ui.prop_bool("chb_dark_gain.active")),
            ("e_dark_offset", ui.prop_bool("chb_dark_offset.active")),
            ("bx_dark_bin", ui.prop_bool("chb_dark_bin.active")),
            ("bx_dark_crop", ui.prop_bool("chb_dark_crop.active")),
            ("spb_def_temp", ui.prop_bool("chb_def_temp.active")),
            ("spb_def_exp", ui.prop_bool("chb_def_exp.active")),
            ("spb_def_gain", ui.prop_bool("chb_def_gain.active")),
            ("spb_def_offs", ui.prop_bool("chb_def_offs.active")),
            ("bx_def_bin", ui.prop_bool("chb_def_bin.active")),
            ("bx_def_crop", ui.prop_bool("chb_def_crop.active")),

            ("grd_dark",                       is_waiting),
            ("btn_create_dark_files",          is_waiting),
            ("prb_dark",                       saving_darks),
            ("btn_stop_dark_files",            saving_darks),

            ("grd_def",                        is_waiting),
            ("prb_def",                        saving_defect_pixels),
            ("btn_stop_defect_pixels_files",   saving_defect_pixels),
            ("btn_create_defect_pixels_files", is_waiting),
        ]);
    }

    fn show_info(&self) {
        let ui_options = self.ui_options.borrow();
        let options = self.options.read().unwrap();
        let dark_library_program = ui_options.dark_lib.create_program(&options.cam);
        self.show_program_info(&dark_library_program, "l_dark_info");

        let defect_pixels_program = ui_options.defect_pixels.create_program(&options.cam);
        self.show_program_info(&defect_pixels_program, "l_def_info");
    }

    fn show_program_info(
        &self,
        program:    &Vec<DarkCreationProgramItem>,
        label_name: &str
    ) {
        let duration: f64 = program.iter()
            .map(|item| item.count as f64 * item.exposure)
            .sum();

        let text = format!(
            "Sessions: {} (~ {})",
            program.len(),
            seconds_to_total_time_str(duration, false)
        );

        let label = self.builder.object::<gtk::Label>(label_name).unwrap();
        label.set_text(&text);
    }

    fn handler_btn_create_master_darks(&self) {
        self.start(DarkLibMode::DarkFiles);
    }

    fn handler_btn_create_defect_pixels_files(&self) {
        self.start(DarkLibMode::DefectPixelsFiles);
    }

    fn start(&self, mode: DarkLibMode) {
        self.get_options();
        self.save_options();
        let options = self.options.read().unwrap();
        let ui_options = self.ui_options.borrow();
        let program = match mode {
            DarkLibMode::DarkFiles =>
                ui_options.dark_lib.create_program(&options.cam),
            DarkLibMode::DefectPixelsFiles =>
                ui_options.defect_pixels.create_program(&options.cam),
        };
        drop(ui_options);
        drop(options);
        gtk_utils::exec_and_show_error(&self.dialog, || {
            self.core.start_creating_dark_library(mode, &program)?;
            Ok(())
        });
        self.correct_widgets_enable_state();
    }

    fn handler_btn_stop(&self) {
        self.core.abort_active_mode();
        self.correct_widgets_enable_state();
    }

    fn process_core_event(&self, event: CoreEvent) {
        let show_progress = |prb_name, cur, total| {
            let prb = self.builder.object::<gtk::ProgressBar>(prb_name).unwrap();
            if total != 0 {
                prb.set_fraction(cur as f64 / total as f64);
                prb.set_text(Some(&format!("{} / {}", cur, total)));
            }
        };

        if let CoreEvent::Propress(Some(progress), mode_type) = event {
            match mode_type {
                ModeType::CreatingDarks => {
                    show_progress("prb_dark", progress.cur, progress.total);
                    self.correct_widgets_enable_state();
                }
                ModeType::CreatingDefectPixels => {
                    show_progress("prb_def", progress.cur, progress.total);
                    self.correct_widgets_enable_state();
                }
                _ => {},
            }
        }
    }
}
