use gtk::prelude::*;
use crate::{image::raw::FrameType, indi::{sexagesimal_to_value, value_to_sexagesimal}, options::*, utils::gtk_utils};

impl Options {
    /* read */

    pub fn read_all(&mut self, builder: &gtk::Builder) {
        self.read_indi(builder);
        self.read_telescope(builder);
        self.read_site(builder);
        self.read_guiding(builder);
        self.read_guiding_cam(builder);
        self.read_cam(builder);
        self.read_cam_ctrl(builder);
        self.read_cam_frame(builder);
        self.read_calibration(builder);
        self.read_raw(builder);
        self.read_live_stacking(builder);
        self.read_frame_quality(builder);
        self.read_preview(builder);
        self.read_focuser(builder);
        self.read_focuser_cam(builder);
        self.read_plate_solve(builder);
        self.read_mount(builder);
        self.read_polar_align(builder);
    }

    pub fn read_indi(&mut self, builder: &gtk::Builder) {
        let ui = gtk_utils::UiHelper::new_from_builder(builder);
        self.indi.mount    = ui.prop_string("cb_mount_drivers.active-id");
        self.indi.camera   = ui.prop_string("cb_camera_drivers.active-id");
        self.indi.guid_cam = ui.prop_string("cb_guid_cam_drivers.active-id");
        self.indi.focuser  = ui.prop_string("cb_focuser_drivers.active-id");
        self.indi.remote   = ui.prop_bool  ("chb_remote.active");
        self.indi.address  = ui.prop_string("e_remote_addr.text").unwrap_or_default();
    }

    pub fn read_telescope(&mut self, builder: &gtk::Builder) {
        let ui = gtk_utils::UiHelper::new_from_builder(builder);
        self.telescope.focal_len = ui.prop_f64("spb_foc_len.value");
        self.telescope.barlow    = ui.prop_f64("spb_barlow.value");
    }

    pub fn read_site(&mut self, builder: &gtk::Builder) {
        let ui = gtk_utils::UiHelper::new_from_builder(builder);
        let lat_string = ui.prop_string("e_site_lat.text").unwrap_or_default();
        if let Some(latitude) = sexagesimal_to_value(&lat_string) {
            self.site.latitude = latitude;
        }
        let long_str = ui.prop_string("e_site_long.text").unwrap_or_default();
        if let Some(longitude) = sexagesimal_to_value(&long_str) {
            self.site.longitude = longitude;
        }
    }

    pub fn read_guiding(&mut self, builder: &gtk::Builder) {
        let ui = gtk_utils::UiHelper::new_from_builder(builder);
        self.guiding.mode =
            if ui.prop_bool("rbtn_guide_main_cam.active") {
                GuidingMode::MainCamera
            } else if ui.prop_bool("rbtn_guide_ext.active") {
                GuidingMode::External
            } else {
                GuidingMode::Disabled
            };

        self.guiding.dith_period          = ui.prop_string("cb_dith_perod.active-id").and_then(|v| v.parse().ok()).unwrap_or(0);
        self.guiding.ext_guider.foc_len   = ui.prop_f64("spb_guid_foc_len.value");
        self.guiding.ext_guider.dith_dist = ui.prop_f64("sb_ext_dith_dist.value") as i32;
    }

    pub fn read_guiding_cam(&mut self, builder: &gtk::Builder) {
        let ui = gtk_utils::UiHelper::new_from_builder(builder);
        self.guiding.main_cam.dith_dist       = ui.prop_f64("sb_dith_dist.value") as i32;
        self.guiding.main_cam.calibr_exposure = ui.prop_f64("spb_mnt_cal_exp.value");
        self.guiding.main_cam.calibr_gain     = Gain::from_active_id(ui.prop_string("cbx_mnt_cal_gain.active-id").as_deref());
        self.guiding.main_cam.max_error       = ui.prop_f64("spb_guid_max_err.value");
    }

