use std::path::PathBuf;

use crate::{
    core::utils::{FileNameArg, FileNameUtils},
    hal::FrameType,
    image::{io::load_raw_image_from_fits_file, raw::{BadPixels, CalibrMethods, RawImage}},
    utils::log_utils::TimeLogger,
};

#[derive(Default, Debug)]
pub struct CalibrParams {
    pub extract_dark:    bool,
    pub dark_lib_path:   PathBuf,
    pub flat_fname:      Option<PathBuf>,
    pub ccd_temp:        Option<f64>,
    pub sar_hot_pixels:  bool, // "sar" means Search And Remove (hot pixels)
}

#[derive(Default)]
pub struct RawCalibration {
    pub subtract_image:      Option<RawImage>,
    pub subtract_fname:      Option<PathBuf>,
    pub master_flat:         Option<RawImage>,
    pub master_flat_fname:   Option<PathBuf>,
    pub defect_pixels:       Option<BadPixels>,
    pub defect_pixels_fname: Option<PathBuf>,
}

impl RawCalibration {
    pub fn clear(&mut self) {
        self.subtract_image = None;
        self.subtract_fname = None;
        self.master_flat = None;
        self.master_flat_fname = None;
        self.defect_pixels = None;
        self.defect_pixels_fname = None;
    }

