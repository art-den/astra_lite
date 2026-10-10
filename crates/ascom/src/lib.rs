//! Production-oriented Rust wrapper over **classic ASCOM/COM** (not Alpaca).
//!
//! Four device families are covered: [`camera::Camera`] (`ICameraV4`),
//! [`telescope::Telescope`] (`ITelescopeV4`), [`focuser::Focuser`] (`IFocuserV4`)
//! and [`filterwheel::FilterWheel`] (`IFilterWheelV3`).
//! The module layout makes adding another family one more file: a `device.rs`
//! member table plus the interface-specific methods.
//!
//! # How it talks to a driver
//!
//! ASCOM drivers guarantee only *late binding*, so every member access goes through
//! `IDispatch::GetIDsOfNames` + `IDispatch::Invoke`; there is no typelib and no early
//! binding. See [`com`] for that layer, which is the only place `unsafe` lives.
//!
//! # Threading
//!
//! One device owns one dedicated STA thread; a handle is a `Send + Clone` sender of
//! closures to it ([`actor`]). An `IDispatch` is never handed between threads, and
//! the thread pumps the message queue so a driver's modal setup dialog stays alive.
//!
//! # Errors
//!
//! Any ASCOM member may raise, so every fallible call returns
//! [`Result<T>`](error::Result). "This driver does not implement that
//! member" is a normal outcome, reported as [`AscomErrorKind::Unsupported`] and
//! testable with [`AscomError::is_unsupported`]; "no value" is an error, never a
//! default.
//!
//! # Finding a driver to talk to
//!
//! [`chooser::choose`] shows the platform's own modal picker; [`drivers::installed_drivers`]
//! returns the same registrations programmatically by reading the registry, which is the
//! only machine-readable list ASCOM offers.
//!
//! # Async
//!
//! An async layer is planned as a separate `tokio` feature, but it is **not
//! implemented yet**: the feature currently gates no code, and its dependency is
//! not vendored.
//!
//! # Example
//!
//! ```no_run
//! use ascom::device::{AscomDevice, DeviceSpec};
//! use ascom::focuser::Focuser;
//! use ascom::wait::WaitSpec;
//! use std::time::Duration;
//!
//! # fn main() -> ascom::error::Result<()> {
//! let focuser = Focuser::open(&DeviceSpec::new("ASCOM.OmniSim.Focuser"))?;
//! focuser.set_connected(true)?;
//! let target = focuser.position()? + 50;
//! focuser.move_to_and_wait(target, WaitSpec::new(Duration::from_secs(30)))?;
//! focuser.set_connected(false)?;
//! # Ok(())
//! # }
//! ```

pub mod actor;
pub mod camera;
pub mod chooser;
pub mod com;
pub mod device;
pub mod drivers;
pub mod error;
pub mod filterwheel;
pub mod focuser;
pub mod image;
pub mod telescope;
pub mod wait;

/// The types an application normally needs, in one import.
pub mod prelude {
    pub use crate::camera::Camera;
    pub use crate::chooser::{DeviceType, choose};
    pub use crate::device::{AscomDevice, CapabilitySnapshot, DeviceSpec, StateValue};
    pub use crate::drivers::{DriverInfo, installed_drivers, installed_prog_ids};
    pub use crate::error::{AscomError, AscomErrorKind, Result};
    pub use crate::filterwheel::FilterWheel;
    pub use crate::focuser::Focuser;
    pub use crate::image::Image;
    pub use crate::telescope::Telescope;
    pub use crate::wait::WaitSpec;
}

/// Re-exported so that callers can name [`AscomEnum`] for their own enum mirrors
/// without naming a path inside the [`com`] module.
pub use com::variant::{AscomEnum, VariantKind};
pub use error::{AscomError, AscomErrorKind};

/// The vocabulary of the mock-driver seams (`Camera::open_mock` and friends): the mock
/// module itself stays crate-internal, so this is the only public door to it.
#[cfg(feature = "mock")]
pub mod mock {
    pub use crate::com::mock::{Element, Member};
    // Named here so a caller can build a `Member::Refuses` answer without depending
    // on the `windows` crate itself.
    pub use windows::core::HRESULT;

    // A member table is carried into the actor's `Send` closure, so a variant holding a
    // non-`Send` payload must fail here rather than deep inside `Actor::start`.
    const _: fn() = || {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Member>();
    };
}
