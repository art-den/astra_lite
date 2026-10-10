//! Everything that touches COM directly.
//!
//! `unsafe` is confined to this module tree by design: the device modules
//! (`telescope`, `focuser`, `camera`) are safe wrappers over it.

pub mod apartment;
pub mod collections;
pub mod dispatch;
pub mod registry;
pub mod safearray;
pub mod variant;

/// In-process COM mocks, so that the collection and dispatch plumbing can be tested
/// without a driver installed. The `mock` feature exposes them (and the driver
/// injection seams built on them) to tests outside this crate.
#[cfg(any(test, feature = "mock"))]
pub mod mock;

pub use apartment::ComGuard;
pub use dispatch::Dispatch;
pub use variant::{Variant, VariantKind};

use crate::error::AscomError;
use windows::Win32::System::Com::{GetErrorInfo, IErrorInfo};

/// Encodes a Rust `&str` as the NUL-terminated UTF-16 the Win32 W APIs expect.
pub(crate) fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Reads the thread's `IErrorInfo`, which a failing driver populates.
///
/// Returns `(source, description)`; either may be empty. `IErrorInfo` has no
/// `GetClass` in `windows` 0.62, so the exception class name is not available.
pub(crate) fn error_info_text() -> (String, String) {
    let mut out = (String::new(), String::new());
    if let Ok(info) = error_info() {
        let (source, description) = (&mut out.0, &mut out.1);
        unsafe {
            if let Ok(s) = info.GetSource() {
                *source = s.to_string();
            }
            if let Ok(s) = info.GetDescription() {
                *description = s.to_string();
            }
        }
    }
    out
}

/// Note that `GetErrorInfo` also *clears* the thread's error info, which is what
/// we want: a stale description must not leak into the next failure.
fn error_info() -> windows::core::Result<IErrorInfo> {
    unsafe { GetErrorInfo(0) }
}

/// Classifies an HRESULT and attaches whatever the thread's `IErrorInfo` says.
pub(crate) fn fail(hresult: i32, member: impl Into<String>) -> AscomError {
    let mut err = AscomError::from_hresult(hresult, member);
    let (source, message) = error_info_text();
    if !source.is_empty() {
        err.source = source;
    }
    if !message.is_empty() {
        err.message = message;
    }
    err
}
