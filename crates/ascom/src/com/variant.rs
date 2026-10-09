//! Our own `VARIANT` wrapper.
//!
//! In `windows` 0.62 `VARIANT` is a `#[repr(C)]` struct around nested unions
//! (`VARIANT_0` → `VARIANT_0_0` → `VARIANT_0_0_0`) with no ASCOM conversions, so the
//! whole ASCOM type-marshalling table lives here. It is *not* free of trait impls:
//! `windows` adds `Drop` (`VariantClear`) and `Clone` (`VariantCopy`) to it.
//!
//! Ownership rule: whoever holds a `Variant` owns its payload (BSTR, SAFEARRAY,
//! interface pointer) and releases it in `Drop` via `VariantClear`. Arguments we
//! build are therefore ours to free, and a result returned by a driver becomes ours
//! the moment we wrap it.

use std::time::{SystemTime, UNIX_EPOCH};

use windows::Win32::Foundation::{SysAllocStringLen, VARIANT_FALSE, VARIANT_TRUE};
use windows::core::{IUnknown, Interface};
use windows::Win32::System::Variant::{
    VariantClear, VARENUM, VARIANT, VARIANT_0_0, VARIANT_0_0_0, VT_ARRAY, VT_BOOL, VT_BSTR,
    VT_DATE, VT_DISPATCH, VT_EMPTY, VT_ERROR, VT_I1, VT_I2, VT_I4, VT_I8, VT_NULL, VT_R4, VT_R8,
    VT_UNKNOWN, VT_UI1, VT_UI2, VT_UI4, VT_UI8,
};

use crate::com::{fail, safearray};
use crate::error::{AscomError, Result};

/// Days between the OLE Automation epoch (midnight 1899-12-30) and the Unix epoch.
const OA_DAYS_TO_UNIX_DAYS: f64 = 25_569.0;
const SECONDS_PER_DAY: f64 = 86_400.0;

/// A value-classification of a `VARIANT`, used where the concrete type is only
/// known at runtime (`StateValue.Value`, `SupportedActions` members, probing).
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum VariantKind {
    Empty,
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    /// Days since 1899-12-30, as carried by `VT_DATE`.
    Date(f64),
    Str(String),
    /// An OLE error code (`VT_ERROR`).
    Error(i32),
    /// A nested COM object (`VT_DISPATCH` / `VT_UNKNOWN`).
    Object,
    /// A SAFEARRAY of any element type.
    Array,
    /// Anything else, with the raw `VARENUM` preserved.
    Other(i32),
}

/// Implemented by the ASCOM enumeration mirrors so they can be read from a
/// `VARIANT` uniformly ("enum arrives as an integer").
pub trait AscomEnum: Copy {
    /// `None` for a value the specification does not define.
    fn from_raw(raw: i32) -> Option<Self>;
    fn to_raw(self) -> i32;
}

/// An owned COM `VARIANT`.
pub struct Variant(VARIANT);

impl Drop for Variant {
    fn drop(&mut self) {
        unsafe {
            // Not a double clear: `VariantClear` leaves the VARIANT as `VT_EMPTY`, so the
            // `Drop` impl `windows` adds to `VARIANT` (which also calls `VariantClear`)
            // has nothing left to free. Clearing here keeps our ownership rule independent
            // of that glue.
            let _ = VariantClear(&mut self.0);
        }
    }
}

/// `VARIANT` is a one-field `repr(C)` struct over `VARIANT_0`, a `repr(C)` union
/// whose first member is the `vt`/payload record, so the record shares the address
/// of the `VARIANT`. Reaching it by address avoids dereferencing through
/// `ManuallyDrop`, which the field chain would force us to do.
fn head(v: &VARIANT) -> &VARIANT_0_0 {
    unsafe { &*core::ptr::addr_of!(*v).cast::<VARIANT_0_0>() }
}

impl Variant {
    fn new(vt: VARENUM, payload: VARIANT_0_0_0) -> Self {
        let mut v = VARIANT::default();
        unsafe {
            let h = &mut *(core::ptr::addr_of_mut!(v).cast::<VARIANT_0_0>());
            h.vt = vt;
            core::ptr::write(core::ptr::addr_of_mut!(h.Anonymous), payload);
        }
        Variant(v)
    }

