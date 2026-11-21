use std::{cell::{Cell, RefCell}, collections::HashMap, rc::Rc, sync::Arc};
use gtk::{gdk::ffi::GDK_CURRENT_TIME, glib::{self, clone}, prelude::*};
use itertools::Itertools;
use macros::FromBuilder;
use serde::{Deserialize, Serialize};
use crate::{
    core::{api::{Command, Reply, StartMode}, engine::*, events::*, mode_darks_lib::*}, image::info::seconds_to_total_time_str, options::*,
    utils::{io_utils::*, log_utils::log_if_error}
};

use super::{gtk_utils::*, module::*, ui_main::MainUi, utils::DelayedActions};

pub fn init_ui(
    window:   &gtk::ApplicationWindow,
    main_ui:  &Rc<MainUi>,
    engine:   &Arc<Engine>,
) -> Rc<dyn UiModule> {
    let mut ui_options = UiOptions::default();
    exec_and_show_error(Some(window), || {
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
        main_ui:                Rc::clone(main_ui),
        window:                 window.clone(),
        engine:                 Arc::clone(engine),
        ui_options:             RefCell::new(ui_options),
        pending_cmds:           RefCell::new(HashMap::new()),
        pending_start_build_id: Cell::new(None),
        delayed_actions:        DelayedActions::new(250),
    });
    obj.delayed_actions.set_event_handler(
        clone!(@weak obj => move |action| {
            obj.delayed_actions_handler(action);
        })
    );

    obj.init_widgets();
    obj.load_options();
    obj.show_options();
    obj.connect_widgets_events();
    obj.connect_core_events();
    obj.show_info();

    obj.correct_widgets_enable_state();

    obj
}


#[derive(Serialize, Deserialize, Debug, Default)]
#[serde(default)]
struct UiOptions {
    defect_pixels: DefectPixelsOptions,
    master_darks:  MasterDarksOptions,
    master_biases: MasterBiasesOptions,
    cur_tab_page:  i32,
    expanded:      bool,
}

#[derive(FromBuilder)]
struct CommonWidgets {
    bx:               gtk::Box,
    fch_dark_library: gtk::FileChooserButton,
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
    chb_min_count:         gtk::CheckButton,
    spb_min_count:         gtk::SpinButton,
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
    widgets:      Widgets,
    main_ui:      Rc<MainUi>,
    window:       gtk::ApplicationWindow,
    engine:       Arc<Engine>,
    ui_options:   RefCell<UiOptions>,
    pending_cmds: RefCell<HashMap<u64, PendingCmd>>,
    // Build command id whose reply is still allowed to trigger Start
    pending_start_build_id: Cell<Option<u64>>,
    delayed_actions: DelayedActions<DelayedAction>,
}

enum PendingCmd {
    Build(DarkLibMode),
    BuildForStart(DarkLibMode),
    Start,
}

#[derive(Hash, Eq, PartialEq)]
enum DelayedAction {
    ShowInfo,
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

    fn panels(&self) -> Vec<Panel> {
        vec![
            Panel {
                str_id: "darks_lib",
                name:   "Darks library".to_string(),
                widget: self.widgets.common.bx.clone().upcast(),
                pos:    PanelPosition::Left,
                tab:    TabPage::Main,
                flags:  PanelFlags::empty(),
            },
        ]
    }