    pub fn read_cam(&mut self, builder: &gtk::Builder) {
        let ui = gtk_utils::UiHelper::new_from_builder(builder);
        self.cam.live_view = ui.prop_bool("chb_shots_cont.active");
        self.cam.device    = ui.prop_string("cb_camera_list.active-id").map(|str| DeviceAndProp::new(&str));
    }

    pub fn read_cam_ctrl(&mut self, builder: &gtk::Builder) {
        let ui = gtk_utils::UiHelper::new_from_builder(builder);
        self.cam.ctrl.enable_cooler = ui.prop_bool("chb_cooler.active");
        self.cam.ctrl.temperature   = ui.prop_f64("spb_temp.value");
        self.cam.ctrl.enable_fan    = ui.prop_bool("chb_fan.active");
    }

    pub fn read_cam_frame(&mut self, builder: &gtk::Builder) {
        let ui = gtk_utils::UiHelper::new_from_builder(builder);
        self.cam.frame.frame_type   = FrameType::from_active_id(ui.prop_string("cb_frame_mode.active-id").as_deref());
        self.cam.frame.set_exposure  (ui.prop_f64("spb_exp.value"));
        self.cam.frame.gain         = ui.prop_f64("spb_gain.value");
        self.cam.frame.offset       = ui.prop_f64("spb_offset.value") as i32;
        self.cam.frame.low_noise    = ui.prop_bool("chb_low_noise.active");
        self.cam.frame.binning      = Binning::from_active_id(ui.prop_string("cb_bin.active-id").as_deref());
        self.cam.frame.crop         = Crop::from_active_id(ui.prop_string("cb_crop.active-id").as_deref());
    }

    pub fn read_calibration(&mut self, builder: &gtk::Builder) {
        let ui = gtk_utils::UiHelper::new_from_builder(builder);
        self.calibr.dark_frame_en = ui.prop_bool("chb_master_dark.active");
        self.calibr.dark_library_path  = ui.fch_pathbuf ("fch_dark_library").unwrap_or_default();
        self.calibr.flat_frame_en = ui.prop_bool("chb_master_flat.active");
        self.calibr.flat_frame_fname    = ui.fch_pathbuf("fch_master_flat");
        self.calibr.hot_pixels    = ui.prop_bool("chb_hot_pixels.active");
    }

    pub fn read_raw(&mut self, builder: &gtk::Builder) {
        let ui = gtk_utils::UiHelper::new_from_builder(builder);
        self.raw_frames.use_cnt       = ui.prop_bool("chb_raw_frames_cnt.active");
        self.raw_frames.frame_cnt     = ui.prop_f64("spb_raw_frames_cnt.value") as usize;
        self.raw_frames.out_path      = ui.fch_pathbuf("fcb_raw_frames_path").unwrap_or_default();
        self.raw_frames.create_master = ui.prop_bool("chb_master_frame.active");
    }

    pub fn read_live_stacking(&mut self, builder: &gtk::Builder) {
        let ui = gtk_utils::UiHelper::new_from_builder(builder);
        self.live.save_orig     = ui.prop_bool("chb_live_save_orig.active");
        self.live.save_enabled  = ui.prop_bool("chb_live_save.active");
        self.live.save_minutes  = ui.prop_f64("spb_live_minutes.value") as usize;
        self.live.out_dir       = ui.fch_pathbuf("fch_live_folder").unwrap_or_default();
        self.live.remove_tracks = ui.prop_bool("chb_live_no_tracks.active");
    }

    pub fn read_frame_quality(&mut self, builder: &gtk::Builder) {
        let ui = gtk_utils::UiHelper::new_from_builder(builder);
        self.quality.use_max_fwhm    = ui.prop_bool("chb_max_fwhm.active");
        self.quality.max_fwhm        = ui.prop_f64("spb_max_fwhm.value") as f32;
        self.quality.use_max_ovality = ui.prop_bool("chb_max_oval.active");
        self.quality.max_ovality     = ui.prop_f64("spb_max_oval.value") as f32;
    }

