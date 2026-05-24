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

    pub fn state(&self) -> eyre::Result<HalState> {
        let impl_ = self.impl_.read().unwrap();
        if let Some(impl_) = &*impl_ {
            Ok(impl_.state()?)
        } else {
            Ok(HalState::ImplNotDefined)
        }
    }

    pub fn devices(&self, type_filter: DeviceType) -> eyre::Result<Vec<DeviceInfo>> {
        let impl_ = self.impl_.read().unwrap();
        if let Some(impl_) = &*impl_ {
            impl_.devices(type_filter)
        } else {
            eyre::bail!("HAL is not selected!");
        }
    }

    pub fn camera(&self, id: &str) -> eyre::Result<Arc<dyn Camera + Send + Sync>> {
        let impl_ = self.impl_.read().unwrap();
        if let Some(impl_) = &*impl_ {
            impl_.camera(id)
        } else {
            eyre::bail!("HAL is not selected!");
        }
    }

    pub fn focuser(&self, id: &str) -> eyre::Result<Arc<dyn Focuser + Send + Sync>> {
        let impl_ = self.impl_.read().unwrap();
        if let Some(impl_) = &*impl_ {
            impl_.focuser(id)
        } else {
            eyre::bail!("HAL is not selected!");
        }
    }
}

pub trait HalImpl {
    fn state(&self) -> eyre::Result<HalState>;
    fn devices(&self, type_filter: DeviceType) -> eyre::Result<Vec<DeviceInfo>>;
    fn camera(&self, id: &str) -> eyre::Result<Arc<dyn Camera + Send + Sync>>;
    fn focuser(&self, id: &str) -> eyre::Result<Arc<dyn Focuser + Send + Sync>>;
}

///////////////////////////////////////////////////////////////////////////////
// Device

pub trait Device {
    fn id(&self) -> &str;
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

pub trait Camera : Device {
    fn init_before_shot(&self) -> eyre::Result<()>;

    // Exposure
    fn exposure_range(&self) -> eyre::Result<RangeInclusive<f64>>;
    fn start_exposure(&self, value: f64) -> eyre::Result<()>;
    fn abort_exposure(&self) -> eyre::Result<()>;

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
}

///////////////////////////////////////////////////////////////////////////////
// Focuser

pub trait Focuser : Device {
    fn abs_position_range(&self) -> eyre::Result<RangeInclusive<f64>>;
    fn abs_position(&self) -> eyre::Result<f64>;
    fn set_abs_position(&self, value: f64) -> eyre::Result<()>;
    fn temperature(&self) -> eyre::Result<f64>;
}
