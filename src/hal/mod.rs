#![allow(dead_code)]

pub mod indi;
pub mod events;
pub mod hal_indi;
pub mod hal_ascom_alpaca;

use bitflags::bitflags;
use serde::{Deserialize, Serialize};
use std::{ops:: RangeInclusive, sync::{Arc, RwLock}};

use crate::hal::{events::{HalEvent, HalEventSubscribers}, hal_indi::IndiHalImpl};

bitflags! {
    #[derive(Debug, Clone, Copy)]
    pub struct DeviceType: u32 {
        const CAMERA =    (1 << 0);
        const TELESCOPE = (1 << 1);
        const FOCUSER =   (1 << 2);
    }
}

#[derive(Debug)]
pub struct DeviceInfo {
    pub id:    String,
    pub name:  String,
    pub type_: DeviceType,
}

#[derive(Debug, PartialEq)]
pub enum HalState {
    ImplNotDefined,
    Connecting,
    Connected,
    Disconnecting,
    Disconnected,
    Error(String),
}

pub struct Hal {
    impl_:            RwLock<Option<Arc<dyn HalImpl + Send + Sync + 'static>>>,
    event_subscibers: Arc<HalEventSubscribers>,
}

impl Hal {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            impl_:            RwLock::new(None),
            event_subscibers: Arc::new(HalEventSubscribers::new()),
        })
    }

    pub fn create_indy_impl(&self, indi: &Arc<indi::Connection>) -> Arc<IndiHalImpl> {
        IndiHalImpl::new(indi, &self.event_subscibers)
    }

    pub fn set_impl(&self, hal_impl: Arc<dyn HalImpl + Send + Sync + 'static>) {
        let mut impl_ = self.impl_.write().unwrap();
        *impl_ = Some(hal_impl);
    }

    pub fn reset_impl(&self) {
        let mut impl_ = self.impl_.write().unwrap();
        *impl_ = None;
    }

    pub fn connect_event_handler(&self, fun: impl Fn(HalEvent) + Send + Sync + 'static) {
        self.event_subscibers.connect_event_handler(fun);
    }

    pub fn disconnect_all_subscribers(&self) {
        self.event_subscibers.disconnect_all_subscribers();
    }

    pub fn state(&self) -> HalState {
        let impl_ = self.impl_.read().unwrap();
        if let Some(impl_) = &*impl_ {
            impl_.state()
        } else {
            HalState::ImplNotDefined
        }
    }

    pub fn notify_periodical_timer_tick(&self, timer_period: usize) -> anyhow::Result<()> {
        let impl_ = self.impl_.read().unwrap();
        if let Some(impl_) = &*impl_ {
            impl_.notify_periodical_timer_tick(timer_period)
        } else {
            Ok(())
        }
    }

    pub fn devices(&self, type_filter: DeviceType) -> anyhow::Result<Vec<DeviceInfo>> {
        let impl_ = self.impl_.read().unwrap();
        if let Some(impl_) = &*impl_ {
            impl_.devices(type_filter)
        } else {
            anyhow::bail!("HAL is not selected!");
        }
    }

    pub fn camera(&self, id: &str) -> anyhow::Result<Arc<dyn Camera + Send + Sync>> {
        let impl_ = self.impl_.read().unwrap();
        if let Some(impl_) = &*impl_ {
            impl_.camera(id)
        } else {
            anyhow::bail!("HAL is not selected!");
        }
    }

    pub fn telescope(&self, id: &str) -> anyhow::Result<Arc<dyn Telescope + Send + Sync>> {
        let impl_ = self.impl_.read().unwrap();
        if let Some(impl_) = &*impl_ {
            impl_.telescope(id)
        } else {
            anyhow::bail!("HAL is not selected!");
        }
    }

    pub fn focuser(&self, id: &str) -> anyhow::Result<Arc<dyn Focuser + Send + Sync>> {
        let impl_ = self.impl_.read().unwrap();
        if let Some(impl_) = &*impl_ {
            impl_.focuser(id)
        } else {
            anyhow::bail!("HAL is not selected!");
        }
    }
}

pub trait HalImpl {
    fn state(&self) -> HalState;
    fn notify_periodical_timer_tick(&self, timer_period: usize) -> anyhow::Result<()>;
    fn devices(&self, type_filter: DeviceType) -> anyhow::Result<Vec<DeviceInfo>>;
    fn camera(&self, id: &str) -> anyhow::Result<Arc<dyn Camera + Send + Sync>>;
    fn telescope(&self, id: &str) -> anyhow::Result<Arc<dyn Telescope + Send + Sync>>;
    fn focuser(&self, id: &str) -> anyhow::Result<Arc<dyn Focuser + Send + Sync>>;
}

///////////////////////////////////////////////////////////////////////////////
// Device

pub trait Device {
    fn id(&self) -> &str;
    fn is_active(&self) -> anyhow::Result<bool>;
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

pub enum CcdPurpose {
    Main,
    Guider,
}

pub trait Camera : Device {
    fn init_before_shot(&self) -> anyhow::Result<()>;