    pub fn read_preview(&mut self, builder: &gtk::Builder) {
        let ui = gtk_utils::UiHelper::new_from_builder(builder);
        self.preview.scale = PreviewScale::from_active_id(
            ui.prop_string("cb_preview_scale.active-id").as_deref());
            self.preview.source = PreviewSource::from_active_id(
            ui.prop_string("cb_preview_src.active-id").as_deref()
        );
        self.preview.gamma       = ui.range_value("scl_gamma");
        self.preview.dark_lvl    = ui.range_value("scl_dark");
        self.preview.light_lvl   = ui.range_value("scl_highlight");
        self.preview.remove_grad = ui.prop_bool("chb_rem_grad.active");
        self.preview.wb_auto     = ui.prop_bool("chb_wb_auto.active");
        self.preview.wb_red      = ui.range_value("scl_wb_red");
        self.preview.wb_green    = ui.range_value("scl_wb_green");
        self.preview.wb_blue     = ui.range_value("scl_wb_blue");
    }

    pub fn read_focuser(&mut self, builder: &gtk::Builder) {
        let ui = gtk_utils::UiHelper::new_from_builder(builder);
        self.focuser.on_temp_change  = ui.prop_bool("chb_foc_temp.active");
        self.focuser.max_temp_change = ui.prop_f64("spb_foc_temp.value");
        self.focuser.on_fwhm_change  = ui.prop_bool("chb_foc_fwhm.active");
        self.focuser.max_fwhm_change = ui.prop_string("cb_foc_fwhm.active-id").and_then(|v| v.parse().ok()).unwrap_or(20);
        self.focuser.periodically    = ui.prop_bool("chb_foc_period.active");
        self.focuser.period_minutes  = ui.prop_string("cb_foc_period.active-id").and_then(|v| v.parse().ok()).unwrap_or(120);
        self.focuser.measures        = ui.prop_f64("spb_foc_measures.value") as u32;
        self.focuser.step            = ui.prop_f64("spb_foc_auto_step.value");
    }

    pub fn read_focuser_cam(&mut self, builder: &gtk::Builder) {
        let ui = gtk_utils::UiHelper::new_from_builder(builder);
        self.focuser.exposure = ui.prop_f64("spb_foc_exp.value");
        self.focuser.gain     = Gain::from_active_id(ui.prop_string("cbx_foc_gain.active-id").as_deref());
    }

    pub fn read_plate_solve(&mut self, builder: &gtk::Builder) {
        let ui = gtk_utils::UiHelper::new_from_builder(builder);
        self.plate_solver.exposure      = ui.prop_f64("spb_ps_exp.value");
        self.plate_solver.gain          = Gain::from_active_id(ui.prop_string("cbx_ps_gain.active-id").as_deref());
        self.plate_solver.bin           = Binning::from_active_id(ui.prop_string("cbx_ps_bin.active-id").as_deref());
        self.plate_solver.solver        = PlateSolverType::from_active_id(ui.prop_string("cbx_ps_solver.active-id").as_deref());
        self.plate_solver.timeout       = ui.prop_f64("spb_ps_timeout.value") as _;
        self.plate_solver.blind_timeout = ui.prop_f64("spb_ps_blind_timeout.value") as _;
    }

    pub fn read_mount(&mut self, builder: &gtk::Builder) {
        let ui = gtk_utils::UiHelper::new_from_builder(builder);
        self.mount.inv_ns = ui.prop_bool("chb_inv_ns.active");
        self.mount.inv_we = ui.prop_bool("chb_inv_we.active");
        self.mount.speed  = ui.prop_string("cb_mnt_speed.active-id");
    }

