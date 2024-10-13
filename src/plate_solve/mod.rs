use astrometry::*;
use crate::{image::{image::Image, stars::Stars}, options::PlateSolverType, ui::sky_map::math::EqCoord};

mod astrometry;

#[derive(Debug, Default)]
pub struct PlateSolveConfig {
    pub eq_coord: Option<EqCoord>,
    pub time_out: u32, // in seconds
}

#[derive(Debug, Clone)]
pub struct PlateSolveResult {
    pub crd_j2000: EqCoord,
    pub crd_now: EqCoord,
    pub width: f64,
    pub height: f64,
    pub rotation: f64,
}

pub struct PlateSolver {
    solver: Box<dyn PlateSolverIface + Sync + Send + 'static>,
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
            solver
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

        self.solver.start(data, config)?;
        Ok(())
    }

    pub fn get_result(&mut self) -> Option<anyhow::Result<PlateSolveResult>> {
        self.solver.get_result()
    }
}

trait PlateSolverIface {
    fn support_stars_as_input(&self) -> bool;
    fn start(&mut self, data: &PlateSolverInData, config: &PlateSolveConfig) -> anyhow::Result<()>;
    fn get_result(&mut self) -> Option<anyhow::Result<PlateSolveResult>>;
}

#[derive(Clone)]
pub struct PlateSolverEvent {
    pub cam_name: String,
    pub result:   PlateSolveResult,
}