    fn payload(&self) -> VARIANT_0_0_0 {
        unsafe { core::ptr::read(core::ptr::addr_of!(head(&self.0).Anonymous)) }
    }

    pub fn from_bool(b: bool) -> Self {
        let mut u = VARIANT_0_0_0::default();
        u.boolVal = if b { VARIANT_TRUE } else { VARIANT_FALSE };
        Self::new(VT_BOOL, u)
    }

    pub fn from_i32(i: i32) -> Self {
        let mut u = VARIANT_0_0_0::default();
        u.lVal = i;
        Self::new(VT_I4, u)
    }

    pub fn from_f64(x: f64) -> Self {
        let mut u = VARIANT_0_0_0::default();
        u.dblVal = x;
        Self::new(VT_R8, u)
    }

    /// `VT_BSTR`.
    ///
    /// An empty `&str` still becomes a *valid, zero-length* BSTR rather than a NULL
    /// one: `windows` maps `""` to `BSTR::new()` (a null pointer), and .NET interop
    /// marshals a NULL `VT_BSTR` as a C# `null` — which makes managed drivers throw
    /// `ArgumentNullException` instead of seeing `""`. Verified live against the
    /// OmniSim COM proxy: `Action(name, "")` failed with
    /// `Value cannot be null. (Parameter 'value')` until this was fixed.
    pub fn from_str(s: &str) -> Self {
        let bstr = if s.is_empty() {
            // Allocates the same empty BSTR a managed caller would send.
            unsafe { SysAllocStringLen(None) }
        } else {
            windows::core::BSTR::from(s)
        };
        let mut u = VARIANT_0_0_0::default();
        u.bstrVal = core::mem::ManuallyDrop::new(bstr);
        Self::new(VT_BSTR, u)
    }

    /// `VT_DATE` from a UTC instant (ASCOM timestamps are UTC).
    pub fn from_date(time: SystemTime) -> Self {
        let mut u = VARIANT_0_0_0::default();
        u.date = system_time_to_date(time);
        Self::new(VT_DATE, u)
    }

    pub fn from_enum<E: AscomEnum>(e: E) -> Self {
        Self::from_i32(e.to_raw())
    }

    pub fn vt(&self) -> VARENUM {
        head(&self.0).vt
    }

    /// `VT_EMPTY` is what a driver returns for "nothing here"; for a property read
    /// that is a protocol violation, not a value.
    pub fn is_empty(&self) -> bool {
        self.vt() == VT_EMPTY
    }

    pub fn is_array(&self) -> bool {
        self.vt().0 & VT_ARRAY.0 != 0
    }

    pub fn is_object(&self) -> bool {
        let vt = self.vt();
        vt == VT_DISPATCH || vt == VT_UNKNOWN
    }

    // Scalars ---------------------------------------------------------------

    /// Numeric value for any scalar VARIANT type ASCOM drivers return.
    ///
    /// Widening is deliberate: the spec allows `float` to arrive as `VT_R4` from
    /// old drivers and `integer` as `VT_I2`.
    pub fn as_f64(&self) -> Result<f64> {
        let out = unsafe {
            let u = self.payload();
            match self.vt() {
                VT_R4 => f64::from(u.fltVal),
                VT_R8 | VT_DATE => u.dblVal,
                VT_I1 => f64::from(u.cVal),
                VT_I2 => f64::from(u.iVal),
                VT_I4 => f64::from(u.lVal),
                VT_I8 => u.llVal as f64,
                VT_UI1 => f64::from(u.bVal),
                VT_UI2 => f64::from(u.uiVal),
                VT_UI4 => f64::from(u.ulVal),
                VT_UI8 => u.ullVal as f64,
                VT_BOOL => f64::from(u.boolVal.0),
                other => return Err(type_mismatch("float", other)),
            }
        };
        Ok(out)
    }

    /// Exact integer payload of an integer `VARENUM`; `None` for any other type.
    /// Reading through `f64` would drop the bits above 2^53 of `VT_I8`/`VT_UI8`.
    fn exact_int(&self) -> Option<i64> {
        let out = unsafe {
            let u = self.payload();
            match self.vt() {
                VT_I1 => i64::from(u.cVal),
                VT_I2 => i64::from(u.iVal),
                VT_I4 => i64::from(u.lVal),
                VT_I8 => u.llVal,
                VT_UI1 => i64::from(u.bVal),
                VT_UI2 => i64::from(u.uiVal),
                VT_UI4 => i64::from(u.ulVal),
                // i64 is the widest value we can hand out, so a larger u64 saturates.
                VT_UI8 => i64::try_from(u.ullVal).unwrap_or(i64::MAX),
                _ => return None,
            }
        };
        Some(out)
    }