    pub fn read_polar_align(&mut self, builder: &gtk::Builder) {
        let ui = gtk_utils::UiHelper::new_from_builder(builder);
        self.polar_align.angle       = ui.prop_f64("spb_pa_angle.value");
        self.polar_align.direction   = PloarAlignDir::from_active_id(ui.prop_string("cbx_pa_dir.active-id").as_deref()).unwrap_or(PloarAlignDir::West);
        self.polar_align.speed       = ui.prop_string("cbx_pa_speed.active_id");
        self.polar_align.sim_alt_err = ui.prop_f64("spb_pa_sim_alt_err.value");
        self.polar_align.sim_az_err  = ui.prop_f64("spb_pa_sim_az_err.value");
    }

    /* show */

    pub fn show_all(&self, builder: &gtk::Builder) {
        self.show_indi(builder);
        self.show_telescope(builder);
        self.show_site(builder);
        self.show_guiding(builder);
        self.show_cam(builder);
        self.show_cam_frame(builder);
        self.show_calibr(builder);
        self.show_cam_ctrl(builder);
        self.show_raw(builder);
        self.show_live_stacking(builder);
        self.show_frame_quality(builder);
        self.show_preview(builder);
        self.show_plate_solve(builder);
        self.show_focuser(builder);
        self.show_mount(builder);
        self.show_polar_align(builder);
    }

    pub fn show_indi(&self, builder: &gtk::Builder) {
        let ui = gtk_utils::UiHelper::new_from_builder(builder);
        ui.set_prop_bool("chb_remote.active", self.indi.remote);
        ui.set_prop_str("e_remote_addr.text", Some(&self.indi.address));
    }

    pub fn show_telescope(&self, builder: &gtk::Builder) {
        let ui = gtk_utils::UiHelper::new_from_builder(builder);
        ui.set_prop_f64("spb_foc_len.value", self.telescope.focal_len);
        ui.set_prop_f64("spb_barlow.value",  self.telescope.barlow);
    }

    pub fn show_site(&self, builder: &gtk::Builder) {
        let ui = gtk_utils::UiHelper::new_from_builder(builder);
        ui.set_prop_str("e_site_lat.text", Some(&value_to_sexagesimal(self.site.latitude, true, 6)));
        ui.set_prop_str("e_site_long.text", Some(&value_to_sexagesimal(self.site.longitude, true, 6)));
    }

    pub fn show_guiding(&self, builder: &gtk::Builder) {
        let ui = gtk_utils::UiHelper::new_from_builder(builder);
        match self.guiding.mode {
            GuidingMode::Disabled =>
                ui.set_prop_bool("rbtn_no_guiding.active", true),
            GuidingMode::MainCamera =>
                ui.set_prop_bool("rbtn_guide_main_cam.active", true),
            GuidingMode::External =>
                ui.set_prop_bool("rbtn_guide_ext.active", true),
        }
        ui.set_prop_str("cb_dith_perod.active-id",    Some(self.guiding.dith_period.to_string().as_str()));
        ui.set_prop_f64("spb_guid_foc_len.value",     self.guiding.ext_guider.foc_len);
        ui.set_prop_f64("sb_ext_dith_dist.value",     self.guiding.ext_guider.dith_dist as f64);
        ui.set_prop_f64("spb_guid_max_err.value",     self.guiding.main_cam.max_error);
        ui.set_prop_f64("sb_dith_dist.value",         self.guiding.main_cam.dith_dist as f64);
        ui.set_prop_f64("spb_mnt_cal_exp.value",      self.guiding.main_cam.calibr_exposure);
        ui.set_prop_str("cbx_mnt_cal_gain.active-id", Some(self.guiding.main_cam.calibr_gain.to_active_id()));
    }

