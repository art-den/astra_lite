#![allow(dead_code)]

pub mod indi;
pub mod events;
pub mod hal_indi;

#[cfg(windows)]
pub mod hal_ascom_alpaca;

use bitflags::bitflags;
use serde::{Deserialize, Serialize};
use std::{ops:: RangeInclusive, path::Path, sync::Arc};

use crate::hal::{events::{HalEvent, HalEventHandlers}, hal_indi::IndiHalImpl};

#[cfg(windows)]
use super::hal::hal_ascom_alpaca::AscomAlpacaHalImpl;

bitflags! {
    #[derive(Debug, Clone, Copy)]
    pub struct DeviceType: u32 {
        const CAMERA    = (1 << 0);
        const TELESCOPE = (1 << 1);
        const FOCUSER   = (1 << 2);
        const FLT_WHEEL = (1 << 3);
    }
}

#[derive(Debug, Clone)]
pub struct DeviceInfo {
    pub id:    String,
    pub name:  String,
    pub type_: DeviceType,
}

#[derive(PartialEq, Debug, Clone, Copy)]
pub enum CcdPurpose {
    MainTelescopeCcd,
    SecondaryTelescopeCcd,
    GuiderCcd,
    Unknown,
}

pub struct CameraInfo {
    pub id:    String,
    pub name:  String,
    pub ccd:   CcdPurpose,
}

#[derive(Clone, Debug, PartialEq)]
pub enum HalState {
    ImplNotDefined,
    Connecting,
    Connected,
    Disconnecting,
    Disconnected,
    Error(String),
}

pub struct Hal {
    list:           Vec<Arc<dyn HalImpl + Send + Sync + 'static>>,
    indi:           Arc<IndiHalImpl>,
    #[cfg(windows)]
    ascom_alpaca:   Arc<AscomAlpacaHalImpl>,
    event_handlers: Arc<HalEventHandlers>,
}

impl Hal {
    pub fn new() -> Arc<Self> {
        let event_handlers = Arc::new(HalEventHandlers::new());
        let indi = IndiHalImpl::new(&event_handlers);

        let mut list: Vec<Arc<dyn HalImpl + Send + Sync + 'static>> = Vec::new();
        list.push(Arc::clone(&indi) as Arc<_>);

        #[cfg(windows)]
        let ascom_alpaca = AscomAlpacaHalImpl::new(&event_handlers);
        #[cfg(windows)]
        list.push(Arc::clone(&ascom_alpaca) as Arc<_>);

        Arc::new(Self {
            #[cfg(windows)]
            ascom_alpaca,
            indi,
            list,
            event_handlers
        })
    }

    pub fn indi_impl(&self) -> &Arc<IndiHalImpl> {
        &self.indi
    }

    #[cfg(windows)]
    pub fn ascom_alpaca_impl(&self) -> &Arc<AscomAlpacaHalImpl> {
        &self.ascom_alpaca
    }

    pub fn connect_event_handler(&self, fun: impl Fn(HalEvent) + Send + Sync + 'static) {
        self.event_handlers.connect(fun);
    }

    pub fn disconnect_all_subscribers(&self) {
        self.event_handlers.disconnect_all();
    }

    pub fn notify_periodical_timer_tick(&self, timer_period_ms: usize) -> eyre::Result<()> {
        for impl_ in &self.list {
            impl_.notify_periodical_timer_tick(timer_period_ms)?;
        }
        Ok(())
    }

    pub fn devices(&self, type_filter: DeviceType) -> eyre::Result<Vec<DeviceInfo>> {
        let result = self.list
            .iter()
            .filter_map(|hal| hal.devices(type_filter).ok())
            .flatten()
            .collect();
        Ok(result)
    }

    pub fn cameras(&self) -> eyre::Result<Vec<CameraInfo>> {
        let result = self.list
            .iter()
            .filter_map(|hal| hal.cameras().ok())
            .flatten()
            .collect();
        Ok(result)
    }

    pub fn camera(&self, id: &str) -> eyre::Result<Arc<dyn Camera + Send + Sync>> {
        let camera = self.list
            .iter()
            .filter_map(|hal| hal.camera(id))
            .next();
        if let Some(camera) = camera {
            return Ok(camera);
        } else {
            eyre::bail!("Camera with id={id} not found");
        }
    }

    pub fn telescope(&self, id: &str) -> eyre::Result<Arc<dyn Telescope + Send + Sync>> {
        let telescope = self.list
            .iter()
            .filter_map(|hal| hal.telescope(id))
            .next();
        if let Some(telescope) = telescope {
            return Ok(telescope);
        } else {
            eyre::bail!("Telescope with id={id} not found");
        }
    }