    /// Integer value for any scalar VARIANT a driver returns for an `integer` member.
    ///
    /// Reads the exact payload field instead of going through `f64`, whose cast
    /// saturates: NaN would become `Ok(0)` and `VT_UI4 3_000_000_000` would become
    /// `i32::MAX`. A driver may answer an integer member with a float, so integral
    /// `VT_R4/VT_R8` still convert; a fractional value is refused, not truncated, and
    /// `VT_DATE` stays out of the integer path rather than reading as a day serial.
    pub fn as_i32(&self) -> Result<i32> {
        let vt = self.vt();
        if let Some(v) = self.exact_int() {
            return narrow_i32(v);
        }
        if vt == VT_BOOL {
            // A flag member is still an integer; VARIANT_TRUE is -1, not 1.
            return Ok(unsafe { self.payload().boolVal }.0 as i32);
        }
        if vt == VT_R4 || vt == VT_R8 {
            return float_as_i32(self.as_f64()?);
        }
        Err(type_mismatch("int", vt))
    }

    /// `Short` in the spec (`InterfaceVersion`), which really does arrive as `VT_I2`.
    pub fn as_i16(&self) -> Result<i16> {
        let v = self.as_f64()?;
        if v != v.trunc() || v < f64::from(i16::MIN) || v > f64::from(i16::MAX) {
            return Err(AscomError::type_mismatch(
                "i16",
                format!("value {v} does not fit in 16 bits"),
            ));
        }
        Ok(v as i16)
    }

    pub fn as_bool(&self) -> Result<bool> {
        let vt = self.vt();
        if vt == VT_BOOL {
            // VARIANT_TRUE is -1, not 1, so only 0 means false.
            return Ok(unsafe { self.payload().boolVal } != VARIANT_FALSE);
        }
        // Some drivers expose boolean members as integer flags.
        if vt == VT_I1 || vt == VT_I2 || vt == VT_I4 {
            return Ok(self.as_i32()? != 0);
        }
        Err(type_mismatch("bool", vt))
    }

    pub fn as_str(&self) -> Result<String> {
        let vt = self.vt();
        match vt {
            VT_BSTR => {
                let u = self.payload();
                Ok(unsafe { &*u.bstrVal }.to_string())
            }
            VT_I4 => Ok(self.as_i32()?.to_string()),
            VT_R8 => Ok(self.as_f64()?.to_string()),
            other => Err(type_mismatch("string", other)),
        }
    }

    /// Reads an `enum` member through the [`AscomEnum`] mirror.
    pub fn as_enum<E: AscomEnum>(&self, member: &str) -> Result<E> {
        let raw = self.as_i32()?;
        E::from_raw(raw).ok_or_else(|| {
            AscomError::type_mismatch(member, format!("unknown enum value {raw}"))
        })
    }

    // Dates -----------------------------------------------------------------

    /// `VT_DATE` (OLE automation date) as a UTC instant.
    ///
    /// A bare `f64` is not a time: reading `UTCDate` without this conversion is how
    /// a wrapper ends up silently off by 25569 days.
    pub fn as_date(&self) -> Result<SystemTime> {
        if self.vt() != VT_DATE {
            return Err(type_mismatch("date", self.vt()));
        }
        let serial = unsafe { self.payload().date };
        date_to_system_time(serial).ok_or_else(|| {
            AscomError::type_mismatch("VT_DATE", format!("out of range OLE date {serial}"))
        })
    }

    /// The raw OLE automation serial, for callers that only need to compare or log.
    pub fn as_date_serial(&self) -> Result<f64> {
        if self.vt() != VT_DATE {
            return Err(type_mismatch("date", self.vt()));
        }
        Ok(unsafe { self.payload().date })
    }

    // Structured ------------------------------------------------------------

