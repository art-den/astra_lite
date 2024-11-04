use astrometry::*;
use chrono::{DateTime, Utc};
use crate::{image::{image::Image, stars::Stars}, options::PlateSolverType, ui::sky_map::math::EqCoord};

mod astrometry;

#[derive(Debug, Default, Clone)]
pub struct PlateSolveConfig {
    pub eq_coord:       Option<EqCoord>,
    pub time_out:       u32, // in seconds
    pub blind_time_out: u32, // in seconds
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
        stars:      &'a Stars,
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
        self.config = config.clone();
        self.solver.start(data, config)?;
        Ok(())
    }

    pub fn get_result(&mut self) -> anyhow::Result<PlateSolveResult> {
        let result = self.solver.get_result();

        if matches!(result, Ok(PlateSolveResult::Failed))
        && self.config.eq_coord.is_some()
        && self.solver.support_coordinates() {
            log::debug!("Restarting platesolver in blind mode...");
            self.config.eq_coord = None;
            self.solver.restart(&self.config)?;
            return Ok(PlateSolveResult::Waiting);
        }
        result
    }
}

trait PlateSolverIface {
    fn support_stars_as_input(&self) -> bool;
    fn support_coordinates(&self) -> bool;
    fn start(&mut self, data: &PlateSolverInData, config: &PlateSolveConfig) -> anyhow::Result<()>;
    fn restart(&mut self, config: &PlateSolveConfig) -> anyhow::Result<()>;
    fn get_result(&mut self) -> anyhow::Result<PlateSolveResult>;
}

#[derive(Clone)]
pub struct PlateSolverEvent {
    pub cam_name: String,
    pub result:   PlateSolveOkResult,
}
