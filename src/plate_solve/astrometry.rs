use std::{io::Read, path::{Path, PathBuf}};
use chrono::Utc;
use crate::{image::{image::Image, simple_fits::*}, ui::sky_map::math::{degree_to_radian, j2000_time, radian_to_degree, EpochCvt}};
use super::*;

const EXECUTABLE_FNAME: &str = "solve-field";

pub struct AstrometryPlateSolver {
    child:     Option<std::process::Child>,
    file_name: Option<PathBuf>,
}

impl AstrometryPlateSolver {
    pub fn new() -> Self {
        Self {
            child: None,
            file_name: None,
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

    fn exec_solve_field(
        &mut self,
        file_with_data: &Path,
        config: &PlateSolveConfig,
        extra_args: impl Fn(&mut std::process::Command)
    ) -> anyhow::Result<()> {
        use std::process::*;
        let mut cmd = Command::new(EXECUTABLE_FNAME);
        cmd.stdout(std::process::Stdio::piped());
        cmd
            .arg("--no-plots")
            .arg("--corr").arg("none")
            .arg("--solved").arg("none")
            .arg("--match").arg("none")
            .arg("--rdls").arg("none")
            .arg("--index-xyls").arg("none")
            .arg("--new-fits").arg("none")
            .arg("--temp-axy");
        if let Some(crd) = &config.eq_coord {
            cmd.arg("--ra").arg(format!("{:.6}", radian_to_degree(crd.ra)));
            cmd.arg("--dec").arg(format!("{:.6}", radian_to_degree(crd.dec)));
            cmd.arg("--radius").arg("20");
        }
        cmd.arg("--cpulimit").arg(config.time_out.to_string());
        extra_args(&mut cmd);
        cmd.arg(file_with_data);
        log::debug!("Running solve-field args={:?}", cmd.get_args());
        let child = cmd.spawn().map_err(|e|
            anyhow::format_err!("{} when trying to execute {}", e.to_string(), EXECUTABLE_FNAME)
        )?;
        self.child = Some(child);
        Ok(())
    }

    fn start_platesolve_image(
        &mut self,
        image:  &Image,
        config: &PlateSolveConfig
    ) -> anyhow::Result<()> {
        self.clear_prev_resources();
        let layer = if !image.l.is_empty() { &image.l } else { &image.g };
        let file_name = format!("astralite_platesolve_{}.tif", rand::random::<u64>());
        let temp_file = std::env::temp_dir().join(&file_name);
        log::debug!("Saving image into {:?} for plate solving...", temp_file);
        layer.save_to_tiff(&temp_file)?;
        self.file_name = Some(temp_file.clone());

        self.exec_solve_field(&temp_file, config, |_| {})?;

        Ok(())
    }

    fn start_platesolve_stars(
        &mut self,
        stars:      &Stars,
        img_width:  usize,
        img_height: usize,
        config:     &PlateSolveConfig
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
        let cols = [
            FitsTableCol { name: "X", type_: "1D", unit: "pix" },
            FitsTableCol { name: "Y", type_: "1D", unit: "pix" },
            FitsTableCol { name: "FLUX", type_: "1D", unit: "unknown" },
        ];
        let bintable_header = Header::new();
        fits_writer.write_header_and_bintable_f64(&mut file, &bintable_header, &cols, &data)?;
        drop(file);
        self.file_name = Some(temp_file.clone());

        // execute 'solve-field'

        self.exec_solve_field(
            &temp_file,
            config,
            |cmd| {
                cmd.arg("--width").arg(img_width.to_string());
                cmd.arg("--height").arg(img_height.to_string());
            }
        )?;

        Ok(())
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
        match data {
            PlateSolverInData::Image(image) =>
                self.start_platesolve_image(image, config),
            PlateSolverInData::Stars{ stars, img_width, img_height } =>
                self.start_platesolve_stars(*stars, *img_width, *img_height, config),
        }
    }

    fn get_result(&mut self) -> Option<anyhow::Result<PlateSolveResult>> {
        if let Some(child) = &mut self.child {
            let exit_status = match child.try_wait() {
                Ok(Some(status)) => status,
                Err(e) => return Some(Err(e.into())),
                _ => return None,
            };
            if exit_status.success() {
                let mut output = child.stdout.take().unwrap();
                let mut str_output = String::new();
                _ = output.read_to_string(&mut str_output);

                self.child = None;

                let re_ra_dec = regex::Regex::new(r"Field center: \(RA,Dec\) = \(([0-9.+-]+), ([0-9.+-]+)\)*.").unwrap();
                let re_fld_size = regex::Regex::new(r"Field size: ([0-9.]+) x ([0-9.]+) degrees*.").unwrap();
                let re_rot = regex::Regex::new(r"Field rotation angle: up is ([0-9.+-]+) degrees.*").unwrap();

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
                        result_width = width_str.parse::<f64>().ok().map(degree_to_radian);
                        let height_str = cap.get(2).unwrap().as_str();
                        result_height = height_str.parse::<f64>().ok().map(degree_to_radian);
                    }
                    if let Some(cap) = re_rot.captures(line) {
                        let rot_str = cap.get(1).unwrap().as_str();
                        result_rot = rot_str.parse::<f64>().ok().map(degree_to_radian);
                    }
                }

                if result_ra.is_none() || result_dec.is_none()
                || result_width.is_none() || result_height.is_none() {
                    log::error!("Can't extract data from solve-field stdout:\n{}", str_output);
                    return Some(Err(anyhow::format_err!(
                        "Can't extract data from solve-field stdout"
                    )));
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

                let result = PlateSolveResult {
                    crd_j2000, crd_now,
                    width: result_width.unwrap_or(0.0),
                    height: result_height.unwrap_or(0.0),
                    rotation: result_rot.unwrap_or(0.0),
                };
                return Some(Ok(result));
            } else {
                return Some(Err(anyhow::format_err!(
                    "solve-field exit with code {}",
                    exit_status.code().unwrap_or_default()
                )));
            }
        }

        return None;
    }
}