    /// `VT_ARRAY | VT_BSTR` — `Gains`, `Offsets`, `ReadoutModes`, `SupportedActions`
    /// when the driver exposes a real array instead of an ArrayList.
    pub fn as_string_array(&self) -> Result<Vec<String>> {
        safearray::read_strings(self)
    }

    /// `VT_ARRAY | VT_I2/I4` — `TrackingRates` when it arrives as an array.
    pub fn as_i32_array(&self) -> Result<Vec<i32>> {
        safearray::read_ints(self)
    }

    /// The nested COM object behind `VT_DISPATCH` / `VT_UNKNOWN`.
    pub(crate) fn as_unknown(&self) -> Result<IUnknown> {
        let vt = self.vt();
        let u = self.payload();
        if vt == VT_DISPATCH {
            let disp = unsafe { &*u.pdispVal };
            return disp
                .as_ref()
                .ok_or_else(|| AscomError::type_mismatch("VT_DISPATCH", "null pointer"))?
                .cast()
                .map_err(|e| fail(e.code().0, "QueryInterface(IUnknown)"));
        }
        if vt == VT_UNKNOWN {
            let unk = unsafe { &*u.punkVal };
            return unk
                .as_ref()
                .ok_or_else(|| AscomError::type_mismatch("VT_UNKNOWN", "null pointer"))?
                .cast()
                .map_err(|e| fail(e.code().0, "QueryInterface(IUnknown)"));
        }
        Err(type_mismatch("dispatch or unknown", vt))
    }

    pub(crate) fn array_ptr(&self) -> Result<*mut core::ffi::c_void> {
        if !self.is_array() {
            return Err(type_mismatch("safe array", self.vt()));
        }
        let p = unsafe { self.payload().parray };
        if p.is_null() {
            return Err(AscomError::type_mismatch("safe array", "null pointer"));
        }
        Ok(p.cast())
    }

    /// Runtime classification without a type expectation.
    pub fn kind(&self) -> VariantKind {
        let vt = self.vt();
        match vt {
            VT_EMPTY => VariantKind::Empty,
            VT_NULL => VariantKind::Null,
            VT_BOOL => VariantKind::Bool(self.as_bool().unwrap_or(false)),
            VT_BSTR => VariantKind::Str(self.as_str().unwrap_or_default()),
            VT_DATE => VariantKind::Date(unsafe { self.payload().date }),
            VT_ERROR => VariantKind::Error(unsafe { self.payload().scode }),
            VT_DISPATCH | VT_UNKNOWN => VariantKind::Object,
            v if v.0 & VT_ARRAY.0 != 0 => VariantKind::Array,
            v if v == VT_I1 || v == VT_I2 || v == VT_I4 || v == VT_I8 || v == VT_UI1
                || v == VT_UI2 || v == VT_UI4 || v == VT_UI8 =>
            {
                // Exact payload; the arm guard means `exact_int` is always Some.
                VariantKind::Int(self.exact_int().unwrap_or(0))
            }
            v if v == VT_R4 || v == VT_R8 => VariantKind::Float(self.as_f64().unwrap_or(0.0)),
            other => VariantKind::Other(other.0 as i32),
        }
    }

    /// Human-readable rendering used for property discovery.
    pub fn describe(&self) -> String {
        match self.kind() {
            VariantKind::Empty => "empty".to_string(),
            VariantKind::Null => "null".to_string(),
            VariantKind::Bool(b) => format!("{b}"),
            VariantKind::Int(i) => format!("{i}"),
            VariantKind::Float(x) => format!("{x}"),
            VariantKind::Str(s) => s,
            VariantKind::Date(serial) => format!("{} ({serial})", format_date_utc(serial)),
            VariantKind::Error(c) => format!("scode 0x{:08X}", c as u32),
            VariantKind::Object => "<object>".to_string(),
            VariantKind::Array => "<array>".to_string(),
            VariantKind::Other(v) => format!("<unhandled vt {v}>"),
        }
    }

    pub(crate) fn raw(&self) -> &VARIANT {
        &self.0
    }

    /// Takes ownership of a raw `VARIANT` the caller just obtained (an `Invoke`
    /// result, a SAFEARRAY element copy).
    ///
    /// # Safety
    /// `v` must be a VARIANT the caller owns exactly one reference to; it is cleared
    /// in `Drop`.
    pub(crate) unsafe fn from_raw(v: VARIANT) -> Self {
        Self(v)
    }

