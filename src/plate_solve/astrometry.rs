use std::{io::Read, path::PathBuf, time::Instant};
use chrono::Utc;
use crate::{image::{image::Image, io::save_image_layer_to_tif_file, simple_fits::*}, ui::sky_map::math::{arcmin_to_radian, degree_to_radian, j2000_time, radian_to_degree, EpochCvt}};
use super::*;

const EXECUTABLE_FNAME: &str = "solve-field";

const FIRST_TRY_RADIUS: i32 = 3;
const SECOND_TRY_RADIUS: i32 = 10;

#[derive(PartialEq)]
enum RadiusTry {
    First,
    Second,
    Blind,
}

enum Mode {
    None,
    Image,
    Stars { img_width: usize, img_height: usize },
}

pub struct AstrometryPlateSolver {
    child:      Option<std::process::Child>,
    file_name:  Option<PathBuf>,
    mode:       Mode,
    config:     PlateSolveConfig,
    radius_try: RadiusTry,
    start_time: Instant,
}

impl AstrometryPlateSolver {
    pub fn new() -> Self {
        Self {
            child: None,
            file_name: None,
            mode: Mode::None,
            config: PlateSolveConfig::default(),
            radius_try: RadiusTry::First,
            start_time: Instant::now(),
        }
    }

    fn clear_prev_resources(&mut self) {
        if let Some(mut child) = self.child.take() {
            _ = child.kill();
            _ = child.wait();
        }

        if let Some(file_name) = self.file_name.take() {
            _ = std::fs::remove_file(file_name.clone());
            _ = std::fs::remove_file(file_name.with_extension("wcs"));
        }
    }
}

impl Drop for AstrometryPlateSolver {
    fn drop(&mut self) {
        self.clear_prev_resources();
    }
}

impl AstrometryPlateSolver {
    fn exec_solve_field(&mut self) -> anyhow::Result<()> {
        let Some(file_name) = self.file_name.clone() else {
            anyhow::bail!("Calling exec_solve_field before saving image!");
        };
        use std::process::*;
        let mut cmd = Command::new(EXECUTABLE_FNAME);
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());
        cmd
            .arg("--no-plots")
            .arg("--overwrite")
            .arg("--corr").arg("none")
            .arg("--solved").arg("none")
            .arg("--match").arg("none")
            .arg("--rdls").arg("none")
            .arg("--index-xyls").arg("none")
            .arg("--new-fits").arg("none")
            .arg("--temp-axy");

        let mut blind = true;
        if let Some(crd) = &self.config.eq_coord {
            let mut add_ra_and_dec = || {
                let j2000 = j2000_time();
                let time = Utc::now().naive_utc();
                let epoch_cvt = EpochCvt::new(&time, &j2000);
                let crd_j2000 = epoch_cvt.convert_eq(&crd);
                cmd.arg("--ra").arg(format!("{:.6}", radian_to_degree(crd_j2000.ra)));
                cmd.arg("--dec").arg(format!("{:.6}", radian_to_degree(crd_j2000.dec)));
            };
            match self.radius_try {
                RadiusTry::First  => {
                    add_ra_and_dec();
                    cmd.arg("--radius").arg(FIRST_TRY_RADIUS.to_string());
                    blind = false;
                }
                RadiusTry::Second => {
                    add_ra_and_dec();
                    cmd.arg("--radius").arg(SECOND_TRY_RADIUS.to_string());
                    blind = false;
                }
                _ => {},
            }
        }
        let time_out = if blind { self.config.blind_time_out } else { self.config.time_out };

        cmd.arg("--cpulimit").arg(time_out.to_string());

        if let Mode::Stars { img_width, img_height } = &self.mode {
            cmd.arg("--width").arg(img_width.to_string());
            cmd.arg("--height").arg(img_height.to_string());
            cmd.arg("--x-column").arg("X");
            cmd.arg("--y-column").arg("Y");
            cmd.arg("--sort-column").arg("FLUX");
        }
        cmd.arg(&file_name);

        self.start_time = Instant::now();

