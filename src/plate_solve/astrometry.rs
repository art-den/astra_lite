use std::{io::Read, path::PathBuf, time::Instant};
use chrono::Utc;
use crate::{
    image::{image::Image, io::save_image_layer_to_tif_file, simple_fits::*},
};
use super::*;

const EXECUTABLE_FNAME: &str = "solve-field";

// Optional drop-in replacement for `solve-field`, placed next to the app binary.
const STAR_SOLVER_FNAME: &str = if cfg!(windows) { "starsolver.exe" } else { "starsolver" };

#[derive(Clone, Copy, PartialEq)]
enum ResolveExecutable {
    SolveField,
    StarSolver,
}

impl ResolveExecutable {
    fn fname(&self) -> &'static str {
        match self {
            ResolveExecutable::SolveField => EXECUTABLE_FNAME,
            ResolveExecutable::StarSolver => STAR_SOLVER_FNAME,
        }
    }
}

/// Directory containing the running app executable.
fn app_dir() -> Option<PathBuf> {
    Some(std::env::current_exe().ok()?.parent()?.to_path_buf())
}

/// Returns the path to the plate-solving executable.
/// Prefers a `starsolver` binary next to the app; falls back to `solve-field`.
fn resolve_executable() -> (PathBuf, ResolveExecutable) {
    if let Some(dir) = app_dir() {
        let path = dir.join(STAR_SOLVER_FNAME);
        if path.is_file() {
            return (path, ResolveExecutable::StarSolver);
        }
    }
    (PathBuf::from(EXECUTABLE_FNAME), ResolveExecutable::SolveField)
}

// Directory of index files placed next to the app binary (used with `starsolver`).
const INDEX_DIR_NAME: &str = "index_files";

/// Returns the path to the `index_files` directory next to the app, if present.
fn resolve_index_dir() -> Option<PathBuf> {
    let path = app_dir()?.join(INDEX_DIR_NAME);
    if path.is_dir() { Some(path) } else { None }
}

// Solver output marker meaning "could not solve" (not a hard error).
const NOT_SOLVED_MARKER: &str = "not solved";

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

/// stdout/stderr are drained by threads: a full pipe buffer (~64 KB) would
/// block the child forever while we poll `try_wait()`
struct SolverPipes {
    stdout: std::thread::JoinHandle<Vec<u8>>,
    stderr: std::thread::JoinHandle<Vec<u8>>,
}

fn drain_stream(stream: impl Read + Send + 'static) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let mut stream = stream;
        _ = stream.read_to_end(&mut buf);
        buf
    })
}

pub struct AstrometryPlateSolver {
    child:      Option<std::process::Child>,
    pipes:      Option<SolverPipes>,
    file_name:  Option<PathBuf>,
    mode:       Mode,
    config:     PlateSolveConfig,
    radius_try: RadiusTry,
    start_time: Instant,
    img_width:  Option<f64>, // radians, to optimize subsequent platesolves
    executable: ResolveExecutable, // which solver binary is in use
}

impl AstrometryPlateSolver {
    pub fn new() -> Self {
        Self {
            child:      None,
            pipes:      None,
            file_name:  None,
            mode:       Mode::None,
            config:     PlateSolveConfig::default(),
            radius_try: RadiusTry::Blind,
            start_time: Instant::now(),
            img_width:  None,
            executable: ResolveExecutable::SolveField,
        }
    }