    fn ccd_type(&self) -> CcdPurpose; // For multy-CCD cameras

    // Exposure
    fn exposure_range(&self) -> anyhow::Result<RangeInclusive<f64>>;
    fn start_exposure(&self, value: f64) -> anyhow::Result<()>;
    fn abort_exposure(&self) -> anyhow::Result<()>;
    fn remaining_time(&self) -> anyhow::Result<f64>;

    // Frame type
    fn set_frame_type(&self, frame_type: FrameType) -> anyhow::Result<()>;

    // Frame
    fn pixel_size_um(&self) -> anyhow::Result<(f64, f64)>;
    fn is_frame_supported(&self) -> anyhow::Result<bool>;
    fn ccd_size(&self) -> anyhow::Result<(usize, usize)>;
    fn set_frame(&self, x: usize, y: usize, width: usize, height: usize) -> anyhow::Result<()>;

    // Gain
    fn is_gain_supported(&self) -> anyhow::Result<bool>;
    fn gain_range(&self) -> anyhow::Result<RangeInclusive<f64>>;
    fn set_gain(&self, value: f64) -> anyhow::Result<()>;

    // Offset
    fn is_offset_supported(&self) -> anyhow::Result<bool>;
    fn offset_range(&self) -> anyhow::Result<RangeInclusive<f64>>;
    fn set_offset(&self, value: f64) -> anyhow::Result<()>;

    // Bin
    fn is_binning_supported(&self) -> anyhow::Result<bool>;
    fn max_binning(&self) -> anyhow::Result<(usize/*x*/, usize/*y*/)>;
    fn set_binning(&self, bin_x: usize, bin_y: usize) -> anyhow::Result<()>;

    // Cooler
    fn is_cooler_supported(&self) -> anyhow::Result<bool>;
    fn temperature(&self) -> anyhow::Result<f64>;
    fn temperature_range(&self) -> anyhow::Result<RangeInclusive<f64>>;
    fn set_temperature(&self, temperature: Option<f64>) -> anyhow::Result<()>;

    // Heater
    fn is_heater_supported(&self) -> anyhow::Result<bool>;
    fn heater_ctrl_list(&self) -> anyhow::Result<Vec<(String/*id*/, String/*text*/)>>;
    fn control_heater(&self, id: &str) -> anyhow::Result<()>;

    // Fan
    fn is_fan_ctrl_supported(&self) -> anyhow::Result<bool>;
    fn enable_fan(&self, enable: bool) -> anyhow::Result<()>;

    // Low noise mode
    fn is_low_noise_supported(&self) -> anyhow::Result<bool>;
    fn enable_low_noise_mode(&self, enable: bool) -> anyhow::Result<()>;

    // High fullwell mode
    fn is_high_fullwell_supported(&self) -> anyhow::Result<bool>;
    fn enable_high_fullwell_mode(&self, enable: bool) -> anyhow::Result<()>;

    // Conversion gain
    fn is_conversion_gain_supported(&self) -> anyhow::Result<bool>;
    fn conversion_gain_list(&self) -> anyhow::Result<Vec<(String/*id*/, String/*text*/)>>;
    fn set_conversion_gain(&self, id: &str) -> anyhow::Result<()>;
}

///////////////////////////////////////////////////////////////////////////////
// Telescope (mount)

pub trait Telescope : Device {
    fn is_abort_motion_supported(&self) -> bool;
    fn abort_motion(&self) -> anyhow::Result<()>;

    fn is_parked(&self) -> anyhow::Result<bool>;
    fn park(&self) -> anyhow::Result<()>;
    fn unpark(&self) -> anyhow::Result<()>;

    fn set_slew_speed(&self, speed_id: &str) -> anyhow::Result<()>;
    fn eq_coord(&self) -> anyhow::Result<(f64/*ra*/, f64/*dec*/)>;
    fn goto_and_track(&self, ra: f64, dec: f64) -> anyhow::Result<()>;
    fn is_slewing(&self) -> anyhow::Result<bool>;

    fn sync(&self, ra: f64, dec: f64) -> anyhow::Result<()>;

    fn is_guide_rate_supported(&self) -> anyhow::Result<bool>;
    fn guide_rate(&self) -> anyhow::Result<(f64/*ra*/, f64/*dec*/)>;
    fn can_set_guide_rate(&self) -> anyhow::Result<bool>;
    fn set_guide_rate(&self, rate_ns: f64, rate_we: f64) -> anyhow::Result<()>;
    fn pulse_guide(&self, duration_ns: f64, duration_we: f64) -> anyhow::Result<()>;
    fn is_pulse_guiding(&self) -> anyhow::Result<bool>;
}

///////////////////////////////////////////////////////////////////////////////
// Focuser

pub trait Focuser : Device {
    fn abs_position_range(&self) -> anyhow::Result<RangeInclusive<f64>>;
    fn abs_position(&self) -> anyhow::Result<f64>;
    fn set_abs_position(&self, value: f64) -> anyhow::Result<()>;
    fn temperature(&self) -> anyhow::Result<f64>;
}