        log::debug!("Running solve-field args={:?}", cmd.get_args());
        let child = cmd.spawn().map_err(|e|
            anyhow::format_err!("{} when trying to execute {}", e.to_string(), EXECUTABLE_FNAME)
        )?;
        self.child = Some(child);
        Ok(())
    }

    fn save_image_file(
        &mut self,
        image: &Image,
    ) -> anyhow::Result<()> {
        self.clear_prev_resources();
        let layer = if !image.l.is_empty() { &image.l } else { &image.g };
        let file_name = format!("astralite_platesolve_{}.tif", rand::random::<u64>());
        let temp_file = std::env::temp_dir().join(&file_name);
        log::debug!("Saving image into {:?} for plate solving...", temp_file);
        save_image_layer_to_tif_file(layer, &temp_file)?;
        self.file_name = Some(temp_file.clone());
        self.mode = Mode::Image;
        Ok(())
    }

    fn save_stars_file(
        &mut self,
        stars:      &StarItems,
        img_width:  usize,
        img_height: usize,
    ) -> anyhow::Result<()> {
        self.clear_prev_resources();

        // save stars into fits file

        const MAX_STARS_COUNT: usize = 50;

        let file_name = format!("astralite_platesolve_{}.xyls", rand::random::<u64>());
        let temp_file = std::env::temp_dir().join(&file_name);
        log::debug!("Saving stars into {:?} for plate solving...", temp_file);
        let mut file = std::fs::File::create(&temp_file)?;
        let fits_writer = FitsWriter::new();
        let mut main_header = Header::new();
        main_header.set_bool("SIMPLE", true);
        main_header.set_i64("BITPIX", 8);
        main_header.set_i64("NAXIS", 0);
        main_header.set_bool("EXTEND", true);
        fits_writer.write_header(&mut file, &main_header)?;
        let mut data = Vec::new();
        let stars_count = stars.len().min(MAX_STARS_COUNT);
        for star in &stars[..stars_count] {
            data.push((star.x + 1.0) as f64);
            data.push((star.y + 1.0) as f64);
            data.push(star.brightness as f64);
        }
        log::debug!("Saved stars count = {}", stars_count);
        let cols = [
            FitsTableCol { name: "X", type_: "1D", unit: "pix" },
            FitsTableCol { name: "Y", type_: "1D", unit: "pix" },
            FitsTableCol { name: "FLUX", type_: "1D", unit: "unknown" },
        ];
        let bintable_header = Header::new();
        fits_writer.write_header_and_bintable_f64(&mut file, &bintable_header, &cols, &data)?;
        drop(file);
        self.file_name = Some(temp_file.clone());
        self.mode = Mode::Stars {img_width, img_height};
        Ok(())
    }

    fn angle_to_radian(angle: Option<f64>, unit: &str) -> Option<f64> {
        match unit {
            "degrees"|"degree"|"deg" =>
                angle.map(degree_to_radian),
            "arcminutes"|"arcminute" =>
                angle.map(arcmin_to_radian),
            _ => None
        }
    }

    fn get_result_impl(&mut self) -> anyhow::Result<PlateSolveResult> {
        if let Some(child) = &mut self.child {
            let exit_status = match child.try_wait() {
                Ok(Some(status)) => status,
                Err(e)           => return Err(e.into()),
                _                => return Ok(PlateSolveResult::Waiting),
            };
            if exit_status.success() {
                let mut output = child.stdout.take().unwrap();
                let mut str_output = String::new();
                _ = output.read_to_string(&mut str_output);

                let platesolve_time = self.start_time.elapsed().as_secs_f64();

                log::debug!("Platesolver time = {:.1}s", platesolve_time);
                log::debug!("Platesolver stdout:\n{}", str_output);

                self.child = None;

                let re_ra_dec = regex::Regex::new(r"Field center: \(RA,Dec\) = \(([0-9.+-]+), ([0-9.+-]+)\)*.").unwrap();
                let re_fld_size = regex::Regex::new(r"Field size: ([0-9.]+) x ([0-9.]+) (\w+)*.").unwrap();
                let re_rot = regex::Regex::new(r"Field rotation angle: up is ([0-9.+-]+) (\w+).*").unwrap();

                let mut result_ra = None;
                let mut result_dec = None;
                let mut result_width = None;
                let mut result_height = None;
                let mut result_rot = None;
                for line in str_output.lines() {
                    let line = line.trim();
                    if let Some(cap) = re_ra_dec.captures(line) {
                        let ra_str = cap.get(1).unwrap().as_str();
                        result_ra = ra_str.parse::<f64>().ok().map(degree_to_radian);
                        let dec_str = cap.get(2).unwrap().as_str();
                        result_dec = dec_str.parse::<f64>().ok().map(degree_to_radian);
                    }
                    if let Some(cap) = re_fld_size.captures(line) {
                        let width_str = cap.get(1).unwrap().as_str();
                        result_width = width_str.parse::<f64>().ok();
                        let height_str = cap.get(2).unwrap().as_str();
                        result_height = height_str.parse::<f64>().ok();
                        let unit = cap.get(3).unwrap().as_str();
                        result_width = Self::angle_to_radian(result_width, unit);
                        result_height = Self::angle_to_radian(result_height, unit);
                    }
                    if let Some(cap) = re_rot.captures(line) {
                        let rot_str = cap.get(1).unwrap().as_str();
                        result_rot = rot_str.parse::<f64>().ok();
                        let unit = cap.get(2).unwrap().as_str();
                        result_rot = Self::angle_to_radian(result_rot, unit);
                    }
                }

                if result_ra.is_none() || result_dec.is_none()
                || result_width.is_none() || result_height.is_none() {
                    log::error!("Can't extract data from solve-field stdout");
                    log::error!(
                        "result_ra={:?}, result_dec={:?}, result_width={:?}, result_height={:?}",
                        result_ra, result_dec, result_width, result_height
                    );
                    return Ok(PlateSolveResult::Failed);
                }

                let crd_j2000 = EqCoord {
                    ra: result_ra.unwrap_or_default(),
                    dec: result_dec.unwrap_or_default(),
                };

                // convert plate solving coordinate from j2000 to now
                let j2000 = j2000_time();
                let time = Utc::now().naive_utc();
                let epoch_cvt = EpochCvt::new(&j2000, &time);
                let crd_now = epoch_cvt.convert_eq(&crd_j2000);

                let result = PlateSolveOkResult {
                    crd_j2000, crd_now,
                    width: result_width.unwrap_or(0.0),
                    height: result_height.unwrap_or(0.0),
                    rotation: result_rot.unwrap_or(0.0),
                    time: Utc::now(),
                };
                return Ok(PlateSolveResult::Done(result));
            } else {
                let mut output = child.stderr.take().unwrap();
                let mut str_output = String::new();
                _ = output.read_to_string(&mut str_output);

                return Err(anyhow::format_err!(
                    "solve-field exited with code {}\n\n{}",
                    exit_status.code().unwrap_or_default(),
                    str_output
                ));
            }
        }

        anyhow::bail!("Not started!");
    }
}