    pub fn show_cam(&self, builder: &gtk::Builder) {
        let ui = gtk_utils::UiHelper::new_from_builder(builder);
        ui.set_prop_bool("chb_shots_cont.active", self.cam.live_view);

        let cb_camera_list = builder.object::<gtk::ComboBoxText>("cb_camera_list").unwrap();

        if let Some(device) = &self.cam.device {
            let id = device.to_string();
            cb_camera_list.set_active_id(Some(&id));
            if cb_camera_list.active_id().map(|v| v.as_str() != &id).unwrap_or(true) {
                cb_camera_list.append(Some(&id), &id);
                cb_camera_list.set_active_id(Some(&id));
            }
        } else {
            cb_camera_list.set_active_id(None);
        }
    }

    pub fn show_cam_frame(&self, builder: &gtk::Builder) {
        let ui = gtk_utils::UiHelper::new_from_builder(builder);
        ui.set_prop_str ("cb_frame_mode.active-id", self.cam.frame.frame_type.to_active_id());
        ui.set_prop_f64 ("spb_exp.value",           self.cam.frame.exposure());
        ui.set_prop_f64 ("spb_gain.value",          self.cam.frame.gain);
        ui.set_prop_f64 ("spb_offset.value",        self.cam.frame.offset as f64);
        ui.set_prop_str ("cb_bin.active-id",        self.cam.frame.binning.to_active_id());
        ui.set_prop_str ("cb_crop.active-id",       self.cam.frame.crop.to_active_id());
        ui.set_prop_bool("chb_low_noise.active",    self.cam.frame.low_noise);
    }

    pub fn show_calibr(&self, builder: &gtk::Builder) {
        let ui = gtk_utils::UiHelper::new_from_builder(builder);
        ui.set_prop_bool("chb_master_dark.active", self.calibr.dark_frame_en);
        ui.set_fch_path ("fch_dark_library",       Some(&self.calibr.dark_library_path));
        ui.set_prop_bool("chb_master_flat.active", self.calibr.flat_frame_en);
        ui.set_fch_path ("fch_master_flat",        self.calibr.flat_frame_fname.as_deref());
        ui.set_prop_bool("chb_hot_pixels.active",  self.calibr.hot_pixels);

        ui.enable_widgets(false, &[("l_hot_pixels_warn", self.calibr.hot_pixels)]);
    }

    pub fn show_cam_ctrl(&self, builder: &gtk::Builder) {
        let ui = gtk_utils::UiHelper::new_from_builder(builder);
        ui.set_prop_bool("chb_cooler.active", self.cam.ctrl.enable_cooler);
        ui.set_prop_f64 ("spb_temp.value",    self.cam.ctrl.temperature);
        ui.set_prop_bool("chb_fan.active",    self.cam.ctrl.enable_fan);
    }

    pub fn show_raw(&self, builder: &gtk::Builder) {
        let ui = gtk_utils::UiHelper::new_from_builder(builder);
        ui.set_prop_bool("chb_raw_frames_cnt.active", self.raw_frames.use_cnt);
        ui.set_prop_f64 ("spb_raw_frames_cnt.value",  self.raw_frames.frame_cnt as f64);
        ui.set_fch_path ("fcb_raw_frames_path",       Some(&self.raw_frames.out_path));
        ui.set_prop_bool("chb_master_frame.active",   self.raw_frames.create_master);
    }

    pub fn show_live_stacking(&self, builder: &gtk::Builder) {
        let ui = gtk_utils::UiHelper::new_from_builder(builder);
        ui.set_prop_bool("chb_live_save_orig.active", self.live.save_orig);
        ui.set_prop_bool("chb_live_save.active",      self.live.save_enabled);
        ui.set_prop_f64 ("spb_live_minutes.value",    self.live.save_minutes as f64);
        ui.set_fch_path ("fch_live_folder",           Some(&self.live.out_dir));
        ui.set_prop_bool("chb_live_no_tracks.active", self.live.remove_tracks);
    }