    /// Wraps a freshly created SAFEARRAY. Used by tests and by nothing on the hot
    /// path: real arrays arrive from a driver inside a VARIANT.
    ///
    /// # Safety
    /// `psa` must be a SAFEARRAY created by `SafeArrayCreate` and not owned elsewhere;
    /// the returned `Variant` destroys it.
    #[cfg(test)]
    pub(crate) unsafe fn from_owned_array(psa: *mut windows::Win32::System::Com::SAFEARRAY, element: VARENUM) -> Self {
        let mut u = VARIANT_0_0_0::default();
        u.parray = psa;
        Self::new(VARENUM(VT_ARRAY.0 | element.0), u)
    }

    /// `VT_UNKNOWN` around a COM object, as the inverse of [`Variant::as_unknown`].
    ///
    /// Test-only: it lets an in-process mock travel through exactly the same
    /// marshalling path as a real driver's reply.
    #[cfg(test)]
    pub(crate) fn from_unknown(unknown: &IUnknown) -> Self {
        let mut u = VARIANT_0_0_0::default();
        // `clone` takes the reference that `VariantClear` in `Drop` will release;
        // `ManuallyDrop` keeps the wrapper itself from releasing it early.
        u.punkVal = core::mem::ManuallyDrop::new(Some(unknown.clone()));
        Self::new(VT_UNKNOWN, u)
    }
}

fn type_mismatch(wanted: &str, vt: VARENUM) -> AscomError {
    AscomError::vartype(wanted, vt.0)
}

/// Narrows an exact integer payload; out of range is an error, never a saturation.
pub(crate) fn narrow_i32(v: i64) -> Result<i32> {
    if v < i64::from(i32::MIN) || v > i64::from(i32::MAX) {
        return Err(AscomError::type_mismatch(
            "i32",
            format!("value {v} does not fit in 32 bits"),
        ));
    }
    Ok(v as i32)
}

/// An `integer` member that arrived as a float: integral values convert, the rest are
/// refused, because `as i32` would truncate 6.5 and saturate 1e20 onto `i32::MAX`.
pub(crate) fn float_as_i32(x: f64) -> Result<i32> {
    if !x.is_finite() || x != x.trunc() {
        return Err(AscomError::type_mismatch(
            "i32",
            format!("value {x} is not an integer"),
        ));
    }
    // Both bounds are exactly representable, so the cast below is exact.
    if x < f64::from(i32::MIN) || x > f64::from(i32::MAX) {
        return Err(AscomError::type_mismatch(
            "i32",
            format!("value {x} does not fit in 32 bits"),
        ));
    }
    Ok(x as i32)
}

/// `VT_EMPTY` without taking ownership, used to decide whether an `Invoke` result
/// carries a value at all.
pub(crate) fn is_raw_empty(v: &VARIANT) -> bool {
    head(v).vt == VT_EMPTY
}

/// Reads `dblVal` from a VARIANT we do not own. Test-only: it exists to assert the
/// `rgvarg` argument order without constructing a second owner for the same payload.
#[cfg(test)]
pub(crate) fn peek_f64(v: &VARIANT) -> Option<f64> {
    if head(v).vt != VT_R8 {
        return None;
    }
    let payload = unsafe { core::ptr::read(core::ptr::addr_of!(head(v).Anonymous)) };
    Some(unsafe { payload.dblVal })
}

/// Reads `lVal` from a VARIANT we do not own, used by the COM mocks that receive an
/// index argument through `DISPPARAMS`.
#[cfg(test)]
pub(crate) fn peek_i32(v: &VARIANT) -> Option<i32> {
    if head(v).vt != VT_I4 {
        return None;
    }
    let payload = unsafe { core::ptr::read(core::ptr::addr_of!(head(v).Anonymous)) };
    Some(unsafe { payload.lVal })
}

// ---------------------------------------------------------------------------
// OLE automation date conversion
// ---------------------------------------------------------------------------