    pub fn focuser(&self, id: &str) -> eyre::Result<Arc<dyn Focuser + Send + Sync>> {
        let focuser = self.list
            .iter()
            .filter_map(|hal| hal.focuser(id))
            .next();
        if let Some(focuser) = focuser {
            return Ok(focuser);
        } else {
            eyre::bail!("Focuser with id={id} not found");
        }
    }

    pub fn filter_wheel(&self, id: &str) -> eyre::Result<Arc<dyn FilterWheel + Send + Sync>> {
        let filter_wheel = self.list
            .iter()
            .filter_map(|hal| hal.filter_wheel(id))
            .next();
        if let Some(filter_wheel) = filter_wheel {
            return Ok(filter_wheel);
        } else {
            eyre::bail!("Filter wheel with id={id} not found");
        }
    }
}

pub trait HalImpl {
    fn state(&self) -> HalState;
    fn disconnect(&self) -> eyre::Result<()>;
    fn notify_periodical_timer_tick(&self, timer_period_ms: usize) -> eyre::Result<()>;
    fn devices(&self, type_filter: DeviceType) -> eyre::Result<Vec<DeviceInfo>>;
    fn cameras(&self) -> eyre::Result<Vec<CameraInfo>>;
    fn camera(&self, id: &str) -> Option<Arc<dyn Camera + Send + Sync>>;
    fn telescope(&self, id: &str) -> Option<Arc<dyn Telescope + Send + Sync>>;
    fn focuser(&self, id: &str) -> Option<Arc<dyn Focuser + Send + Sync>>;
    fn filter_wheel(&self, id: &str) -> Option<Arc<dyn FilterWheel + Send + Sync>>;
}

///////////////////////////////////////////////////////////////////////////////
// Device

pub trait Device {
    fn id(&self) -> &str;
    fn name(&self) -> &str;
    fn is_active(&self) -> eyre::Result<bool>;
}

///////////////////////////////////////////////////////////////////////////////
// Camera

#[derive(Debug, Serialize, Deserialize, PartialEq, Clone, Copy, Default)]
pub enum FrameType {
    #[default]
    Lights,
    Flats,
    Darks,
    Biases,
}

#[derive(PartialEq, Clone, Copy)]
pub enum CameraShotType {
    RawCcdData,
    ReadyImage,
}

bitflags! {
    #[derive(Debug, Clone, Copy)]
    pub struct CameraFeatures: u32 {
        const CAN_START_EXP_AT_DOWNLOAD_BEGIN = (1 << 0);
    }
}

/// Interface to extract data from camera shot
pub trait CameraShot {
    fn get_type(&self) -> CameraShotType;
    fn get_raw(&self) -> eyre::Result<crate::image::raw::RawImage>;
    fn get_image(&self, image: &mut crate::image::image::Image) -> eyre::Result<()>;
    fn download_time(&self) -> f64;
    fn file_ext(&self) -> &str;
    fn save_to_file(&self, file_name: &Path) -> eyre::Result<()>;
}

pub trait Camera : Device {
    fn features(&self) -> CameraFeatures;
    fn init_before_shot(&self) -> eyre::Result<()>;

    // Exposure
    fn exposure_range(&self) -> eyre::Result<RangeInclusive<f64>>;
    fn start_exposure(&self, duration: f64) -> eyre::Result<()>;
    fn abort_exposure(&self) -> eyre::Result<()>;
    fn remaining_time(&self) -> Option<f64>;

    // Frame type
    fn set_frame_type(&self, frame_type: FrameType) -> eyre::Result<()>;

    // Frame
    fn pixel_size_um(&self) -> eyre::Result<(f64, f64)>;
    fn is_frame_supported(&self) -> eyre::Result<bool>;
    fn ccd_size(&self) -> eyre::Result<(usize, usize)>;
    fn set_frame(&self, x: usize, y: usize, width: usize, height: usize) -> eyre::Result<()>;

    // Gain
    fn is_gain_supported(&self) -> eyre::Result<bool>;
    fn gain_range(&self) -> eyre::Result<RangeInclusive<f64>>;
    fn set_gain(&self, value: f64) -> eyre::Result<()>;

    // Offset
    fn is_offset_supported(&self) -> eyre::Result<bool>;
    fn offset_range(&self) -> eyre::Result<RangeInclusive<f64>>;
    fn set_offset(&self, value: f64) -> eyre::Result<()>;

    // Bin
    fn is_binning_supported(&self) -> eyre::Result<bool>;
    fn max_binning(&self) -> eyre::Result<(usize/*x*/, usize/*y*/)>;
    fn set_binning(&self, bin_x: usize, bin_y: usize) -> eyre::Result<()>;

    // Cooler
    fn is_cooler_supported(&self) -> eyre::Result<bool>;
    fn temperature(&self) -> eyre::Result<f64>;
    fn temperature_range(&self) -> eyre::Result<RangeInclusive<f64>>;
    fn set_temperature(&self, temperature: Option<f64>) -> eyre::Result<()>;