    pub fn show_frame_quality(&self, builder: &gtk::Builder) {
        let ui = gtk_utils::UiHelper::new_from_builder(builder);
        ui.set_prop_bool("chb_max_fwhm.active", self.quality.use_max_fwhm);
        ui.set_prop_f64 ("spb_max_fwhm.value",  self.quality.max_fwhm as f64);
        ui.set_prop_bool("chb_max_oval.active", self.quality.use_max_ovality);
        ui.set_prop_f64 ("spb_max_oval.value",  self.quality.max_ovality as f64);
    }

    pub fn show_preview(&self, builder: &gtk::Builder) {
        let ui = gtk_utils::UiHelper::new_from_builder(builder);
        ui.set_prop_str   ("cb_preview_src.active-id",   self.preview.source.to_active_id());
        ui.set_prop_str   ("cb_preview_scale.active-id", self.preview.scale.to_active_id());
        ui.set_prop_str   ("cb_preview_color.active-id", self.preview.color.to_active_id());
        ui.set_range_value("scl_dark",                   self.preview.dark_lvl);
        ui.set_range_value("scl_highlight",              self.preview.light_lvl);
        ui.set_range_value("scl_gamma",                  self.preview.gamma);
        ui.set_prop_bool  ("chb_rem_grad.active",        self.preview.remove_grad);
        ui.set_prop_bool  ("chb_wb_auto.active",         self.preview.wb_auto);
        ui.set_range_value("scl_wb_red",                 self.preview.wb_red);
        ui.set_range_value("scl_wb_green",               self.preview.wb_green);
        ui.set_range_value("scl_wb_blue",                self.preview.wb_blue);
    }

    pub fn show_plate_solve(&self, builder: &gtk::Builder) {
        let ui = gtk_utils::UiHelper::new_from_builder(builder);
        ui.set_prop_f64("spb_ps_exp.value",           self.plate_solver.exposure);
        ui.set_prop_str("cbx_ps_gain.active-id",      Some(self.plate_solver.gain.to_active_id()));
        ui.set_prop_str("cbx_ps_bin.active-id",       self.plate_solver.bin.to_active_id());
        ui.set_prop_str("cbx_ps_solver.active-id",    self.plate_solver.solver.to_active_id());
        ui.set_prop_f64("spb_ps_timeout.value",       self.plate_solver.timeout as f64);
        ui.set_prop_f64("spb_ps_blind_timeout.value", self.plate_solver.blind_timeout as f64);
    }

    pub fn show_focuser(&self, builder: &gtk::Builder) {
        let ui = gtk_utils::UiHelper::new_from_builder(builder);
        ui.set_prop_bool("chb_foc_temp.active",     self.focuser.on_temp_change);
        ui.set_prop_f64 ("spb_foc_temp.value",      self.focuser.max_temp_change);
        ui.set_prop_bool("chb_foc_fwhm.active",     self.focuser.on_fwhm_change);
        ui.set_prop_str ("cb_foc_fwhm.active-id",   Some(self.focuser.max_fwhm_change.to_string()).as_deref());
        ui.set_prop_bool("chb_foc_period.active",   self.focuser.periodically);
        ui.set_prop_str ("cb_foc_period.active-id", Some(self.focuser.period_minutes.to_string()).as_deref());
        ui.set_prop_f64 ("spb_foc_measures.value",  self.focuser.measures as f64);
        ui.set_prop_f64 ("spb_foc_auto_step.value", self.focuser.step);
        ui.set_prop_f64 ("spb_foc_exp.value",       self.focuser.exposure);
        ui.set_prop_str ("cbx_foc_gain.active-id",  Some(self.focuser.gain.to_active_id()));
    }

    pub fn show_mount(&self, builder: &gtk::Builder) {
        let ui = gtk_utils::UiHelper::new_from_builder(builder);
        ui.set_prop_bool("chb_inv_ns.active", self.mount.inv_ns);
        ui.set_prop_bool("chb_inv_we.active", self.mount.inv_we);
    }