/// OLE serial → UTC instant. Fractions of a day are the time of day.
pub fn date_to_system_time(serial: f64) -> Option<SystemTime> {
    if !serial.is_finite() {
        return None;
    }
    let unix_seconds = serial * SECONDS_PER_DAY - OA_DAYS_TO_UNIX_DAYS * SECONDS_PER_DAY;
    if unix_seconds < 0.0 {
        // ASCOM timestamps are never earlier than 1970; below that we would have to
        // invent a sign convention for `Duration`, so treat it as out of range.
        return None;
    }
    UNIX_EPOCH.checked_add(std::time::Duration::try_from_secs_f64(unix_seconds).ok()?)
}

/// UTC instant → OLE serial.
///
/// A pre-1970 instant is a legal clock value, and `duration_since` reports it as an
/// error; treating that as zero would silently write 1970-01-01 into the driver.
pub fn system_time_to_date(time: SystemTime) -> f64 {
    let secs = match time.duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_secs_f64(),
        // The error carries the magnitude of the negative offset.
        Err(e) => -e.duration().as_secs_f64(),
    };
    secs / SECONDS_PER_DAY + OA_DAYS_TO_UNIX_DAYS
}

/// Largest OLE serial [`format_date_utc`] renders. The serial is driver data, and a
/// saturating `i64` cast of an extreme value would overflow the epoch shifts inside
/// `civil_from_days`; this bound is trillions of years, far outside any date a driver
/// can mean.
const MAX_FORMATTABLE_SERIAL: f64 = 1e15;