    fn clear_prev_resources(&mut self) {
        if let Some(mut child) = self.child.take() {
            _ = child.kill();
            _ = child.wait();
        }
        // Drain threads end on pipe EOF after kill/wait; a surviving
        // grandchild holding the pipe fd can park one (holds no app state)
        self.pipes = None;

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
    fn exec_solver(&mut self) -> eyre::Result<()> {
        let Some(file_name) = self.file_name.clone() else {
            eyre::bail!("Calling exec_solver before saving input!");
        };
        use std::process::*;
        let (exe, executable) = resolve_executable();
        self.executable = executable;
        // `starsolver` is a stripped-down solver that accepts only a subset of
        // `solve-field` args and reads only `.xyls` star lists.
        let star_solver = executable == ResolveExecutable::StarSolver;
        let mut cmd = Command::new(&exe);
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            // App is a GUI subsystem binary; without this Windows allocates
            // a new console window for the child process.
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
        if !star_solver {
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
        }

        if star_solver
            && let Some(index_dir) = resolve_index_dir() {
            cmd.arg("--index-dir").arg(index_dir);
        }

        if let Some(img_width) = self.img_width {
            let degwidth = radian_to_degree(img_width);
            cmd
                .arg("--scale-units").arg("degwidth")
                .arg("--scale-low").arg((0.5*degwidth).to_string())
                .arg("--scale-high").arg((2.0*degwidth).to_string());
        }

        let mut blind = true;
        if self.config.eq_coord.is_some() || self.config.eq_coord_j2000.is_some() {
            let mut add_ra_and_dec = || {
                let crd_j2000 = if let Some(crd) = &self.config.eq_coord {
                    let j2000 = j2000_time();
                    let time = Utc::now().naive_utc();
                    let epoch_cvt = EpochCvt::new(&time, &j2000);
                    epoch_cvt.convert_eq(crd)
                } else if let Some(crd) = &self.config.eq_coord_j2000 {
                    *crd
                } else {
                    unreachable!()
                };
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
        let timeout = if blind { self.config.blind_timeout } else { self.config.timeout };

        cmd.arg("--cpulimit").arg(timeout.to_string());

        if let Mode::Stars { img_width, img_height } = &self.mode {
            cmd.arg("--width").arg(img_width.to_string());
            cmd.arg("--height").arg(img_height.to_string());
            if !star_solver {
                cmd.arg("--x-column").arg("X");
                cmd.arg("--y-column").arg("Y");
                cmd.arg("--sort-column").arg("FLUX");
            }
        }
        cmd.arg(&file_name);

        self.start_time = Instant::now();

        log::debug!("Running {:?} args={:?}", exe, cmd.get_args());
        let mut child = cmd.spawn().map_err(|e|
            eyre::format_err!("{} when trying to execute {:?}", e, exe)
        )?;
        let pipes = SolverPipes {
            stdout: drain_stream(child.stdout.take().unwrap()),
            stderr: drain_stream(child.stderr.take().unwrap()),
        };
        self.child = Some(child);
        self.pipes = Some(pipes);
        Ok(())
    }

    fn save_image_file(
        &mut self,
        image: &Image,
    ) -> eyre::Result<()> {
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
    ) -> eyre::Result<()> {
        self.clear_prev_resources();

        // save stars into FITS file

        const MAX_STARS_COUNT: usize = 500;

        let file_name = format!("astralite_platesolve_{}.xyls", rand::random::<u64>());
        let temp_file = std::env::temp_dir().join(&file_name);
        log::debug!("Saving stars into {:?} for plate solving...", temp_file);
        let mut file = std::fs::File::create(&temp_file)?;
        let fits_writer = FitsWriter::new();
        let mut main_header = Header::new();
        main_header.set_bool("SIMPLE", true);
        main_header.set_i64("BITPIX", 8);
        main_header.set_i64("NAXIS", 0);
        main_header.set_i64("IMAGEW", img_width as _);
        main_header.set_i64("IMAGEH", img_height as _);
        main_header.set_bool("EXTEND", true);
        fits_writer.write_header(&mut file, &main_header)?;
        let mut data = Vec::new();
        let stars_count = stars.len().min(MAX_STARS_COUNT);
        for star in &stars[..stars_count] {
            data.push(star.x + 1.0);
            data.push(star.y + 1.0);
            data.push(star.brightness as f64);
        }
        log::debug!("Saved stars count = {}", stars_count);
        let cols = [
            FitsTableCol { name: "X", type_: "1D", unit: "pix" },
            FitsTableCol { name: "Y", type_: "1D", unit: "pix" },
            FitsTableCol { name: "FLUX", type_: "1D", unit: "unknown" },
        ];
        fits_writer.write_header_and_bintable_f64(&mut file, &cols, &data)?;
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

    fn get_result_impl(&mut self) -> eyre::Result<PlateSolveResult> {
        if let Some(child) = &mut self.child {
            let exit_status = match child.try_wait() {
                Ok(Some(status)) => status,
                Err(e)           => return Err(e.into()),
                _                => return Ok(PlateSolveResult::Waiting),
            };
            // Child exited, so the drain threads are at EOF and join() is instant
            let (str_output, str_err) = match self.pipes.take() {
                Some(pipes) => {
                    let out = pipes.stdout.join().unwrap_or_default();
                    let err = pipes.stderr.join().unwrap_or_default();
                    (String::from_utf8_lossy(&out).into_owned(),
                     String::from_utf8_lossy(&err).into_owned())
                }
                None => (String::new(), String::new()),
            };

            if exit_status.success() {
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
                    log::error!("Can't extract data from solver stdout");
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

                // convert plate solving coordinates from J2000 to now
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

                self.img_width = Some(f64::min(result.width, result.height));

                return Ok(PlateSolveResult::Done(result));
            } else {
                self.child = None;

                // `starsolver` exits non-zero when it simply cannot solve the
                // field; report Failed so the retry logic can widen the search.
                // Keep the original hard-error behavior for `solve-field`.
                if self.executable == ResolveExecutable::StarSolver
                    && format!("{}\n{}", str_output, str_err)
                        .to_lowercase().contains(NOT_SOLVED_MARKER) {
                    log::debug!("Platesolver did not solve the field");
                    return Ok(PlateSolveResult::Failed);
                }

                let exe_name = self.executable.fname();
                return Err(eyre::format_err!(
                    "{} exited with code {}\n\n{}",
                    exe_name,
                    exit_status.code().unwrap_or_default(),
                    if str_err.is_empty() { str_output } else { str_err }
                ));
            }
        }

        eyre::bail!("Not started!");
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
    ) -> eyre::Result<()> {
        if self.child.is_some() {
            eyre::bail!("AstrometryPlateSolver already started");
        }
        self.config = config.clone();
        self.radius_try = RadiusTry::First;
        match data {
            PlateSolverInData::Image(image) =>
                self.save_image_file(image)?,
            PlateSolverInData::Stars{ stars, img_width, img_height } =>
                self.save_stars_file(stars, *img_width, *img_height)?,
        }
        self.exec_solver()?;
        Ok(())
    }

    fn abort(&mut self) {
        self.clear_prev_resources();
    }

    fn get_result(&mut self) -> eyre::Result<PlateSolveResult> {
        let result = self.get_result_impl();

        // Try to restart platesolver
        if matches!(result, Ok(PlateSolveResult::Failed))
        && self.config.eq_coord.is_some() {
            match (&self.radius_try, self.config.allow_blind) {
                (RadiusTry::First, _) => {
                    log::debug!("Restarting platesolver");
                    self.radius_try = RadiusTry::Second;
                    self.exec_solver()?;
                    return Ok(PlateSolveResult::Waiting);
                }
                (RadiusTry::Second, true) => {
                    log::debug!("Restarting platesolver");
                    self.radius_try = RadiusTry::Blind;
                    self.exec_solver()?;
                    return Ok(PlateSolveResult::Waiting);
                }
                _ => {
                    return result;
                }
            }
        }
        result
    }

    fn reset(&mut self) {
        self.img_width = None;
    }
}
