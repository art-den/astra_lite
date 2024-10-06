use astrometry::*;
use crate::{image::image::Image, ui::sky_map::math::EqCoord, options::PlateSolverType};

mod astrometry;

#[derive(Debug, Default)]
pub struct PlateSolveConfig {
    pub eq_coord: Option<EqCoord>,
    pub time_out: u32, // in seconds
}

#[derive(Debug)]
pub struct PlateSolveResult {
    pub eq_coord: EqCoord,
    pub width: f64,
    pub height: f64,
    pub rotation: f64,
}

pub struct PlateSolver {
    solver: Box<dyn PlateSolverIface + Sync + Send + 'static>,
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

    pub fn start(
        &mut self,
        image:  &Image,
        config: &PlateSolveConfig
    ) -> anyhow::Result<()> {
        self.solver.start(image, config)?;
        Ok(())
    }

    pub fn get_result(&mut self) -> Option<anyhow::Result<PlateSolveResult>> {
        self.solver.get_result()
    }
}

trait PlateSolverIface {
    fn start(&mut self, image: &Image, config: &PlateSolveConfig) -> anyhow::Result<()>;
    fn get_result(&mut self) -> Option<anyhow::Result<PlateSolveResult>>;
}