    pub fn apply_calibr_data_and_remove_hot_pixels(
        &mut self,
        params:    &Option<CalibrParams>,
        raw_image: &mut RawImage,
    ) -> eyre::Result<()> {
        let Some(params) = params else { return Ok(()); };

        let image_info = raw_image.info();
        let is_flat_file = image_info.frame_type == FrameType::Flats;
        let mut calibr_methods = CalibrMethods::empty();

        let fn_utils = FileNameUtils::default();
        let (defect_pixel_file, subtract_fname, subtract_method) =
            if params.extract_dark {
                let calibr_filename_data = FileNameArg::RawInfo{
                    info:     image_info,
                    ccd_temp: params.ccd_temp,
                };
                let defect_pixel_file = fn_utils.defect_pixels_file_name(
                    &calibr_filename_data,
                    &params.dark_lib_path
                );
                let (subtract_fname, subtract_method) = fn_utils.get_subtract_master_fname(
                    &calibr_filename_data,
                    &params.dark_lib_path
                );
                (Some(defect_pixel_file), Some(subtract_fname), subtract_method)
            } else {
                (None, None, CalibrMethods::empty())
            };

        log::debug!("apply_calibr_data_and_remove_hot_pixels params={:?}", params);
        log::debug!("self.defect_pixels_fname={:?}", self.defect_pixels_fname);
        log::debug!("self.subtract_fname={:?}", self.subtract_fname);
        log::debug!("self.master_flat_fname={:?}", self.master_flat_fname);

        let mut reload_flat = false;

        // Load defect pixels file

        if self.defect_pixels_fname != defect_pixel_file {
            self.defect_pixels = None;
            if let Some(file_name) = &defect_pixel_file && file_name.is_file() {
                let mut defect_pixels = BadPixels::default();
                log::info!(
                    "Loading defect pixels file {} ...",
                    file_name.to_str().unwrap_or_default()
                );
                defect_pixels.load_from_file(file_name)?;
                self.defect_pixels = Some(defect_pixels);
                reload_flat = true;
            }
            self.defect_pixels_fname = defect_pixel_file.clone();
        }

        // Load master dark or bias file

        if self.subtract_fname != subtract_fname {
            self.subtract_image = None;
            if let Some(file_name) = &subtract_fname && file_name.is_file() {
                log::info!(
                    "Loading master file for subtraction {} ...",
                    file_name.to_str().unwrap_or_default()
                );
                let tmr = TimeLogger::start();
                let subtract_image = load_raw_image_from_fits_file(file_name)
                    .map_err(|e| eyre::eyre!(
                        "Error '{}'\nwhen loading file '{}'",
                        e, file_name.to_str().unwrap_or_default(),
                    ))?;
                tmr.log("loading master file for subtraction");

                if subtract_method.contains(CalibrMethods::BY_DARK)
                && self.defect_pixels.is_none() {
                    let tmr = TimeLogger::start();
                    let defect_pixels = subtract_image.find_hot_pixels_in_master_dark();
                    tmr.log("searching hot pixels in dark image");
                    self.defect_pixels = Some(defect_pixels);
                    reload_flat = true;
                }

                self.subtract_image = Some(subtract_image);
            }
            self.subtract_fname = subtract_fname.clone();
        }

        // Load master flat file

        if !is_flat_file && (self.master_flat_fname != params.flat_fname || reload_flat) {
            self.master_flat = None;
            if let Some(file_name) = &params.flat_fname {
                let tmr = TimeLogger::start();
                let mut master_flat = load_raw_image_from_fits_file(file_name)
                    .map_err(|e| eyre::eyre!(
                        "Error '{}'\nreading master flat '{}'",
                        e, file_name.to_str().unwrap_or_default(),
                    ))?;
                tmr.log("loading master flat from file");
                if let Some(defect_pixels) = &self.defect_pixels {
                    let tmr = TimeLogger::start();
                    master_flat.remove_bad_pixels(&defect_pixels.items);
                    tmr.log("removing bad pixels from master flat");
                }
                let tmr = TimeLogger::start();
                master_flat.filter_flat();
                tmr.log("filtering master flat");
                log::info!(
                    "Loaded master flat file {}",
                    file_name.to_str().unwrap_or_default()
                );
                self.master_flat = Some(master_flat);
            }
            self.master_flat_fname = params.flat_fname.clone();
        }

        // Apply master dark or bias image

        if let (Some(file_name), Some(dark_image)) = (&subtract_fname, &self.subtract_image) {
            let tmr = TimeLogger::start();
            raw_image.subtract_dark_or_bias(dark_image)
                .map_err(|err| eyre::eyre!(
                    "Error {}\nwhen trying to subtract image {}",
                    err, file_name.to_str().unwrap_or_default(),
                ))?;
            tmr.log("subtracting master dark");
            calibr_methods.set(subtract_method, true);
        }

        // Apply master flat image

        if let (Some(file_name), Some(flat_image)) = (&params.flat_fname, &self.master_flat) {
            let tmr = TimeLogger::start();
            raw_image.apply_flat(flat_image)
                .map_err(|err| eyre::eyre!(
                    "Error {}\nwhen trying to apply flat image {}",
                    err, file_name.to_str().unwrap_or_default(),
                ))?;

            tmr.log("applying master flat");
            calibr_methods.set(CalibrMethods::BY_FLAT, true);
        }

        // remove defect pixels

        if let Some(defect_pixels) = &self.defect_pixels {
            if !defect_pixels.items.is_empty() {
                let tmr = TimeLogger::start();
                raw_image.remove_bad_pixels(&defect_pixels.items);
                tmr.log("removing hot pixels from light frame");
            }
            calibr_methods.set(CalibrMethods::DEFECTIVE_PIXELS, true);
        }

        // Search and remove hot pixels if there is no calibration data

        if !is_flat_file
        && params.sar_hot_pixels
        && self.defect_pixels.is_none() {
            let tmr = TimeLogger::start();
            let hot_pixels = raw_image.find_hot_pixels_in_light();
            tmr.log("searching hot pixels in light image");
            log::debug!("hot pixels count = {}", hot_pixels.len());
            if !hot_pixels.is_empty() {
                let tmr = TimeLogger::start();
                raw_image.remove_bad_pixels(&hot_pixels);
                tmr.log("removing hot pixels");
            }
            calibr_methods.set(CalibrMethods::HOT_PIXELS_SEARCH, true);
        }

        raw_image.set_calibr_methods(calibr_methods);

        Ok(())
    }
}
