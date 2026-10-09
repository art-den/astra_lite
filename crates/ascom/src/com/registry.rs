//! The slice of the Win32 registry that driver discovery needs.
//!
//! ASCOM defines no COM call for "which drivers are installed?" — the Chooser object
//! exposes only `DeviceType` and `Choose` (verified over `IDispatch` against platform
//! 7.1), so the registration keys a driver writes when it installs are the only
//! machine-readable list. This module reads them; [`crate::drivers`] turns them into a
//! typed list.
//!
//! Handles are owned (`Owned<HKEY>` closes them), so no path through this module can
//! leak a registry handle.

use windows::core::{Owned, PCWSTR, PWSTR};
use windows::Win32::Foundation::{
    ERROR_FILE_NOT_FOUND, ERROR_NO_MORE_ITEMS, ERROR_PATH_NOT_FOUND, ERROR_SUCCESS, WIN32_ERROR,
};
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ, KEY_WOW64_32KEY, KEY_WOW64_64KEY,
    REG_EXPAND_SZ, REG_SAM_FLAGS, REG_SZ, RegEnumKeyExW, RegOpenKeyExW, RegQueryValueExW,
};

use crate::com::{fail, wide};
use crate::error::{AscomError, AscomErrorKind, Result};

/// Registry hive holding the ASCOM keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hive {
    /// `HKEY_LOCAL_MACHINE` — a normal all-users ASCOM install.
    Machine,
    /// `HKEY_CURRENT_USER` — per-user registrations.
    User,
}

impl Hive {
    fn as_raw(self) -> HKEY {
        match self {
            Self::Machine => HKEY_LOCAL_MACHINE,
            Self::User => HKEY_CURRENT_USER,
        }
    }
}

/// Which registry view to read.
///
/// A 64-bit process reads the 64-bit view by default, but most ASCOM drivers are
/// 32-bit .NET servers whose keys live under `WOW6432Node`, so a caller that wants the
/// complete list has to read both views and merge them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    SixtyFour,
    ThirtyTwo,
}

impl View {
    fn sam_flags(self) -> REG_SAM_FLAGS {
        match self {
            Self::SixtyFour => KEY_WOW64_64KEY,
            Self::ThirtyTwo => KEY_WOW64_32KEY,
        }
    }
}

/// Longest sub-key name Windows allows, not counting the terminating NUL.
const MAX_KEY_NAME: usize = 255;

/// One sub-key name, or `None` once the enumeration is exhausted.
///
/// `RegEnumKeyExW` takes the input buffer size in characters *including* the
/// terminating NUL and reports the outgoing length without it. Verified live: with
/// an input size of 255 a legal 255-character name answers `ERROR_MORE_DATA`.
fn enum_key_name(key: HKEY, index: u32, buffer: &mut [u16]) -> Result<Option<String>> {
    let mut chars = buffer.len() as u32;
    let hr = unsafe {
        RegEnumKeyExW(
            key,
            index,
            Some(PWSTR(buffer.as_mut_ptr())),
            &mut chars,
            None,
            None,
            None,
            None,
        )
    };
    if hr == ERROR_NO_MORE_ITEMS {
        return Ok(None);
    }
    win32_ok(hr, "RegEnumKeyExW")?;
    Ok(Some(String::from_utf16_lossy(&buffer[..chars as usize])))
}

/// `HRESULT_FROM_WIN32`: folds a positive Win32 code into its `0x8007_xxxx` form.
///
/// A raw `LSTATUS` must never reach `AscomErrorKind::from_hresult`: only the
/// folded form matches the RPC constants it special-cases (`RPC_S_SERVER_UNAVAILABLE`
/// 1722 is `Disconnected` as `0x8007_06BA`, raw `1722` is just a number), and
/// `Display` would print a non-HRESULT as if it were one.
fn win32_hresult(code: WIN32_ERROR) -> i32 {
    if code.0 & 0x8000_0000 != 0 {
        // Already an HRESULT; pass it through, like the C macro does.
        code.0 as i32
    } else {
        (0x8007_0000 | (code.0 & 0xFFFF)) as i32
    }
}

/// A failed Win32 registry call as an [`AscomError`].
///
/// Wrapping fixes `Display` and maps known RPC codes (1722 becomes `Disconnected`).
/// The local ASCOM-class downgrade that used to live here is gone: `from_hresult`
/// now consults the ASCOM table only behind the `0x8004` prefix, so a Win32 code
/// like 1058 (`0x8007_0422`) reaches us as `Com` on its own.
fn win32_fail(code: WIN32_ERROR, member: &str) -> AscomError {
    fail(win32_hresult(code), member)
}

fn win32_ok(hr: WIN32_ERROR, member: &str) -> Result<()> {
    if hr == ERROR_SUCCESS {
        Ok(())
    } else {
        Err(win32_fail(hr, member))
    }
}

fn open(hive: Hive, path: &str, view: View) -> Result<Option<Owned<HKEY>>> {
    let name = wide(path);
    let mut key = HKEY::default();
    let hr =
        unsafe { RegOpenKeyExW(hive.as_raw(), PCWSTR(name.as_ptr()), None, KEY_READ | view.sam_flags(), &mut key) };
    if hr == ERROR_SUCCESS {
        // `RegOpenKeyExW` handed us a handle, which `Owned` now closes.
        return Ok(Some(unsafe { Owned::new(key) }));
    }
    if hr == ERROR_FILE_NOT_FOUND || hr == ERROR_PATH_NOT_FOUND {
        // "No ASCOM on this machine" and "no key for that device family" are normal.
        return Ok(None);
    }
    Err(win32_fail(hr, "RegOpenKeyExW"))
}