    // Heater
    fn is_heater_supported(&self) -> eyre::Result<bool>;
    fn heater_ctrl_list(&self) -> eyre::Result<Vec<(String/*id*/, String/*text*/)>>;
    fn control_heater(&self, id: &str) -> eyre::Result<()>;

    // Fan
    fn is_fan_ctrl_supported(&self) -> eyre::Result<bool>;
    fn enable_fan(&self, enable: bool) -> eyre::Result<()>;

    // Low noise mode
    fn is_low_noise_supported(&self) -> eyre::Result<bool>;
    fn enable_low_noise_mode(&self, enable: bool) -> eyre::Result<()>;

    // High fullwell mode
    fn is_high_fullwell_supported(&self) -> eyre::Result<bool>;
    fn enable_high_fullwell_mode(&self, enable: bool) -> eyre::Result<()>;

    // Conversion gain
    fn is_conversion_gain_supported(&self) -> eyre::Result<bool>;
    fn conversion_gain_list(&self) -> eyre::Result<Vec<(String/*id*/, String/*text*/)>>;
    fn set_conversion_gain(&self, id: &str) -> eyre::Result<()>;

    // Telescope
    fn set_telescope_focal_len(&self, focal_len: f64) -> eyre::Result<()>;
}

///////////////////////////////////////////////////////////////////////////////
// Telescope (mount)

pub enum TelescopeMoveDir {
    North, South, West, East,
    NorthWest, NorthEast, SouthWest, SouthEast,
}

#[derive(Clone, Copy, Eq, PartialEq, Default)]
pub enum TelescopeState {
    #[default]
    Stopped,
    Parked,
    Tracking,
    Slewing,
    Error,
    Correction,
    Moved,
}

pub struct TelescopeSite {
    pub latitude: f64,
    pub longitude: f64,
    pub elevation: f64,
}

pub trait Telescope : Device {
    fn state(&self) -> eyre::Result<TelescopeState>;
    fn site(&self) -> eyre::Result<TelescopeSite>;

    fn is_abort_motion_supported(&self) -> bool;
    fn abort_motion(&self) -> eyre::Result<()>;

    fn is_parked(&self) -> eyre::Result<bool>;
    fn park(&self) -> eyre::Result<()>;
    fn unpark(&self) -> eyre::Result<()>;

    fn is_tracking(&self) -> eyre::Result<bool>;
    fn track(&self, enabled: bool) -> eyre::Result<()>;

    fn revert_motion(&self, reverse_ns: bool, reverse_we: bool) -> eyre::Result<()>;
    fn move_(&self, direction: TelescopeMoveDir) -> eyre::Result<()>;

    fn slew_speed_list(&self) -> eyre::Result<Vec<(String/*id*/, String/*text*/)>>;
    fn set_slew_speed(&self, speed_id: &str) -> eyre::Result<()>;
    fn eq_coord(&self) -> eyre::Result<(f64/*ra*/, f64/*dec*/)>;
    fn goto_and_track(&self, ra: f64, dec: f64) -> eyre::Result<()>;
    fn is_slewing(&self) -> eyre::Result<bool>;

    fn sync(&self, ra: f64, dec: f64) -> eyre::Result<()>;

    fn is_guide_rate_supported(&self) -> eyre::Result<bool>;
    fn guide_rate(&self) -> eyre::Result<(f64/*ns*/, f64/*we*/)>;
    fn pulse_max_duration(&self) -> eyre::Result<(f64/*ns*/, f64/*we*/)>;
    fn can_set_guide_rate(&self) -> eyre::Result<bool>;
    fn set_guide_rate(&self, rate_ns: f64, rate_we: f64) -> eyre::Result<()>;
    fn pulse_guide(&self, duration_ns: f64, duration_we: f64) -> eyre::Result<()>;
    fn is_pulse_guiding(&self) -> eyre::Result<bool>;
}

///////////////////////////////////////////////////////////////////////////////
// Focuser

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum FocuserState {
    Stopped,
    Moving,
    Error,
}

pub trait Focuser : Device {
    fn state(&self) -> eyre::Result<FocuserState>;
    fn abs_position_range(&self) -> eyre::Result<RangeInclusive<f64>>;
    fn abs_position(&self) -> eyre::Result<f64>;
    fn set_abs_position(&self, value: f64) -> eyre::Result<()>;
    fn temperature(&self) -> eyre::Result<f64>;
}

///////////////////////////////////////////////////////////////////////////////
// Filter wheel

pub trait FilterWheel : Device {
    fn list_and_active(&self) -> eyre::Result<(Vec<String>, usize)>;
    fn set_active(&self, active_elem: usize) -> eyre::Result<()>;
}
