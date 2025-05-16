use std::sync::Arc;

use astrometry::*;
use chrono::{DateTime, Utc};
use crate::{image::{image::Image, preview::PreviewRgbData, stars::StarItems}, options::PlateSolverType, sky_math::math::*};

mod astrometry;

#[derive(Debug, Clone)]
pub struct PlateSolveConfig {
    pub eq_coord:       Option<EqCoord>,
    pub eq_coord_j2000: Option<EqCoord>,
    pub time_out:       u32, // in seconds
    pub blind_time_out: u32, // in seconds
    pub allow_blind:    bool,
}

impl Default for PlateSolveConfig {
    fn default() -> Self {
        Self {
            eq_coord:       None,
            eq_coord_j2000: None,
            time_out:       10,
            blind_time_out: 30,
            allow_blind:    true
        }
    }
}

#[derive(Debug, Clone)]
pub struct PlateSolveOkResult {
    pub crd_j2000: EqCoord,
    pub crd_now:   EqCoord,
    pub width:     f64,
    pub height:    f64,
    pub rotation:  f64,
    pub time:      DateTime<Utc>,
}

impl PlateSolveOkResult {
    pub fn print_to_log(&self) {
        use crate::indi::value_to_sexagesimal;
        log::debug!(
            "plate solver j2000 = (ra: {}, dec: {}), now = (ra: {}, dec: {}), image size = {:.6} x {:.6}",
            value_to_sexagesimal(radian_to_hour(self.crd_j2000.ra), true, 9),
            value_to_sexagesimal(radian_to_degree(self.crd_j2000.dec), true, 8),
            value_to_sexagesimal(radian_to_hour(self.crd_now.ra), true, 9),
            value_to_sexagesimal(radian_to_degree(self.crd_now.dec), true, 8),
            radian_to_degree(self.width),
            radian_to_degree(self.height),
        );
    }
}

pub enum PlateSolveResult {
    Waiting,
    Done(PlateSolveOkResult),
    Failed,
}

pub struct PlateSolver {
    solver: Box<dyn PlateSolverIface + Sync + Send + 'static>,
    config: PlateSolveConfig,
}

pub enum PlateSolverInData<'a> {
    Image(&'a Image),
    Stars{
        stars:      &'a StarItems,
        img_width:  usize,
        img_height: usize,
    },
}

impl PlateSolver {
    pub fn new(tp: PlateSolverType) -> Self {
        let solver = match tp {
            PlateSolverType::Astrometry =>
                Box::new(AstrometryPlateSolver::new()),
        };
        Self {
            solver,
            config: PlateSolveConfig::default(),
        }
    }

    pub fn support_stars_as_input(&self) -> bool {
        self.solver.support_stars_as_input()
    }

    pub fn start(
        &mut self,
        data:   &PlateSolverInData,
        config: &PlateSolveConfig
    ) -> anyhow::Result<()> {
        match data {
            PlateSolverInData::Image(image) => {
                if image.is_empty() {
                    anyhow::bail!("Image is empty!");
                }
            }
            PlateSolverInData::Stars { stars, .. } => {
                if stars.is_empty() {
                    anyhow::bail!("No stars for platesolving!");
                }
            }
        }

        log::debug!("Starting platesolve with config={:?} ...", config);
        match data {
            PlateSolverInData::Image(image) => {
                log::debug!(
                    "Platesolve source is image (height={}, width={})",
                    image.width(), image.height()
                );
            }
            PlateSolverInData::Stars { stars, img_width, img_height } => {
                log::debug!(
                    "Platesolve source is stars (count={}, img_width={}, img_height={})",
                    stars.len(), img_width, img_height
                );
            }
        }

        self.config = config.clone();
        self.solver.start(data, config)?;
        Ok(())
    }

    pub fn abort(&mut self) {
        self.solver.abort();
    }

    pub fn get_result(&mut self) -> anyhow::Result<PlateSolveResult> {
        self.solver.get_result()
    }
}

trait PlateSolverIface {
    fn support_stars_as_input(&self) -> bool;
    fn start(&mut self, data: &PlateSolverInData, config: &PlateSolveConfig) -> anyhow::Result<()>;
    fn abort(&mut self);
    fn get_result(&mut self) -> anyhow::Result<PlateSolveResult>;
}

#[derive(Clone)]
pub struct PlateSolverEvent {
    pub cam_name: String,
    pub result:   PlateSolveOkResult,
    pub preview:  Option<Arc<PreviewRgbData>>,
}