impl PlateSolverIface for AstrometryPlateSolver {
    fn support_stars_as_input(&self) -> bool {
        true
    }

    fn start(
        &mut self,
        data:   &PlateSolverInData,
        config: &PlateSolveConfig
    ) -> anyhow::Result<()> {
        if self.child.is_some() {
            anyhow::bail!("AstrometryPlateSolver already started");
        }
        self.config = config.clone();
        match data {
            PlateSolverInData::Image(image) =>
                self.save_image_file(image)?,
            PlateSolverInData::Stars{ stars, img_width, img_height } =>
                self.save_stars_file(*stars, *img_width, *img_height)?,
        }
        self.exec_solve_field()?;
        Ok(())
    }

    fn get_result(&mut self) -> anyhow::Result<PlateSolveResult> {
        let result = self.get_result_impl();
        if matches!(result, Ok(PlateSolveResult::Failed))
        && self.config.eq_coord.is_some() {
            match self.radius_try {
                RadiusTry::First => {
                    log::debug!("Restarting platesolver");
                    self.radius_try = RadiusTry::Second;
                    self.exec_solve_field()?;
                    return Ok(PlateSolveResult::Waiting);
                }
                RadiusTry::Second => {
                    log::debug!("Restarting platesolver");
                    self.radius_try = RadiusTry::Blind;
                    self.exec_solve_field()?;
                    return Ok(PlateSolveResult::Waiting);
                }
                RadiusTry::Blind => {
                    return result;
                }
            }
        }
        result
    }
}
