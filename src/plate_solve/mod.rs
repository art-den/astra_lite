use astrometry::*;
use crate::{image::image::Image, ui::sky_map::math::EqCoord, options::PlateSolverType};

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
        data:   &PlateSolverInData,
        config: &PlateSolveConfig
    ) -> anyhow::Result<()> {
        self.solver.start(data, config)?;
        Ok(())
    }

    pub fn get_result(&mut self) -> Option<anyhow::Result<PlateSolveResult>> {
        self.solver.get_result()
    }
}

trait PlateSolverIface {
    fn start(&mut self, data: &PlateSolverInData, config: &PlateSolveConfig) -> anyhow::Result<()>;
    fn get_result(&mut self) -> Option<anyhow::Result<PlateSolveResult>>;
}

#[derive(Clone)]
pub struct PlateSolverEvent {
    pub cam_name: String,
    pub result:   PlateSolveResult,
}
