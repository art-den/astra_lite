//! COM apartment ownership and the message pump.
//!
//! ASCOM .NET local servers are registered as "Apartment", and a driver's
//! `SetupDialog()` or the platform `Chooser` shows a modal dialog that needs a
//! message pump on the calling thread. Hence: one STA thread per device.

use std::marker::PhantomData;

use windows::Win32::System::Com::{CoCancelCall, CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, MSG, PeekMessageW, PM_REMOVE, TranslateMessage,
};

use crate::com::fail;
use crate::error::Result;

/// Keeps the COM library initialised for the current thread.
///
/// Deliberately `!Send`: `CoUninitialize` must run on the thread that called
/// `CoInitializeEx`, so this value must never cross a thread boundary.
pub struct ComGuard {
    not_send: PhantomData<*const ()>,
}

impl ComGuard {
    /// Initialises the current thread as a single-threaded apartment.
    pub fn new() -> Result<Self> {
        let hr = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
        if hr.is_err() {
            return Err(fail(hr.0, "CoInitializeEx"));
        }
        Ok(Self { not_send: PhantomData })
    }

    /// The Win32 thread id of the calling thread, for [`cancel_call`].
    pub fn thread_id() -> u32 {
        unsafe { GetCurrentThreadId() }
    }
}

impl Drop for ComGuard {
    fn drop(&mut self) {
        unsafe { CoUninitialize() };
    }
}

/// Aborts a blocking COM call currently running on `thread_id`.
///
/// ASCOM drivers are allowed to hold a call for seconds (`ImageArray`); the way
/// out of a hang is a watchdog thread calling this, not a timeout per call.
pub fn cancel_call(thread_id: u32) -> Result<()> {
    unsafe { CoCancelCall(thread_id, 0) }.map_err(|e| fail(e.code().0, "CoCancelCall"))
}

/// Runs the STA message queue without blocking.
///
/// Needed because a driver's setup dialog or the platform `Chooser` is modal and
/// lives on our thread: without pumping, the dialog stops answering paint and
/// input messages and appears frozen. A `NULL` window handle makes `PeekMessageW`
/// return both thread messages and messages for windows owned by this thread.
pub fn pump_messages() {
    // Bound the pass so a chatty driver cannot starve the command queue.
    const MAX_MESSAGES_PER_PASS: usize = 64;
    unsafe {
        let mut msg = MSG::default();
        for _ in 0..MAX_MESSAGES_PER_PASS {
            if !PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                break;
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}