    fn on_app_closing(&self) {
        self.get_options();
        let ui_options = self.ui_options.borrow();
        _ = save_json_to_config::<UiOptions>(&ui_options, Self::CONF_FN);
        drop(ui_options);
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

        init_spinbutton(&widgets.dp.spb_def_cnt, 5.0, 1_000_000.0, 0, 5.0, 30.0);
        init_spinbutton(&widgets.dp.spb_def_integr, 5.0, 240.0, 0, 5.0, 15.0);
        init_spinbutton(&widgets.dp.spb_def_temp, -50.0, 50.0, 0, 1.0, 10.0);
        init_spinbutton(&widgets.dp.spb_def_exp, 1.0, 1000.0, 0, 1.0, 10.0);
        init_spinbutton(&widgets.dp.spb_def_gain, 0.0, 100_000.0, 0, 10.0, 100.0);
        init_spinbutton(&widgets.dp.spb_def_offs, 0.0, 10_000.0, 0, 10.0, 100.0);

        init_spinbutton(&widgets.darks.spb_dark_cnt, 5.0, 1_000_000.0, 0, 5.0, 30.0);
        init_spinbutton(&widgets.darks.spb_dark_integr, 5.0, 240.0, 0, 5.0, 15.0);
        init_spinbutton(&widgets.darks.spb_min_count, 5.0, 1_000_000.0, 0, 5.0, 20.0);

        init_spinbutton(&widgets.biases.spb_bias_cnt, 5.0, 1_000_000.0, 0, 5.0, 30.0);
        init_spinbutton(&widgets.biases.spb_bias_exp, 0.0001, 0.1, 5, 0.001, 0.01);

        widgets.dp.spb_def_cnt.set_snap_to_ticks(true);
        widgets.darks.spb_dark_cnt.set_snap_to_ticks(true);
        widgets.darks.spb_min_count.set_snap_to_ticks(true);
        widgets.biases.spb_bias_cnt.set_snap_to_ticks(true);
    }

    fn load_options(&self) {
        let mut ui_options = self.ui_options.borrow_mut();
        exec_and_show_error(Some(&self.window), || {
            load_json_from_config_file(&mut *ui_options, Self::CONF_FN)?;
            Ok(())
        });
    }