/// Formats an OLE serial as `YYYY-MM-DDTHH:MM:SSZ` without pulling in a date crate.
pub fn format_date_utc(serial: f64) -> String {
    if !serial.is_finite() || serial.abs() > MAX_FORMATTABLE_SERIAL {
        return "<invalid date>".to_string();
    }
    let days = serial.floor();
    let mut rest_seconds = ((serial - days) * SECONDS_PER_DAY).round() as i64;
    // Inside the bound above the cast is exact, never a saturation onto i64::MAX.
    let mut days = days as i64;
    if rest_seconds >= SECONDS_PER_DAY as i64 {
        rest_seconds -= SECONDS_PER_DAY as i64;
        days += 1;
    }
    // Shift from the OA epoch (1899-12-30) to the Unix epoch before decomposing.
    let unix_days = days - OA_DAYS_TO_UNIX_DAYS as i64;
    let (year, month, day) = civil_from_days(unix_days);
    let (hour, minute, second) = (
        rest_seconds / 3600,
        (rest_seconds % 3600) / 60,
        rest_seconds % 60,
    );
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

/// Howard Hinnant's `civil_from_days`: days since 1970-01-01 → (y, m, d).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn variant_clear_leaves_empty_so_the_drop_glue_is_a_no_op() {
        // The reason `Drop for Variant` may call `VariantClear` explicitly even though
        // `windows` clears every `VARIANT` when it goes out of scope.
        let mut raw = VARIANT::from("payload");
        assert!(!raw.is_empty());
        unsafe {
            let _ = VariantClear(&mut raw);
        }
        assert_eq!(raw.vt(), VT_EMPTY);
        assert!(raw.is_empty());
    }

    #[test]
    fn bool_roundtrip_uses_variant_true() {
        let t = Variant::from_bool(true);
        let f = Variant::from_bool(false);
        assert_eq!(t.vt(), VT_BOOL);
        assert!(t.as_bool().unwrap());
        assert!(!f.as_bool().unwrap());
        // VT_TRUE must be -1, not 1, or drivers will read the flag wrongly.
        assert_eq!(unsafe { t.payload().boolVal }, VARIANT_TRUE);
        assert_eq!(t.as_f64().unwrap(), -1.0);
    }

    #[test]
    fn numeric_widening_matches_what_drivers_return() {
        // An integer member may arrive as a float, but only an integral one: 6.5 is
        // a driver bug, not a value we may truncate to 6.
        assert!(Variant::from_f64(6.5).as_i32().is_err());
        assert_eq!(Variant::from_f64(6.0).as_i32().unwrap(), 6);
        assert_eq!(Variant::from_i32(7).as_f64().unwrap(), 7.0);
        assert_eq!(Variant::from_i32(4).as_i16().unwrap(), 4);
        // A driver returning VT_R4 (single precision) must still read back.
        let mut u = VARIANT_0_0_0::default();
        u.fltVal = 1.5f32;
        assert_eq!(Variant::new(VT_R4, u).as_f64().unwrap(), 1.5);
        // ... and so must a 16-bit integer.
        let mut u = VARIANT_0_0_0::default();
        u.iVal = -4i16;
        assert_eq!(Variant::new(VT_I2, u).as_i32().unwrap(), -4);
    }

    #[test]
    fn i16_range_is_enforced() {
        let mut u = VARIANT_0_0_0::default();
        u.lVal = 70_000;
        assert!(Variant::new(VT_I4, u).as_i16().is_err());
    }

    #[test]
    fn i32_refuses_junk_instead_of_saturating() {
        // A saturating f64 cast would answer Ok(0) and Ok(i32::MAX) for these.
        assert!(Variant::from_f64(f64::NAN).as_i32().is_err());
        assert!(Variant::from_f64(1e20).as_i32().is_err());
        assert!(Variant::from_f64(-1e20).as_i32().is_err());
        let mut u = VARIANT_0_0_0::default();
        u.ulVal = 3_000_000_000;
        assert!(Variant::new(VT_UI4, u).as_i32().is_err());
        // Whole numbers of either float width still read back.
        let mut u = VARIANT_0_0_0::default();
        u.fltVal = 7.0f32;
        assert_eq!(Variant::new(VT_R4, u).as_i32().unwrap(), 7);
        assert_eq!(Variant::from_f64(f64::from(i32::MAX)).as_i32().unwrap(), i32::MAX);
        assert_eq!(Variant::from_f64(f64::from(i32::MIN)).as_i32().unwrap(), i32::MIN);
        // A date is not an integer: it must not read back as a bare day serial.
        assert!(Variant::from_date(UNIX_EPOCH).as_i32().is_err());
    }

    #[test]
    fn wide_integers_keep_their_exact_payload() {
        let mut u = VARIANT_0_0_0::default();
        // 2^53 + 1 is the first integer an f64 cannot represent.
        u.llVal = 9_007_199_254_740_993;
        let v = Variant::new(VT_I8, u);
        assert_eq!(v.kind(), VariantKind::Int(9_007_199_254_740_993));
        assert!(v.as_i32().is_err());

        let mut u = VARIANT_0_0_0::default();
        u.ullVal = u64::MAX;
        let v = Variant::new(VT_UI8, u);
        assert_eq!(v.kind(), VariantKind::Int(i64::MAX), "saturates, never wraps");
        assert!(v.as_i32().is_err());
    }

    #[test]
    fn string_roundtrip_and_type_mismatch() {
        let v = Variant::from_str("ASCOM.OmniSim.Telescope");
        assert_eq!(v.as_str().unwrap(), "ASCOM.OmniSim.Telescope");
        assert_eq!(v.describe(), "ASCOM.OmniSim.Telescope");
        let e = v.as_bool().unwrap_err();
        assert_eq!(e.kind, crate::error::AscomErrorKind::Com);
    }

    #[test]
    fn bool_accepts_integer_flags() {
        assert!(Variant::from_i32(1).as_bool().unwrap());
        assert!(!Variant::from_i32(0).as_bool().unwrap());
    }

    #[test]
    fn empty_is_a_value_of_its_own() {
        let v = Variant(VARIANT::default());
        assert!(v.is_empty());
        assert_eq!(v.kind(), VariantKind::Empty);
        assert_eq!(v.describe(), "empty");
    }

    #[test]
    fn ole_date_matches_the_documented_epoch() {
        // 1970-01-01T00:00:00Z is OLE serial 25569 by definition of the 1899-12-30 epoch.
        assert_eq!(system_time_to_date(UNIX_EPOCH), OA_DAYS_TO_UNIX_DAYS);
        assert_eq!(date_to_system_time(OA_DAYS_TO_UNIX_DAYS), Some(UNIX_EPOCH));
        // 2026-02-18T07:31:05Z, cross-checked against Python's datetime arithmetic.
        let serial = 46_071.313_252_314_816_f64;
        assert_eq!(format_date_utc(serial), "2026-02-18T07:31:05Z");
        assert_eq!(format_date_utc(40_000.5), "2009-07-06T12:00:00Z");
        assert_eq!(format_date_utc(25_569.0), "1970-01-01T00:00:00Z");
    }

    #[test]
    fn date_roundtrips_through_variant() {
        let serial = 46_071.313_252_314_816_f64;
        let time = date_to_system_time(serial).unwrap();
        let v = Variant::from_date(time);
        assert_eq!(v.vt(), VT_DATE);
        assert!((v.as_date_serial().unwrap() - serial).abs() < 1e-6);
        assert_eq!(v.as_date().unwrap(), time);
        // A date must not be silently readable as a plain float of days.
        assert!(Variant::from_f64(serial).as_date().is_err());
    }

    #[test]
    fn date_conversion_rejects_junk() {
        assert!(date_to_system_time(f64::NAN).is_none());
        assert!(date_to_system_time(-1.0).is_none());
    }

    #[test]
    fn pre_1970_instant_keeps_its_sign() {
        // Reading the error as zero used to hand the driver 1970-01-01 instead.
        let one_day_early = UNIX_EPOCH - std::time::Duration::from_secs(SECONDS_PER_DAY as u64);
        assert_eq!(system_time_to_date(one_day_early), OA_DAYS_TO_UNIX_DAYS - 1.0);
        assert_eq!(system_time_to_date(UNIX_EPOCH), OA_DAYS_TO_UNIX_DAYS);
        assert_eq!(
            format_date_utc(system_time_to_date(one_day_early)),
            "1969-12-31T00:00:00Z"
        );
    }

    #[test]
    fn extreme_serials_are_invalid_not_a_panic() {
        // `as i64` saturates onto the i64 edges, where the epoch shift overflows.
        assert_eq!(format_date_utc(1e300), "<invalid date>");
        assert_eq!(format_date_utc(-1e300), "<invalid date>");
        assert_eq!(format_date_utc(f64::MAX), "<invalid date>");
        // Still exact at the bound and far below it, including pre-1970 dates.
        assert_ne!(format_date_utc(MAX_FORMATTABLE_SERIAL), "<invalid date>");
        assert_eq!(format_date_utc(-69_399.0), "1709-12-27T00:00:00Z");
        // The reachable path: a driver's VT_DATE goes through describe().
        let mut u = VARIANT_0_0_0::default();
        u.date = 1e300;
        assert!(Variant::new(VT_DATE, u)
            .describe()
            .starts_with("<invalid date>"));
    }

    #[test]
    fn civil_from_days_covers_leap_years_and_centuries() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(-1), (1969, 12, 31));
        assert_eq!(civil_from_days(59), (1970, 3, 1));
        assert_eq!(civil_from_days(60), (1970, 3, 2));
        // 2000-02-29 exists, 1900-02-29 does not: 2000-03-01 is day 11017.
        assert_eq!(civil_from_days(11_017), (2000, 3, 1));
        assert_eq!(civil_from_days(11_016), (2000, 2, 29));
        assert_eq!(civil_from_days(-25_567), (1900, 1, 1));
    }

    #[test]
    fn date_is_described_readably() {
        let v = Variant::from_date(date_to_system_time(25_569.0).unwrap());
        assert_eq!(v.describe(), "1970-01-01T00:00:00Z (25569)");
    }

    #[test]
    fn empty_string_is_a_real_bstr_not_null() {
        // A NULL VT_BSTR arrives in managed drivers as C# null, which makes them
        // throw ArgumentNullException; the pointer must be allocated either way.
        fn bstr_ptr(v: &Variant) -> *const u16 {
            let payload = v.payload();
            unsafe { core::mem::transmute_copy(&payload.bstrVal) }
        }

        let empty = Variant::from_str("");
        assert_eq!(empty.vt(), VT_BSTR);
        assert!(!bstr_ptr(&empty).is_null(), "empty BSTR must not be NULL");
        assert_eq!(empty.as_str().unwrap(), "");

        let set = Variant::from_str("SlewToHA");
        assert!(!bstr_ptr(&set).is_null());
        assert_eq!(set.as_str().unwrap(), "SlewToHA");
    }

    #[test]
    fn array_and_object_flags_do_not_confuse_scalars() {
        assert!(!Variant::from_i32(1).is_array());
        assert!(!Variant::from_i32(1).is_object());
        assert_eq!(Variant::from_i32(1).kind(), VariantKind::Int(1));
        assert_eq!(Variant::from_f64(0.5).kind(), VariantKind::Float(0.5));
    }
}
