use std::{cell::{Cell, RefCell}, rc::Rc, sync::{Arc, RwLock}};
use gtk::{gdk::ffi::GDK_CURRENT_TIME, glib::{self, clone}, prelude::*};
use itertools::Itertools;
use macros::FromBuilder;
use serde::{Deserialize, Serialize};
use crate::{
    core::{core::*, events::*, mode_darks_library::*},
    image::info::seconds_to_total_time_str,
    indi,
    options::*,
    utils::io_utils::*
};

use super::{gtk_utils::*, module::*};

pub fn init_ui(
    window:  &gtk::ApplicationWindow,
    options: &Arc<RwLock<Options>>,
    core:    &Arc<Core>,
    indi:    &Arc<indi::Connection>,
) -> Rc<dyn UiModule> {
    let mut ui_options = UiOptions::default();
    exec_and_show_error(window, || {
        load_json_from_config_file(&mut ui_options, DarksLibraryUI::CONF_FN)?;
        Ok(())
    });

    let builder = gtk::Builder::from_string(include_str!(r"resources/darks_lib.ui"));

    let widgets = Widgets {
        common: CommonWidgets::from_builder(&builder),
        dp:     DefPixelsWidgets::from_builder(&builder),
        darks:  DarksWidgets::from_builder(&builder),
        biases: BiasesWidgets::from_builder(&builder),
    };

    let obj = Rc::new(DarksLibraryUI {
        widgets,
        window:            window.clone(),
        options:           Arc::clone(options),
        core:              Arc::clone(core),
        indi:              Arc::clone(indi),
        ui_options:        RefCell::new(ui_options),
        core_subscription: RefCell::new(None),
        closed:            Cell::new(false),
    });

    obj.init_widgets();
    obj.load_options();
    obj.show_options();
    obj.connect_widgets_events();
    obj.connect_core_events();
    obj.show_info();

    obj.correct_widgets_enable_state();

    obj
}


fn multiple_of_5(v: usize) -> usize {
    ((v + 4) / 5) * 5
}

#[derive(Serialize, Default, Deserialize, Debug, PartialEq, Eq)]
enum FramesCountMode {
    Count,
    #[default]
    Time,
}

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
struct DefectPixelsOptions {
    frm_cnt_mode:     FramesCountMode,
    frames_count:     usize,
    integr_time:      f64, // minutes
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
            frm_cnt_mode:     FramesCountMode::default(),
            frames_count:     18,
            integr_time:      15.0,
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
    fn create_program(
        &self,
        cam_opts:   &CamOptions,
        indi:       &indi::Connection,
        cam_device: &DeviceAndProp
    ) -> anyhow::Result<Vec<MasterFileCreationProgramItem>> {
        let mut result = Vec::new();

        let mut binnings = self.binning.get_binnings();
        if binnings.is_empty() { binnings.push( cam_opts.frame.binning); }
        let mut crops = self.crop.get_crops();
        if crops.is_empty() { crops.push(cam_opts.frame.crop); }

        let temperature = if indi.camera_is_temperature_supported(&cam_device.name)? {
            if self.temperature_used {
                Some(self.temperature)
            } else if cam_opts.ctrl.enable_cooler {
                Some(cam_opts.ctrl.temperature)
            } else {
                None
            }
        } else {
            None
        };

        let exposure = if self.exposure_used { self.exposure } else { cam_opts.frame.exp_main };

        let gain = if self.gain_used { self.gain } else {  cam_opts.frame.gain };
        let offset = if self.offset_used { self.offset as i32 } else {  cam_opts.frame.offset };

        for bin in &binnings {
            for crop in &crops {
                let frame_count = match self.frm_cnt_mode {
                    FramesCountMode::Count => self.frames_count,
                    FramesCountMode::Time => {
                        let cnt = (60.0 * self.integr_time / exposure) as usize;
                        multiple_of_5(cnt)
                    }
                };
                if frame_count == 0 { continue; }
                let item = MasterFileCreationProgramItem {
                    count:      frame_count,
                    binning:    *bin,
                    crop:       *crop,
                    temperature,
                    exposure,
                    gain,
                    offset,
                };
                result.push(item);
            }
        }

        return Ok(result)
    }
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(default)]
struct ValuesItem {
    used:   bool,
    values: Vec<f64>,
}

impl Default for ValuesItem {
    fn default() -> Self {
        Self {
            used:   false,
            values: Vec::new(),
        }
    }
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(default)]
struct MasterDarksOptions {
    frm_cnt_mode: FramesCountMode,
    frames_count: usize,
    integr_time:  f64, // minutes
    temperature:  ValuesItem,
    exposure:     ValuesItem,
    gain:         ValuesItem,
    offset:       ValuesItem,
    binning:      BinningOptions,
    crop:         CropOptions,
}

impl Default for MasterDarksOptions {
    fn default() -> Self {
        Self {
            frm_cnt_mode: FramesCountMode::default(),
            frames_count: 30,
            integr_time:  60.0,
            temperature:  ValuesItem::default(),
            exposure:     ValuesItem::default(),
            gain:         ValuesItem::default(),
            offset:       ValuesItem::default(),
            binning:      BinningOptions::default(),
            crop:         CropOptions::default(),
        }
    }
}