    fn save_options(&self) {
        let ui_options = self.ui_options.borrow();
        exec_and_show_error(Some(&self.window), || {
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
        dark_w.chb_min_count.set_active(dark_o.min_count_en);
        dark_w.spb_min_count.set_value(dark_o.min_count as f64);

        // Biases library

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
        dark_o.min_count_en = dark_w.chb_min_count.is_active();
        dark_o.min_count = dark_w.spb_min_count.value().max(1.0).round() as usize;

        // Biases library

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
    }

    fn connect_widgets_events(self: &Rc<Self>) {
        self.widgets.common.fch_dark_library.connect_file_set(
            clone!(@weak self as self_ => move |fch| {
                log_if_error(
                    &self_.engine.exec_command(
                        Command::SetDarkLibraryPath {
                            path: fch.filename().unwrap_or_default().to_string_lossy().into_owned()
                        }
                    ),
                    "Set dark library path"
                );
            })
        );

        // Any widget signal invalidates a program waiting to start (its Build
        // may still be queued in the engine FIFO)
        let connect_checkbtn = |checkbox: &gtk::CheckButton| {
            checkbox.connect_active_notify(clone!(@weak self as self_ => move |_| {
                self_.get_options();
                self_.pending_start_build_id.set(None);
                self_.delayed_actions.schedule(DelayedAction::ShowInfo);
                self_.correct_widgets_enable_state();
            }));
        };

        let connect_spinbtn = |spb: &gtk::SpinButton| {
            spb.connect_value_changed(clone!(@weak self as self_ => move |_| {
                self_.get_options();
                self_.pending_start_build_id.set(None);
                self_.delayed_actions.schedule(DelayedAction::ShowInfo);
            }));
        };

        let connect_radiobtn = |rb: &gtk::RadioButton| {
            rb.connect_active_notify(clone!(@weak self as self_ => move |_| {
                self_.get_options();
                self_.pending_start_build_id.set(None);
                self_.delayed_actions.schedule(DelayedAction::ShowInfo);
                self_.correct_widgets_enable_state();
            }));
        };

        let connect_entry = |e: &gtk::Entry| {
            e.connect_text_notify(clone!(@weak self as self_ => move |_| {
                self_.get_options();
                self_.pending_start_build_id.set(None);
                self_.delayed_actions.schedule(DelayedAction::ShowInfo);
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
        connect_checkbtn(&dark.chb_min_count);
        connect_spinbtn (&dark.spb_min_count);

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

        connect_action(&self.window, self, "open_dark_lib_folder",             Self::handler_action_open_dark_lib_folder);
        connect_action(&self.window, self, "create_defect_pixel_files",        Self::handler_action_create_defect_pixels_files);
        connect_action(&self.window, self, "stop_creating_defect_pixel_files", Self::handler_action_stop_creating_defect_pixels_files);
        connect_action(&self.window, self, "create_dark_files",                Self::handler_action_create_dark_files);
        connect_action(&self.window, self, "stop_dark_files",                  Self::handler_action_stop_dark_files);
        connect_action(&self.window, self, "create_bias_files",                Self::handler_action_create_bias_files);
        connect_action(&self.window, self, "stop_bias_files",                  Self::handler_action_stop_bias_files);
    }

    fn connect_core_events(self: &Rc<Self>) {
        let (sender, receiver) = async_channel::unbounded();
        self.engine.events.connect(move |evt| {
            // The receiver side may already be gone (module dropped on window close),
            // so a failed send is simply dropped instead of panicking
            _ = sender.send_blocking(evt);
        });
        // Upgrade the weak ref inside the loop body (not via clone!@weak before
        // `async move`): otherwise the strong ref obtained at the first poll would
        // be kept alive across every await and pin this module forever.

        let weak_self = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            while let Ok(event) = receiver.recv().await {
                let Some(self_) = weak_self.upgrade() else { break; };
                self_.process_core_event(event);
            }
        });
    }

    fn correct_widgets_enable_state(&self) {
        let mode_kind = self.main_ui.snapshot().mode.kind;
        let is_waiting = mode_kind == ModeKind::Waiting;
        let is_live_view = mode_kind == ModeKind::LiveView;
        let saving_defect_pixels =
            mode_kind == ModeKind::DefectPixels ||
            mode_kind == ModeKind::CreatingDefectPixels;
        let saving_master_darks =
            mode_kind == ModeKind::MasterDark ||
            mode_kind == ModeKind::CreatingMasterDarks;
        let saving_master_biases =
            mode_kind == ModeKind::MasterBias ||
            mode_kind == ModeKind::CreatingMasterBiases;

        //self.widgets.common.fch_dark_library.set_sensitive(true);

        let def = &self.widgets.dp;
        let dark = &self.widgets.darks;
        let bias = &self.widgets.biases;

        def.grd_def.set_sensitive(is_waiting || is_live_view);
        def.spb_def_temp.set_sensitive(def.chb_def_temp.is_active());
        def.spb_def_exp.set_sensitive(def.chb_def_exp.is_active());
        def.spb_def_gain.set_sensitive(def.chb_def_gain.is_active());
        def.spb_def_offs.set_sensitive(def.chb_def_offs.is_active());
        def.bx_def_bin.set_sensitive(def.chb_def_bin.is_active());
        def.grd_def_crop.set_sensitive(def.chb_def_crop.is_active());
        def.prb_def.set_sensitive(saving_defect_pixels);
        def.spb_def_cnt.set_sensitive(def.rbtn_def_frames_cnt.is_active());
        def.spb_def_integr.set_sensitive(def.rbtn_def_integr_time.is_active());

        dark.grd_dark.set_sensitive(is_waiting || is_live_view);
        dark.spb_dark_cnt.set_sensitive(dark.rbtn_dark_frames_cnt.is_active());
        dark.spb_dark_integr.set_sensitive(dark.rbtn_dark_integr_time.is_active());
        dark.e_dark_temp.set_sensitive(dark.chb_dark_temp.is_active());
        dark.e_dark_exp.set_sensitive(dark.chb_dark_exp.is_active());
        dark.e_dark_gain.set_sensitive(dark.chb_dark_gain.is_active());
        dark.e_dark_offset.set_sensitive(dark.chb_dark_offset.is_active());
        dark.bx_dark_bin.set_sensitive(dark.chb_dark_bin.is_active());
        dark.grd_dark_crop.set_sensitive(dark.chb_dark_crop.is_active());
        dark.chb_min_count.set_sensitive(dark.rbtn_dark_integr_time.is_active());
        dark.spb_min_count.set_sensitive(dark.chb_min_count.is_active() && dark.rbtn_dark_integr_time.is_active());
        dark.prb_dark.set_sensitive(saving_master_darks);

        bias.grd_bias.set_sensitive(is_waiting || is_live_view);
        bias.e_bias_temp.set_sensitive(bias.chb_bias_temp.is_active());
        bias.e_bias_gain.set_sensitive(bias.chb_bias_gain.is_active());
        bias.e_bias_offset.set_sensitive(bias.chb_bias_offset.is_active());
        bias.bx_bias_bin.set_sensitive(bias.chb_bias_bin.is_active());
        bias.grd_bias_crop.set_sensitive(bias.chb_bias_crop.is_active());
        bias.prb_bias.set_sensitive(saving_master_biases);

        enable_actions(&self.window, &[
            ("create_defect_pixel_files",        is_waiting || is_live_view),
            ("stop_creating_defect_pixel_files", saving_defect_pixels),
            ("create_dark_files",                is_waiting || is_live_view),
            ("stop_dark_files",                  saving_master_darks),
            ("create_bias_files",                is_waiting || is_live_view),
            ("stop_bias_files",                  saving_master_biases),
        ]);
    }

    fn delayed_actions_handler(&self, action: &DelayedAction) {
        match action {
            DelayedAction::ShowInfo => self.show_info(),
        }
    }

    fn show_info(&self) {
        // No camera: nothing to show (as before)
        if self.engine.cur_devices.camera().is_none() { return; }

        let ui_options = self.ui_options.borrow();
        for spec in [
            DarkLibSpec::DefectPixels(ui_options.defect_pixels.clone()),
            DarkLibSpec::MasterDarks(ui_options.master_darks.clone()),
            DarkLibSpec::MasterBiases(ui_options.master_biases.clone()),
        ] {
            let mode = spec.mode();
            match self.engine.submit_command(Command::BuildDarkLibraryProgram { spec }) {
                Ok(id) => { _ = self.pending_cmds.borrow_mut().insert(id, PendingCmd::Build(mode)); }
                Err(err) => { log::debug!("Build dark library program submit error: {err}"); }
            }
        }
    }

    fn show_program_info(
        &self,
        mode:               &DarkLibMode,
        sessions:           usize,
        total_duration_sec: f64,
    ) {
        let label = match mode {
            DarkLibMode::DefectPixels => &self.widgets.dp.l_def_info,
            DarkLibMode::MasterDark   => &self.widgets.darks.l_dark_info,
            DarkLibMode::MasterBias   => &self.widgets.biases.l_bias_info,
        };

        let text = format!(
            "Sessions: {} (~ {})",
            sessions,
            seconds_to_total_time_str(total_duration_sec, false)
        );

        label.set_text(&text);
    }

    fn start(&self, mode: DarkLibMode) {
        exec_and_show_error(Some(&self.window), || {
            self.get_options();
            self.save_options();

            let ui_options = self.ui_options.borrow();
            let spec = match &mode {
                DarkLibMode::DefectPixels =>
                    DarkLibSpec::DefectPixels(ui_options.defect_pixels.clone()),
                DarkLibMode::MasterDark =>
                    DarkLibSpec::MasterDarks(ui_options.master_darks.clone()),
                DarkLibMode::MasterBias =>
                    DarkLibSpec::MasterBiases(ui_options.master_biases.clone()),
            };
            drop(ui_options);

            match self.engine.submit_command(Command::BuildDarkLibraryProgram { spec }) {
                Ok(id) => {
                    self.pending_start_build_id.set(Some(id));
                    _ = self.pending_cmds.borrow_mut().insert(id, PendingCmd::BuildForStart(mode));
                }
                Err(err) => show_error_message(Some(&self.window), "Error", &err.to_string()),
            }
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
            Event::CommandReply { id, result } => {
                let Some(pending) = self.pending_cmds.borrow_mut().remove(&id) else { return; };
                match pending {
                    PendingCmd::Build(mode) => {
                        // Info labels: build errors are silently ignored
                        // (as the old in-panel `is_cooler_supported` errors were)
                        if let Ok(Reply::DarkLibraryProgram { program, total_duration_sec, .. }) = result {
                            self.show_program_info(&mode, program.len(), total_duration_sec);
                        }
                    }

                    PendingCmd::BuildForStart(mode) => {
                        // Start is triggered only by the still-current start-Build
                        if self.pending_start_build_id.get() != Some(id) {
                            return;
                        }
                        self.pending_start_build_id.set(None);
                        match result {
                            Ok(Reply::DarkLibraryProgram { program, .. }) => {
                                match self.engine.submit_command(
                                    Command::Start(StartMode::CreatingDarkLibrary { mode, program })
                                ) {
                                    Ok(id) => {
                                        _ = self.pending_cmds.borrow_mut().insert(id, PendingCmd::Start);
                                    }
                                    Err(err) => show_error_message(
                                        Some(&self.window), "Error", &err.to_string()
                                    ),
                                }
                            }
                            Ok(_) => {}
                            Err(err_msg) => show_error_message(
                                Some(&self.window), "Error", &err_msg
                            ),
                        }
                    }

                    PendingCmd::Start => {
                        if let Err(err_msg) = result {
                            show_error_message(Some(&self.window), "Error", &err_msg);
                        }
                    }
                }
            }
            Event::Progress(Some(progress), ModeKind::CreatingDefectPixels) => {
                show_progress(&self.widgets.dp.prb_def, progress.cur, progress.total);
                self.correct_widgets_enable_state();
            }

            Event::Progress(Some(progress), ModeKind::CreatingMasterDarks) => {
                show_progress(&self.widgets.darks.prb_dark, progress.cur, progress.total);
                self.correct_widgets_enable_state();
            }

            Event::Progress(Some(progress), ModeKind::CreatingMasterBiases) => {
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
        // Fresh: `SetDarkLibraryPath` is eventless
        let lib_path = self.main_ui.fresh_snapshot()
            .options.calibr.dark_library_path.to_str().unwrap_or_default()
            .to_string();
        let uri = "file:///".to_string() + &lib_path;
        _ = gtk::show_uri_on_window(
            Option::<&gtk::Window>::None,
            uri.as_str(),
            GDK_CURRENT_TIME as u32
        );
    }

    fn handler_action_create_defect_pixels_files(&self) {
        self.start(DarkLibMode::DefectPixels);
    }

    fn handler_action_stop_creating_defect_pixels_files(&self) {
        log_if_error(
            &self.engine.exec_command(Command::StopActiveMode),
            "Stop active mode"
        );
    }

    fn handler_action_create_dark_files(&self) {
        self.start(DarkLibMode::MasterDark);
    }

    fn handler_action_stop_dark_files(&self) {
        log_if_error(
            &self.engine.exec_command(Command::StopActiveMode),
            "Stop active mode"
        );
    }

    fn handler_action_create_bias_files(&self) {
        self.start(DarkLibMode::MasterBias);
    }

    fn handler_action_stop_bias_files(&self) {
        log_if_error(
            &self.engine.exec_command(Command::StopActiveMode),
            "Stop active mode"
        );
    }
}
