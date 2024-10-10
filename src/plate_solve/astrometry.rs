use std::{io::Read, path::PathBuf};

use chrono::Utc;

use crate::{image::image::Image, ui::sky_map::math::{degree_to_radian, j2000_time, radian_to_degree, EpochCvt}};

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

impl PlateSolverIface for AstrometryPlateSolver {
    fn start(&mut self, image: &Image, config: &PlateSolveConfig) -> anyhow::Result<()> {
        if self.child.is_some() {
            anyhow::bail!("AstrometryPlateSolver already started");
        }
        self.clear_prev_resources();
        let layer = if !image.l.is_empty() { &image.l } else { &image.g };
        let file_name = format!("astralite_platesolve_{}.tif", rand::random::<u64>());
        let temp_file = std::env::temp_dir().join(&file_name);
        log::debug!("Saving image into {:?} for plate solving...", temp_file);
        layer.save_to_tiff(&temp_file)?;
        self.file_name = Some(temp_file.clone());
        use std::process::*;
        let mut cmd = Command::new(EXECUTABLE_FNAME);
        cmd.stdout(std::process::Stdio::piped());
        cmd
            .arg("--resort")
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
            cmd.arg("--radius").arg("10");
        }
        cmd.arg("--cpulimit").arg(config.time_out.to_string());
        cmd.arg(temp_file);
        log::debug!("Running solve-field args={:?}", cmd.get_args());
        let child = cmd.spawn().map_err(|e|
            anyhow::format_err!("{} when trying to execute {}", e.to_string(), EXECUTABLE_FNAME)
        )?;

        self.child = Some(child);
        Ok(())
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