impl MasterDarksOptions {
    fn create_program(
        &self,
        cam_opts:   &CamOptions,
        indi:       &indi::Connection,
        cam_device: &DeviceAndProp
    ) -> anyhow::Result<Vec<MasterFileCreationProgramItem>> {
        let mut result = Vec::new();

        let mut temperatures = Vec::new();
        if indi.camera_is_temperature_supported(&cam_device.name)? {
            if self.temperature.used && !self.temperature.values.is_empty() {
                for t in &self.temperature.values {
                    temperatures.push(Some(*t));
                }
            } else if cam_opts.ctrl.enable_cooler {
                temperatures.push(Some(cam_opts.ctrl.temperature));
            }
        }
        if temperatures.is_empty() {
            temperatures.push(None);
        }

        let get = |item: &ValuesItem, default: f64| -> Vec<f64> {
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
            for bin in &binnings {
                for crop in &crops {
                    for exp in &exposures {
                        for gain in &gains {
                            for offset in &offsets {
                                let frame_count = match self.frm_cnt_mode {
                                    FramesCountMode::Count => self.frames_count,
                                    FramesCountMode::Time => {
                                        let cnt = (60.0 * self.integr_time / *exp) as usize;
                                        multiple_of_5(cnt)
                                    }
                                };
                                if frame_count == 0 { continue; }
                                let item = MasterFileCreationProgramItem {
                                    count:       frame_count,
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

        return Ok(result);
    }
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(default)]
struct MasterBiasesOptions {
    frames_count: usize,
    temperature:  ValuesItem,
    exposure:     f64,
    gain:         ValuesItem,
    offset:       ValuesItem,
    binning:      BinningOptions,
    crop:         CropOptions,
}

impl Default for MasterBiasesOptions {
    fn default() -> Self {
        Self {
            frames_count: 60,
            temperature:  ValuesItem::default(),
            exposure:     0.01,
            gain:         ValuesItem::default(),
            offset:       ValuesItem::default(),
            binning:      BinningOptions::default(),
            crop:         CropOptions::default(),
        }
    }
}

impl MasterBiasesOptions {
    fn create_program(
        &self,
        cam_opts:   &CamOptions,
        indi:       &indi::Connection,
        cam_device: &DeviceAndProp
    ) -> anyhow::Result<Vec<MasterFileCreationProgramItem>> {
        let mut result = Vec::new();

        let mut temperatures = Vec::new();
        if indi.camera_is_temperature_supported(&cam_device.name)? {
            if self.temperature.used && !self.temperature.values.is_empty() {
                for t in &self.temperature.values {
                    temperatures.push(Some(*t));
                }
            } else if cam_opts.ctrl.enable_cooler {
                temperatures.push(Some(cam_opts.ctrl.temperature));
            }
        }

        if temperatures.is_empty() {
            temperatures.push(None);
        }

        let get = |item: &ValuesItem, default: f64| -> Vec<f64> {
            let mut values = Vec::new();
            if item.used && !item.values.is_empty() {
                values.extend_from_slice(&item.values);
            } else {
                values.push(default);
            }
            values
        };

        let gains = get(&self.gain, cam_opts.frame.gain);
        let offsets = get(&self.offset, cam_opts.frame.offset as f64);
        let mut binnings = self.binning.get_binnings();
        if binnings.is_empty() { binnings.push( cam_opts.frame.binning); }
        let mut crops = self.crop.get_crops();
        if crops.is_empty() { crops.push(cam_opts.frame.crop); }

        for t in &temperatures {
            for bin in &binnings {
                for crop in &crops {
                    for gain in &gains {
                        for offset in &offsets {
                            let item = MasterFileCreationProgramItem {
                                count:       self.frames_count,
                                temperature: *t,
                                exposure:    self.exposure,
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

        Ok(result)
    }
}


#[derive(Serialize, Deserialize, Debug)]
#[serde(default)]
struct UiOptions {
    defect_pixels: DefectPixelsOptions,
    master_darks:  MasterDarksOptions,
    master_biases: MasterBiasesOptions,
    cur_tab_page:  i32,
    expanded:      bool,
}

impl Default for UiOptions {
    fn default() -> Self {
        Self {
            defect_pixels: DefectPixelsOptions::default(),
            master_darks:  MasterDarksOptions::default(),
            master_biases: MasterBiasesOptions::default(),
            cur_tab_page:  0,
            expanded:      false,
        }
    }
}

#[derive(FromBuilder)]
struct CommonWidgets {
    bx:               gtk::Box,
    fch_dark_library: gtk::FileChooser,
    nb_modes:         gtk::Notebook,
}

#[derive(FromBuilder)]
struct DefPixelsWidgets {
    grd_def:              gtk::Grid,
    rbtn_def_frames_cnt:  gtk::RadioButton,
    spb_def_cnt:          gtk::SpinButton,
    rbtn_def_integr_time: gtk::RadioButton,
    spb_def_integr:       gtk::SpinButton,
    chb_def_temp:         gtk::CheckButton,
    spb_def_temp:         gtk::SpinButton,
    chb_def_exp:          gtk::CheckButton,
    spb_def_exp:          gtk::SpinButton,
    chb_def_gain:         gtk::CheckButton,
    spb_def_gain:         gtk::SpinButton,
    chb_def_offs:         gtk::CheckButton,
    spb_def_offs:         gtk::SpinButton,
    bx_def_bin:           gtk::Box,
    chb_def_bin:          gtk::CheckButton,
    chb_def_bin1x1:       gtk::CheckButton,
    chb_def_bin2x2:       gtk::CheckButton,
    chb_def_bin4x4:       gtk::CheckButton,
    grd_def_crop:         gtk::Grid,
    chb_def_crop:         gtk::CheckButton,
    chb_def_crop100:      gtk::CheckButton,
    chb_def_crop75:       gtk::CheckButton,
    chb_def_crop50:       gtk::CheckButton,
    chb_def_crop33:       gtk::CheckButton,
    chb_def_crop25:       gtk::CheckButton,
    l_def_info:           gtk::Label,
    prb_def:              gtk::ProgressBar,
}

#[derive(FromBuilder)]
struct DarksWidgets {
    grd_dark:              gtk::Grid,
    rbtn_dark_frames_cnt:  gtk::RadioButton,
    spb_dark_cnt:          gtk::SpinButton,
    rbtn_dark_integr_time: gtk::RadioButton,
    spb_dark_integr:       gtk::SpinButton,
    chb_dark_temp:         gtk::CheckButton,
    e_dark_temp:           gtk::Entry,
    chb_dark_exp:          gtk::CheckButton,
    e_dark_exp:            gtk::Entry,
    chb_dark_gain:         gtk::CheckButton,
    e_dark_gain:           gtk::Entry,
    chb_dark_offset:       gtk::CheckButton,
    e_dark_offset:         gtk::Entry,
    bx_dark_bin:           gtk::Box,
    chb_dark_bin:          gtk::CheckButton,
    chb_dark_bin1x1:       gtk::CheckButton,
    chb_dark_bin2x2:       gtk::CheckButton,
    chb_dark_bin4x4:       gtk::CheckButton,
    grd_dark_crop:         gtk::Grid,
    chb_dark_crop:         gtk::CheckButton,
    chb_dark_crop100:      gtk::CheckButton,
    chb_dark_crop75:       gtk::CheckButton,
    chb_dark_crop50:       gtk::CheckButton,
    chb_dark_crop33:       gtk::CheckButton,
    chb_dark_crop25:       gtk::CheckButton,
    l_dark_info:           gtk::Label,
    prb_dark:              gtk::ProgressBar,
}

#[derive(FromBuilder)]
struct BiasesWidgets {
    grd_bias:         gtk::Grid,
    spb_bias_cnt:     gtk::SpinButton,
    chb_bias_temp:    gtk::CheckButton,
    e_bias_temp:      gtk::Entry,
    spb_bias_exp:     gtk::SpinButton,
    chb_bias_gain:    gtk::CheckButton,
    e_bias_gain:      gtk::Entry,
    chb_bias_offset:  gtk::CheckButton,
    e_bias_offset:    gtk::Entry,
    chb_bias_bin:     gtk::CheckButton,
    bx_bias_bin:      gtk::Box,
    chb_bias_bin1x1:  gtk::CheckButton,
    chb_bias_bin2x2:  gtk::CheckButton,
    chb_bias_bin4x4:  gtk::CheckButton,
    chb_bias_crop:    gtk::CheckButton,
    grd_bias_crop:    gtk::Grid,
    chb_bias_crop100: gtk::CheckButton,
    chb_bias_crop75:  gtk::CheckButton,
    chb_bias_crop50:  gtk::CheckButton,
    chb_bias_crop33:  gtk::CheckButton,
    chb_bias_crop25:  gtk::CheckButton,
    l_bias_info:      gtk::Label,
    prb_bias:         gtk::ProgressBar,
}

struct Widgets {
    common: CommonWidgets,
    dp:     DefPixelsWidgets,
    darks:  DarksWidgets,
    biases: BiasesWidgets,
}

pub struct DarksLibraryUI {
    widgets:           Widgets,
    window:            gtk::ApplicationWindow,
    core:              Arc<Core>,
    indi:              Arc<indi::Connection>,
    options:           Arc<RwLock<Options>>,
    ui_options:        RefCell<UiOptions>,
    core_subscription: RefCell<Option<Subscription>>,
    closed:            Cell<bool>,
}

impl Drop for DarksLibraryUI {
    fn drop(&mut self) {
        log::info!("DarksLibraryUI dropped");
    }
}

impl UiModule for DarksLibraryUI {
    fn show_options(&self, options: &Options) {
        self.widgets.common.fch_dark_library.set_filename(&options.calibr.dark_library_path);
    }

    fn get_options(&self, options: &mut Options) {
        options.calibr.dark_library_path = self.widgets.common.fch_dark_library.filename().unwrap_or_default();
    }

    fn panels(&self) -> Vec<Panel> {
        vec![
            Panel {
                str_id: "darks_lib",
                name:   "Darks library".to_string(),
                widget: self.widgets.common.bx.clone().upcast(),
                pos:    PanelPosition::Left,
                tab:    PanelTab::Common,
                flags:  PanelFlags::empty(),
            },
        ]
    }

    fn process_event(&self, event: &UiModuleEvent) {
        match event {
            UiModuleEvent::ProgramClosing => {
                self.handler_closing();
            }
            _ => {}
        }
    }
}

impl DarksLibraryUI {
    const CONF_FN: &str = "ui_darks_lib";

    fn init_widgets(&self) {
        let init_spinbutton = |spb: &gtk::SpinButton, min, max, digits, inc, inc_page| {
            spb.set_range(min, max);
            spb.set_digits(digits);
            spb.set_increments(inc, inc_page);
        };

        let widgets = &self.widgets;

        init_spinbutton(&widgets.dp.spb_def_cnt, 5.0, 1000.0, 0, 5.0, 30.0);
        init_spinbutton(&widgets.dp.spb_def_integr, 5.0, 240.0, 0, 5.0, 15.0);
        init_spinbutton(&widgets.dp.spb_def_temp, -50.0, 50.0, 0, 1.0, 10.0);
        init_spinbutton(&widgets.dp.spb_def_exp, 1.0, 1000.0, 0, 1.0, 10.0);
        init_spinbutton(&widgets.dp.spb_def_gain, 0.0, 100_000.0, 0, 10.0, 100.0);
        init_spinbutton(&widgets.dp.spb_def_offs, 0.0, 10_000.0, 0, 10.0, 100.0);

        init_spinbutton(&widgets.darks.spb_dark_cnt, 5.0, 1000.0, 0, 5.0, 30.0);
        init_spinbutton(&widgets.darks.spb_dark_integr, 5.0, 240.0, 0, 5.0, 15.0);

        init_spinbutton(&widgets.biases.spb_bias_cnt, 5.0, 1000.0, 0, 5.0, 30.0);
        init_spinbutton(&widgets.biases.spb_bias_exp, 0.0001, 0.1, 5, 0.001, 0.01);
    }

    fn load_options(&self) {
        let mut ui_options = self.ui_options.borrow_mut();
        exec_and_show_error(&self.window, || {
            load_json_from_config_file(&mut *ui_options, Self::CONF_FN)?;
            Ok(())
        });
    }

    fn save_options(&self) {
        let ui_options = self.ui_options.borrow();
        exec_and_show_error(&self.window, || {
            save_json_to_config(&*ui_options, Self::CONF_FN)?;
            Ok(())
        });
    }

    fn show_options(&self) {
        let widgets = &self.widgets;
        let ui_options = self.ui_options.borrow();

        let show_values = |chb: &gtk::CheckButton, entry: &gtk::Entry, item: &ValuesItem| {
            chb.set_active(item.used);
            let text = item.values
                .iter()
                .map(|v| format!("{:.1}", v))
                .join(" ");
            entry.set_text(&text);
        };

        widgets.common.nb_modes.set_page(ui_options.cur_tab_page);

        // Defect pixels

        let dp_w = &widgets.dp;
        let dp_o = &ui_options.defect_pixels;

        dp_w.rbtn_def_frames_cnt.set_active(dp_o.frm_cnt_mode == FramesCountMode::Count);
        dp_w.rbtn_def_integr_time.set_active(dp_o.frm_cnt_mode == FramesCountMode::Time);
        dp_w.spb_def_cnt.set_value(dp_o.frames_count as f64);
        dp_w.spb_def_integr.set_value(dp_o.integr_time);
        dp_w.chb_def_temp.set_active(dp_o.temperature_used);
        dp_w.spb_def_temp.set_value(dp_o.temperature);
        dp_w.chb_def_exp.set_active(dp_o.exposure_used);
        dp_w.spb_def_exp.set_value(dp_o.exposure);
        dp_w.chb_def_gain.set_active(dp_o.gain_used);
        dp_w.spb_def_gain.set_value(dp_o.gain);
        dp_w.chb_def_offs.set_active(dp_o.offset_used);
        dp_w.spb_def_offs.set_value(dp_o.offset);

        dp_w.chb_def_bin.set_active(dp_o.binning.used);
        dp_w.chb_def_bin1x1.set_active(dp_o.binning.bin1x1);
        dp_w.chb_def_bin2x2.set_active(dp_o.binning.bin2x2);
        dp_w.chb_def_bin4x4.set_active(dp_o.binning.bin4x4);
        dp_w.chb_def_crop.set_active(dp_o.crop.used);
        dp_w.chb_def_crop100.set_active(dp_o.crop.crop100);
        dp_w.chb_def_crop75.set_active(dp_o.crop.crop75);
        dp_w.chb_def_crop50.set_active(dp_o.crop.crop50);
        dp_w.chb_def_crop33.set_active(dp_o.crop.crop33);
        dp_w.chb_def_crop25.set_active(dp_o.crop.crop25);

        // Dark library

        let dark_w = &widgets.darks;
        let dark_o = &ui_options.master_darks;

        show_values(&dark_w.chb_dark_temp, &dark_w.e_dark_temp, &dark_o.temperature);
        show_values(&dark_w.chb_dark_exp, &dark_w.e_dark_exp, &dark_o.exposure);
        show_values(&dark_w.chb_dark_gain, &dark_w.e_dark_gain, &dark_o.gain);
        show_values(&dark_w.chb_dark_offset, &dark_w.e_dark_offset, &dark_o.offset);

        dark_w.rbtn_dark_frames_cnt.set_active(dark_o.frm_cnt_mode == FramesCountMode::Count);
        dark_w.rbtn_dark_integr_time.set_active(dark_o.frm_cnt_mode == FramesCountMode::Time);
        dark_w.spb_dark_cnt.set_value(dark_o.frames_count as f64);
        dark_w.spb_dark_integr.set_value(dark_o.integr_time);
        dark_w.chb_dark_bin.set_active(dark_o.binning.used);
        dark_w.chb_dark_bin1x1.set_active(dark_o.binning.bin1x1);
        dark_w.chb_dark_bin2x2.set_active(dark_o.binning.bin2x2);
        dark_w.chb_dark_bin4x4.set_active(dark_o.binning.bin4x4);
        dark_w.chb_dark_crop.set_active(dark_o.crop.used);
        dark_w.chb_dark_crop100.set_active(dark_o.crop.crop100);
        dark_w.chb_dark_crop75.set_active(dark_o.crop.crop75);
        dark_w.chb_dark_crop50.set_active(dark_o.crop.crop50);
        dark_w.chb_dark_crop33.set_active(dark_o.crop.crop33);
        dark_w.chb_dark_crop25.set_active(dark_o.crop.crop25);

        // Biases libray

        let bias_w = &widgets.biases;
        let bias_o = &ui_options.master_biases;

        show_values(&bias_w.chb_bias_temp, &bias_w.e_bias_temp, &bias_o.temperature);
        show_values(&bias_w.chb_bias_gain, &bias_w.e_bias_gain, &bias_o.gain);
        show_values(&bias_w.chb_bias_offset, &bias_w.e_bias_offset, &bias_o.offset);

        bias_w.spb_bias_cnt.set_value(bias_o.frames_count as f64);
        bias_w.spb_bias_exp.set_value(bias_o.exposure);
        bias_w.chb_bias_bin.set_active(bias_o.binning.used);
        bias_w.chb_bias_bin1x1.set_active(bias_o.binning.bin1x1);
        bias_w.chb_bias_bin2x2.set_active(bias_o.binning.bin2x2);
        bias_w.chb_bias_bin4x4.set_active(bias_o.binning.bin4x4);
        bias_w.chb_bias_crop.set_active(bias_o.crop.used);
        bias_w.chb_bias_crop100.set_active(bias_o.crop.crop100);
        bias_w.chb_bias_crop75.set_active(bias_o.crop.crop75);
        bias_w.chb_bias_crop50.set_active(bias_o.crop.crop50);
        bias_w.chb_bias_crop33.set_active(bias_o.crop.crop33);
        bias_w.chb_bias_crop25.set_active(bias_o.crop.crop25);
    }

    fn get_options(&self) {
        let widgets = &self.widgets;
        let ui_options = &mut *self.ui_options.borrow_mut();

        let get_values = |chb: &gtk::CheckButton, entry: &gtk::Entry| -> ValuesItem {
            let values: Vec<f64> = entry.text()
                .split(" ")
                .map(|t| t.parse::<f64>())
                .filter_map(|v| v.ok())
                .collect();
            ValuesItem {
                used: chb.is_active(),
                values
            }
        };

        ui_options.cur_tab_page = widgets.common.nb_modes.page();

        // Defect pixels

        let dp_w = &widgets.dp;
        let dp_o = &mut ui_options.defect_pixels;

        dp_o.frm_cnt_mode =
            if dp_w.rbtn_def_frames_cnt.is_active() {
                FramesCountMode::Count
            } else if dp_w.rbtn_def_integr_time.is_active() {
                FramesCountMode::Time
            } else {
                unreachable!();
            };

        dp_o.frames_count = dp_w.spb_def_cnt.value() as usize;
        dp_o.integr_time = dp_w.spb_def_integr.value();
        dp_o.temperature_used = dp_w.chb_def_temp.is_active();
        dp_o.temperature = dp_w.spb_def_temp.value();
        dp_o.exposure_used = dp_w.chb_def_exp.is_active();
        dp_o.exposure = dp_w.spb_def_exp.value();
        dp_o.gain_used = dp_w.chb_def_gain.is_active();
        dp_o.gain = dp_w.spb_def_gain.value();
        dp_o.offset_used = dp_w.chb_def_offs.is_active();
        dp_o.offset = dp_w.spb_def_offs.value();
        dp_o.binning.used = dp_w.chb_def_bin.is_active();
        dp_o.binning.bin1x1 = dp_w.chb_def_bin1x1.is_active();
        dp_o.binning.bin2x2 = dp_w.chb_def_bin2x2.is_active();
        dp_o.binning.bin4x4 = dp_w.chb_def_bin4x4.is_active();
        dp_o.crop.used = dp_w.chb_def_crop.is_active();
        dp_o.crop.crop100 = dp_w.chb_def_crop100.is_active();
        dp_o.crop.crop75 = dp_w.chb_def_crop75.is_active();
        dp_o.crop.crop50 = dp_w.chb_def_crop50.is_active();
        dp_o.crop.crop33 = dp_w.chb_def_crop33.is_active();
        dp_o.crop.crop25 = dp_w.chb_def_crop25.is_active();

        // Dark library

        let dark_w = &widgets.darks;
        let dark_o = &mut ui_options.master_darks;

        dark_o.frm_cnt_mode =
            if dark_w.rbtn_dark_frames_cnt.is_active() {
                FramesCountMode::Count
            } else if dark_w.rbtn_dark_integr_time.is_active() {
                FramesCountMode::Time
            } else {
                unreachable!();
            };

        dark_o.temperature = get_values(&dark_w.chb_dark_temp, &dark_w.e_dark_temp);
        dark_o.exposure = get_values(&dark_w.chb_dark_exp, &dark_w.e_dark_exp);
        dark_o.gain = get_values(&dark_w.chb_dark_gain, &dark_w.e_dark_gain);
        dark_o.offset = get_values(&dark_w.chb_dark_offset, &dark_w.e_dark_offset);
        dark_o.frames_count = dark_w.spb_dark_cnt.value() as usize;
        dark_o.integr_time = dark_w.spb_dark_integr.value();
        dark_o.binning.used = dark_w.chb_dark_bin.is_active();
        dark_o.binning.bin1x1 = dark_w.chb_dark_bin1x1.is_active();
        dark_o.binning.bin2x2 = dark_w.chb_dark_bin2x2.is_active();
        dark_o.binning.bin4x4 = dark_w.chb_dark_bin4x4.is_active();
        dark_o.crop.used = dark_w.chb_dark_crop.is_active();
        dark_o.crop.crop100 = dark_w.chb_dark_crop100.is_active();
        dark_o.crop.crop75 = dark_w.chb_dark_crop75.is_active();
        dark_o.crop.crop50 = dark_w.chb_dark_crop50.is_active();
        dark_o.crop.crop33 = dark_w.chb_dark_crop33.is_active();
        dark_o.crop.crop25 = dark_w.chb_dark_crop25.is_active();

        // Biases libray

        let bias_w = &widgets.biases;
        let bias_o = &mut ui_options.master_biases;

        bias_o.temperature = get_values(&bias_w.chb_bias_temp, &bias_w.e_bias_temp);
        bias_o.gain = get_values(&bias_w.chb_bias_gain, &bias_w.e_bias_gain);
        bias_o.offset = get_values(&bias_w.chb_bias_offset, &bias_w.e_bias_offset);

        bias_o.frames_count = bias_w.spb_bias_cnt.value() as usize;
        bias_o.exposure = bias_w.spb_bias_exp.value();
        bias_o.binning.used = bias_w.chb_bias_bin.is_active();
        bias_o.binning.bin1x1 = bias_w.chb_bias_bin1x1.is_active();
        bias_o.binning.bin2x2 = bias_w.chb_bias_bin2x2.is_active();
        bias_o.binning.bin4x4 = bias_w.chb_bias_bin4x4.is_active();
        bias_o.crop.used = bias_w.chb_bias_crop.is_active();
        bias_o.crop.crop100 = bias_w.chb_bias_crop100.is_active();
        bias_o.crop.crop75 = bias_w.chb_bias_crop75.is_active();
        bias_o.crop.crop50 = bias_w.chb_bias_crop50.is_active();
        bias_o.crop.crop33 = bias_w.chb_bias_crop33.is_active();
        bias_o.crop.crop25 = bias_w.chb_bias_crop25.is_active();

        // make frames count is multiple of 3

        ui_options.defect_pixels.frames_count = multiple_of_5(ui_options.defect_pixels.frames_count);
        ui_options.master_darks.frames_count = multiple_of_5(ui_options.master_darks.frames_count);
        ui_options.master_biases.frames_count = multiple_of_5(ui_options.master_biases.frames_count);
    }

    fn connect_widgets_events(self: &Rc<Self>) {
        let connect_checkbtn = |checkbox: &gtk::CheckButton| {
            checkbox.connect_active_notify(clone!(@strong self as self_ => move |_| {
                self_.get_options();
                self_.show_info();
                self_.correct_widgets_enable_state();
            }));
        };

        let connect_spinbtn = |spb: &gtk::SpinButton| {
            spb.connect_value_changed(clone!(@strong self as self_ => move |_| {
                self_.get_options();
                self_.show_info();
            }));
        };

        let connect_radiobtn = |rb: &gtk::RadioButton| {
            rb.connect_active_notify(clone!(@strong self as self_ => move |_| {
                self_.get_options();
                self_.show_info();
                self_.correct_widgets_enable_state();
            }));
        };

        let connect_entry = |e: &gtk::Entry| {
            e.connect_text_notify(clone!(@strong self as self_ => move |_| {
                self_.get_options();
                self_.show_info();
            }));
        };

        let def = &self.widgets.dp;
        connect_radiobtn(&def.rbtn_def_frames_cnt);
        connect_radiobtn(&def.rbtn_def_integr_time);
        connect_spinbtn (&def.spb_def_cnt);
        connect_spinbtn (&def.spb_def_integr);
        connect_checkbtn(&def.chb_def_temp);
        connect_spinbtn (&def.spb_def_temp);
        connect_checkbtn(&def.chb_def_exp);
        connect_spinbtn (&def.spb_def_exp);
        connect_checkbtn(&def.chb_def_gain);
        connect_spinbtn (&def.spb_def_gain);
        connect_checkbtn(&def.chb_def_offs);
        connect_spinbtn (&def.spb_def_offs);
        connect_checkbtn(&def.chb_def_bin);
        connect_checkbtn(&def.chb_def_bin1x1);
        connect_checkbtn(&def.chb_def_bin2x2);
        connect_checkbtn(&def.chb_def_bin4x4);
        connect_checkbtn(&def.chb_def_crop);
        connect_checkbtn(&def.chb_def_crop100);
        connect_checkbtn(&def.chb_def_crop75);
        connect_checkbtn(&def.chb_def_crop50);
        connect_checkbtn(&def.chb_def_crop33);
        connect_checkbtn(&def.chb_def_crop25);

        let dark = &self.widgets.darks;
        connect_radiobtn(&dark.rbtn_dark_frames_cnt);
        connect_radiobtn(&dark.rbtn_dark_integr_time);
        connect_spinbtn (&dark.spb_dark_cnt);
        connect_spinbtn (&dark.spb_dark_integr);
        connect_checkbtn(&dark.chb_dark_temp);
        connect_entry   (&dark.e_dark_temp);
        connect_checkbtn(&dark.chb_dark_exp);
        connect_entry   (&dark.e_dark_exp);
        connect_checkbtn(&dark.chb_dark_gain);
        connect_entry   (&dark.e_dark_gain);
        connect_checkbtn(&dark.chb_dark_offset);
        connect_entry   (&dark.e_dark_offset);
        connect_checkbtn(&dark.chb_dark_bin);
        connect_checkbtn(&dark.chb_dark_bin1x1);
        connect_checkbtn(&dark.chb_dark_bin2x2);
        connect_checkbtn(&dark.chb_dark_bin4x4);
        connect_checkbtn(&dark.chb_dark_crop);
        connect_checkbtn(&dark.chb_dark_crop100);
        connect_checkbtn(&dark.chb_dark_crop75);
        connect_checkbtn(&dark.chb_dark_crop50);
        connect_checkbtn(&dark.chb_dark_crop33);
        connect_checkbtn(&dark.chb_dark_crop25);

        let bias = &self.widgets.biases;
        connect_checkbtn(&bias.chb_bias_temp);
        connect_checkbtn(&bias.chb_bias_gain);
        connect_checkbtn(&bias.chb_bias_offset);
        connect_checkbtn(&bias.chb_bias_bin);
        connect_checkbtn(&bias.chb_bias_bin1x1);
        connect_checkbtn(&bias.chb_bias_bin2x2);
        connect_checkbtn(&bias.chb_bias_bin4x4);
        connect_checkbtn(&bias.chb_bias_crop);
        connect_checkbtn(&bias.chb_bias_crop100);
        connect_checkbtn(&bias.chb_bias_crop75);
        connect_checkbtn(&bias.chb_bias_crop50);
        connect_checkbtn(&bias.chb_bias_crop33);
        connect_checkbtn(&bias.chb_bias_crop25);

        connect_action(&self.window, self, "open_dark_lib_folder",   Self::handler_action_open_dark_lib_folder);
        connect_action(&self.window, self, "create_def_pixls_files", Self::handler_action_create_def_pixls_files);
        connect_action(&self.window, self, "stop_def_pxls_files",    Self::handler_action_stop_def_pxls_files);
        connect_action(&self.window, self, "create_dark_files",      Self::handler_action_create_dark_files);
        connect_action(&self.window, self, "stop_dark_files",        Self::handler_action_stop_dark_files);
        connect_action(&self.window, self, "create_bias_files",      Self::handler_action_create_bias_files);
        connect_action(&self.window, self, "stop_bias_files",        Self::handler_action_stop_bias_files);
    }

    fn handler_closing(&self) {
        self.closed.set(true);

        self.get_options();
        let ui_options = self.ui_options.borrow();
        _ = save_json_to_config::<UiOptions>(&ui_options, Self::CONF_FN);
        drop(ui_options);

        if let Some(core_conn) = self.core_subscription.borrow_mut().take() {
            self.core.event_subscriptions().unsubscribe(core_conn);
        }
    }

    fn connect_core_events(self: &Rc<Self>) {
        let (sender, receiver) = async_channel::unbounded();
        let subscription = self.core.event_subscriptions().subscribe(move |evt| {
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
        let mode = self.core.mode_data().mode.get_type();
        let is_waiting = mode == ModeType::Waiting;
        let saving_defect_pixels =
            mode == ModeType::DefectPixels ||
            mode == ModeType::CreatingDefectPixels;
        let saving_master_darks =
            mode == ModeType::MasterDark ||
            mode == ModeType::CreatingMasterDarks;
        let saving_master_biases =
            mode == ModeType::MasterBias ||
            mode == ModeType::CreatingMasterBiases;

        //self.widgets.common.fch_dark_library.set_sensitive(true);

        let def = &self.widgets.dp;
        let dark = &self.widgets.darks;
        let bias = &self.widgets.biases;

        def.spb_def_temp.set_sensitive(def.chb_def_temp.is_active());
        def.spb_def_exp.set_sensitive(def.chb_def_exp.is_active());
        def.spb_def_gain.set_sensitive(def.chb_def_gain.is_active());
        def.spb_def_offs.set_sensitive(def.chb_def_offs.is_active());
        def.bx_def_bin.set_sensitive(def.chb_def_bin.is_active());
        def.grd_def_crop.set_sensitive(def.chb_def_crop.is_active());
        def.grd_def.set_sensitive(is_waiting);
        def.prb_def.set_sensitive(saving_defect_pixels);
        def.spb_def_cnt.set_sensitive(def.rbtn_def_frames_cnt.is_active());
        def.spb_def_integr.set_sensitive(def.rbtn_def_integr_time.is_active());

        dark.spb_dark_cnt.set_sensitive(dark.rbtn_dark_frames_cnt.is_active());
        dark.spb_dark_integr.set_sensitive(dark.rbtn_dark_integr_time.is_active());
        dark.e_dark_temp.set_sensitive(dark.chb_dark_temp.is_active());
        dark.e_dark_exp.set_sensitive(dark.chb_dark_exp.is_active());
        dark.e_dark_gain.set_sensitive(dark.chb_dark_gain.is_active());
        dark.e_dark_offset.set_sensitive(dark.chb_dark_offset.is_active());
        dark.bx_dark_bin.set_sensitive(dark.chb_dark_bin.is_active());
        dark.grd_dark_crop.set_sensitive(dark.chb_dark_crop.is_active());
        dark.grd_dark.set_sensitive(is_waiting);
        dark.prb_dark.set_sensitive(saving_master_darks);

        bias.e_bias_temp.set_sensitive(bias.chb_bias_temp.is_active());
        bias.e_bias_gain.set_sensitive(bias.chb_bias_gain.is_active());
        bias.e_bias_offset.set_sensitive(bias.chb_bias_offset.is_active());
        bias.bx_bias_bin.set_sensitive(bias.chb_bias_bin.is_active());
        bias.grd_bias_crop.set_sensitive(bias.chb_bias_crop.is_active());
        bias.grd_bias.set_sensitive(is_waiting);
        bias.prb_bias.set_sensitive(saving_master_biases);

        enable_actions(&self.window, &[
            ("create_def_pixls_files", is_waiting),
            ("stop_def_pxls_files",    saving_defect_pixels),
            ("create_dark_files",      is_waiting),
            ("stop_dark_files",        saving_master_darks),
            ("create_bias_files",      is_waiting),
            ("stop_bias_files",        saving_master_biases),
        ]);
    }

    fn show_info(&self) {
        let ui_options = self.ui_options.borrow();
        let options = self.options.read().unwrap();
        let Some(cam_device) = &options.cam.device else { return; };

        if let Ok(defect_pixels_program) = ui_options.defect_pixels.create_program(
            &options.cam,
            &self.indi,
            cam_device
        ) {
            self.show_program_info(&defect_pixels_program, &self.widgets.dp.l_def_info);
        };

        if let Ok(dark_library_program) = ui_options.master_darks.create_program(
            &options.cam,
            &self.indi,
            cam_device
        ) {
            self.show_program_info(&dark_library_program,  &self.widgets.darks.l_dark_info);
        }

        if let Ok(bias_library_program) = ui_options.master_biases.create_program(
            &options.cam,
            &self.indi,
            cam_device
        ) {
            self.show_program_info(&bias_library_program, &self.widgets.biases.l_bias_info);
        }
    }

    fn show_program_info(
        &self,
        program: &Vec<MasterFileCreationProgramItem>,
        label:   &gtk::Label
    ) {
        let duration: f64 = program.iter()
            .map(|item| item.count as f64 * item.exposure)
            .sum();

        let text = format!(
            "Sessions: {} (~ {})",
            program.len(),
            seconds_to_total_time_str(duration, false)
        );

        label.set_text(&text);
    }

    fn start(&self, mode: DarkLibMode) {
        exec_and_show_error(&self.window, || {
            // TODO: read all options

            //self.options.write().unwrap().read_all(&self.builder);

            self.get_options();
            self.save_options();

            let options = self.options.read().unwrap();
            let ui_options = self.ui_options.borrow();

            let Some(cam_device) = &options.cam.device else { return Ok(()); };

            let program = match mode {
                DarkLibMode::DefectPixelsFiles =>
                    ui_options.defect_pixels.create_program(&options.cam, &self.indi, cam_device)?,
                DarkLibMode::MasterDarkFiles =>
                    ui_options.master_darks.create_program(&options.cam, &self.indi, cam_device)?,
                DarkLibMode::MasterBiasFiles =>
                    ui_options.master_biases.create_program(&options.cam, &self.indi, cam_device)?,
            };
            drop(ui_options);
            drop(options);

            self.core.start_creating_dark_library(mode, &program)?;
            Ok(())
        });
    }

    fn process_core_event(&self, event: Event) {
        let show_progress = |prb: &gtk::ProgressBar, cur, total| {
            if total != 0 {
                prb.set_fraction(cur as f64 / total as f64);
                prb.set_text(Some(&format!("{} / {}", cur, total)));
            }
        };

        match event {
            Event::Progress(Some(progress), ModeType::CreatingDefectPixels) => {
                show_progress(&self.widgets.dp.prb_def, progress.cur, progress.total);
                self.correct_widgets_enable_state();
            }

            Event::Progress(Some(progress), ModeType::CreatingMasterDarks) => {
                show_progress(&self.widgets.darks.prb_dark, progress.cur, progress.total);
                self.correct_widgets_enable_state();
            }

            Event::Progress(Some(progress), ModeType::CreatingMasterBiases) => {
                show_progress(&self.widgets.biases.prb_bias, progress.cur, progress.total);
                self.correct_widgets_enable_state();
            }

            Event::ModeChanged => {
                self.correct_widgets_enable_state();
            }

            _ => {},
        }
    }

    fn handler_action_open_dark_lib_folder(&self) {
        let options = self.options.read().unwrap();
        let lib_path = &options.calibr.dark_library_path.to_str().unwrap_or_default();
        let uri = "file:///".to_string() + lib_path;
        drop(options);
        _ = gtk::show_uri_on_window(
            Option::<&gtk::Window>::None,
            uri.as_str(),
            GDK_CURRENT_TIME as u32
        );
    }

    fn handler_action_create_def_pixls_files(&self) {
        self.start(DarkLibMode::DefectPixelsFiles);
    }

    fn handler_action_stop_def_pxls_files(&self) {
        self.core.abort_active_mode();
    }

    fn handler_action_create_dark_files(&self) {
        self.start(DarkLibMode::MasterDarkFiles);
    }

    fn handler_action_stop_dark_files(&self) {
        self.core.abort_active_mode();
    }

    fn handler_action_create_bias_files(&self) {
        self.start(DarkLibMode::MasterBiasFiles);
    }

    fn handler_action_stop_bias_files(&self) {
        self.core.abort_active_mode();
    }
}
