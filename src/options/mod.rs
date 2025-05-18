pub mod indi;
pub mod camera;

pub use indi::*;
pub use camera::*;

pub mod calibration;
pub use calibration::*;

pub mod raw;
pub use raw::*;

pub mod live_stacking;
pub use live_stacking::*;

pub mod quality;
pub use quality::*;

pub mod preview;
pub use preview::*;

pub mod focuser;
pub use focuser::*;

pub mod plate_solver;
pub use plate_solver::*;

pub mod site;
pub use site::*;

pub mod mount;
pub use mount::*;

pub mod telescope;
pub use telescope::*;

pub mod guiding;
pub use guiding::*;

pub mod polar_align;
pub use polar_align::*;

use std::collections::HashMap;

use serde::{Serialize, Deserialize};


#[derive(Serialize, Deserialize, Debug, Default, Copy, Clone, PartialEq)]
pub enum Gain {
    #[default]Same,
    Min,
    P25,
    P50,
    P75,
    Max
}

#[derive(Serialize, Deserialize, Debug, Default)]
#[serde(default)]
pub struct Options {
    pub indi:         IndiOptions,
    pub cam:          CamOptions,
    pub sep_cam:      HashMap<String, SeparatedCamOptions>,
    pub calibr:       CalibrOptions,
    pub raw_frames:   RawFrameOptions,
    pub live:         LiveStackingOptions,
    pub quality:      QualityOptions,
    pub preview:      PreviewOptions,
    pub focuser:      FocuserOptions,
    pub sep_focuser:  HashMap<String, SeparatedFocuserOptions>,
    pub plate_solver: PlateSolverOptions,
    pub sep_ps:       HashMap<String, SeparatedPlateSolverOptions>,
    pub mount:        MountOptions,
    pub telescope:    TelescopeOptions,
    pub site:         SiteOptions,
    pub guiding:      GuidingOptions,
    pub sep_guiding:  HashMap<String, SeparatedGuidingOptions>,
    pub polar_align:  PloarAlignOptions,
}

impl Options {
    pub fn check(&mut self) -> anyhow::Result<()> {
        self.calibr.check()?;
        self.raw_frames.check()?;
        self.live.check()?;
        Ok(())
    }
}