/// Names of the immediate sub-keys of `path`, in registry order.
///
/// An absent `path` yields an empty list rather than an error.
pub fn sub_key_names(hive: Hive, path: &str, view: View) -> Result<Vec<String>> {
    let Some(key) = open(hive, path, view)? else {
        return Ok(Vec::new());
    };
    let mut names = Vec::new();
    // One wider than the longest legal name, for the NUL the input size counts.
    let mut buffer = vec![0u16; MAX_KEY_NAME + 1];
    while let Some(name) = enum_key_name(*key, names.len() as u32, &mut buffer)? {
        names.push(name);
    }
    Ok(names)
}

/// The unnamed (default) string value of `path`, if it has a non-empty one.
///
/// `REG_EXPAND_SZ` is returned unexpanded: an ASCOM description is a display name,
/// not a path, so inventing an expansion for it would rewrite the registration. A
/// value of any other type is an error, never a silently missing description.
pub fn default_string(hive: Hive, path: &str, view: View) -> Result<Option<String>> {
    let Some(key) = open(hive, path, view)? else {
        return Ok(None);
    };
    let name = wide("");
    let mut size = 0u32;
    let mut kind = REG_SZ;
    let hr = unsafe {
        RegQueryValueExW(*key, PCWSTR(name.as_ptr()), None, Some(&mut kind), None, Some(&mut size))
    };
    if hr == ERROR_FILE_NOT_FOUND {
        return Ok(None);
    }
    win32_ok(hr, "RegQueryValueExW")?;
    if kind != REG_SZ && kind != REG_EXPAND_SZ {
        // The value exists but is not a string: a broken registration, which must
        // not collapse into "registered without a description".
        return Err(AscomError::local(
            AscomErrorKind::Com,
            "default_string",
            format!("unexpected registry value type {} at {path}", kind.0),
        ));
    }
    // `size` counts bytes, including the terminating NUL.
    let mut buffer = vec![0u16; (size as usize / 2) + 1];
    let mut read = size;
    let hr = unsafe {
        RegQueryValueExW(
            *key,
            PCWSTR(name.as_ptr()),
            None,
            None,
            Some(buffer.as_mut_ptr() as *mut u8),
            Some(&mut read),
        )
    };
    win32_ok(hr, "RegQueryValueExW")?;
    let units = ((read as usize).min(buffer.len() * 2)) / 2;
    let text = String::from_utf16_lossy(&buffer[..units]);
    Ok(Some(text.trim_end_matches('\0').trim().to_string()).filter(|s| !s.is_empty()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_key_that_cannot_exist_is_reported_as_absent() {
        // Reading a path no installer could have created must not be an error: that is
        // how callers tell "nothing registered" apart from "registry unreadable".
        let names =
            sub_key_names(Hive::Machine, r"SOFTWARE\ASCOM\No Such Family Drivers Xyz", View::SixtyFour)
                .expect("an absent key is not a failure");
        assert!(names.is_empty());
    }

    #[test]
    fn a_known_present_key_enumerates() {
        // Every Windows install has HKLM\SOFTWARE\Microsoft\Windows, so this proves the
        // FFI plumbing (buffer sizes, NUL handling) without depending on ASCOM.
        let names =
            sub_key_names(Hive::Machine, r"SOFTWARE\Microsoft\Windows", View::SixtyFour).expect("readable");
        assert!(
            names.iter().any(|n| n.eq_ignore_ascii_case("CurrentVersion")),
            "expected CurrentVersion among {} sub-keys",
            names.len()
        );
    }

    #[test]
    fn a_win32_code_never_impersonates_an_ascom_driver_code() {
        // Raw Win32 1722 (RPC_S_SERVER_UNAVAILABLE) is 0x6BA and 1058
        // (ERROR_SERVICE_DISABLED) is 0x422: both fall inside the ASCOM Driver range
        // of `from_ascom_code`. The registry wrapper must still map them to
        // binding-layer classes; 1722 is rescued by the RPC special case, 1058 by
        // the `0x8004` facility gate in `from_hresult`.
        assert_eq!(win32_hresult(WIN32_ERROR(1722)), 0x8007_06BA_u32 as i32);
        for (code, kind) in [
            (1722u32, AscomErrorKind::Disconnected),
            (1058, AscomErrorKind::Com),
            (5, AscomErrorKind::Com),
            (234, AscomErrorKind::Com),
        ] {
            let e = win32_fail(WIN32_ERROR(code), "RegEnumKeyExW");
            assert_eq!(e.kind, kind, "Win32 code {code}");
            assert!(!e.is_transient(), "Win32 code {code}");
            assert!(!e.to_string().contains("hr 0x0000"), "{e}");
        }
        assert!(win32_fail(WIN32_ERROR(1722), "RegEnumKeyExW").is_disconnected());
        // A value that already looks like an HRESULT passes through unchanged.
        assert_eq!(win32_hresult(WIN32_ERROR(0x8007_0005)), 0x8007_0005_u32 as i32);
    }

    #[test]
    fn the_two_views_are_distinct_requests() {
        assert_ne!(View::SixtyFour.sam_flags(), View::ThirtyTwo.sam_flags());
    }
}
