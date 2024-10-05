use std::{io::Read, path::PathBuf};

use crate::{image::image::Image, ui::sky_map::math::{degree_to_radian, radian_to_degree}};

use super::*;

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
        let mut cmd = Command::new("solve-field");
        cmd.stdout(std::process::Stdio::piped());
        cmd
            .arg("--no-plots")
            .arg("--no-verify")
            .arg("--corr") .arg("none")
            .arg("--solved").arg("none")
            .arg("--match").arg("none")
            .arg("--rdls").arg("none")
            .arg("--index-xyls").arg("none")
            .arg("--new-fits").arg("none")
            .arg("--temp-axy")
            .arg(temp_file);

        if let Some(crd) = &config.eq_coord {
            cmd.arg("--ra").arg(format!("{:.6}", radian_to_degree(crd.ra)));
            cmd.arg("--dec").arg(format!("{:.6}", radian_to_degree(crd.dec)));
            cmd.arg("--radius").arg("20");
        }

        log::debug!("Running solve-field args={:?}", cmd.get_args());
        self.child = Some(cmd.spawn()?);

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

                let re_ra_dec = regex::Regex::new(r"Field center: \(RA,Dec\) = \(([0-9.]*), ([0-9.]*)\)*.").unwrap();
                let re_fld_size = regex::Regex::new(r"Field size: ([0-9.]*) x ([0-9.]*) degrees*.").unwrap();

                let mut result_ra = None;
                let mut result_dec = None;
                let mut result_width = None;
                let mut result_height = None;
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
                }

                if result_ra.is_none() || result_dec.is_none()
                || result_width.is_none() || result_height.is_none() {
                    return Some(Err(anyhow::format_err!(
                        "Can't extract data from solve-field output"
                    )));
                }

                let eq_coord = EqCoord {
                    ra: result_ra.unwrap_or_default(),
                    dec: result_dec.unwrap_or_default(),
                };

                let result = PlateSolveResult {
                    eq_coord,
                    width: result_width.unwrap_or_default(),
                    height: result_height.unwrap_or_default(),
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