    pub fn show_polar_align(&self, builder: &gtk::Builder) {
        let ui = gtk_utils::UiHelper::new_from_builder(builder);
        ui.set_prop_f64("spb_pa_angle.value",       self.polar_align.angle);
        ui.set_prop_str("cbx_pa_dir.active-id",     self.polar_align.direction.to_active_id());
        ui.set_prop_str("cbx_pa_speed.active_id",   self.polar_align.speed.as_deref());
        ui.set_prop_f64("spb_pa_sim_alt_err.value", self.polar_align.sim_alt_err);
        ui.set_prop_f64("spb_pa_sim_az_err.value",  self.polar_align.sim_az_err);
    }
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

impl FrameType {
    pub fn from_active_id(active_id: Option<&str>) -> Self {
        match active_id {
            Some("flat") => Self::Flats,
            Some("dark") => Self::Darks,
            Some("bias") => Self::Biases,
            _            => Self::Lights,
        }
    }

    pub fn to_active_id(&self) -> Option<&'static str> {
        match self {
            FrameType::Lights => Some("light"),
            FrameType::Flats  => Some("flat"),
            FrameType::Darks  => Some("dark"),
            FrameType::Biases => Some("bias"),
            FrameType::Undef  => Some("light"),

        }
    }
}

impl Gain {
    pub fn from_active_id(active_id: Option<&str>) -> Self {
        match active_id {
            Some("same") => Self::Same,
            Some("min")  => Self::Min,
            Some("25%")  => Self::P25,
            Some("50%")  => Self::P50,
            Some("75%")  => Self::P75,
            Some("max")  => Self::Max,
            _            => Self::Same,
        }
    }

    pub fn to_active_id(&self) -> &'static str {
        match self {
            Self::Same => "same",
            Self::Min  => "min",
            Self::P25  => "25%",
            Self::P50  => "50%",
            Self::P75  => "75%",
            Self::Max  => "max",
        }
    }
}

impl Binning {
    pub fn from_active_id(active_id: Option<&str>) -> Self {
        match active_id {
            Some("2") => Self::Bin2,
            Some("3") => Self::Bin3,
            Some("4") => Self::Bin4,
            _         => Self::Orig,
        }
    }

    pub fn to_active_id(&self) -> Option<&'static str> {
        match self {
            Self::Orig => Some("1"),
            Self::Bin2 => Some("2"),
            Self::Bin3 => Some("3"),
            Self::Bin4 => Some("4"),
        }
    }

    pub fn get_ratio(&self) -> usize {
        match self {
            Self::Orig => 1,
            Self::Bin2 => 2,
            Self::Bin3 => 3,
            Self::Bin4 => 4,
        }
    }
}

impl Crop {
    pub fn from_active_id(active_id: Option<&str>) -> Self {
        match active_id {
            Some("75") => Self::P75,
            Some("50") => Self::P50,
            Some("33") => Self::P33,
            Some("25") => Self::P25,
            _          => Self::None,
        }
    }

    pub fn to_active_id(&self) -> Option<&'static str> {
        match self {
            Self::None => Some("100"),
            Self::P75  => Some("75"),
            Self::P50  => Some("50"),
            Self::P33  => Some("33"),
            Self::P25  => Some("25"),
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

impl PlateSolverType {
    pub fn from_active_id(active_id: Option<&str>) -> Self {
        match active_id {
            Some("astrometry.net") => Self::Astrometry,
            _                  => Self::Astrometry,
        }
    }

    pub fn to_active_id(&self) -> Option<&'static str> {
        match self {
            Self::Astrometry => Some("astrometry.net"),
        }
    }
}

impl PloarAlignDir {
    pub fn from_active_id(active_id: Option<&str>) -> Option<Self> {
        match active_id {
            Some("east") => Some(Self::East),
            Some("west") => Some(Self::West),
            _            => None,
        }
    }

    pub fn to_active_id(&self) -> Option<&'static str> {
        match self {
            Self::East  => Some("east"),
            Self::West  => Some("west"),
        }
    }
}
