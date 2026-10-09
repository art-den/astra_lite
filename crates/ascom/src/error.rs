//! Error model mirroring the ASCOM exception classes.
//!
//! Every ASCOM member may raise, so nothing in this crate panics on driver input:
//! "no value" is an error, never a default.

use windows::Win32::Foundation::{
    CO_E_CLASSSTRING, CO_E_OBJNOTCONNECTED, DISP_E_MEMBERNOTFOUND, DISP_E_TYPEMISMATCH,
    DISP_E_UNKNOWNNAME, E_NOTIMPL, REGDB_E_CLASSNOTREG, RPC_E_CALL_CANCELED, RPC_E_CALL_REJECTED,
    RPC_E_DISCONNECTED, RPC_E_SERVERCALL_REJECTED, RPC_E_SERVERCALL_RETRYLATER,
};

/// ASCOM exception classes, identified by the low 16 bits of the HRESULT
/// (ASCOM ORs its 16-bit codes with `0x8004_0000`).
///
/// The numeric code of each variant is documented on the variant; `Unsupported`
/// covers both `MethodNotImplementedException` and `PropertyNotImplementedException`,
/// which share code `0x400` for historical reasons.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum AscomErrorKind {
    /// 0x400 — Method/PropertyNotImplementedException.
    Unsupported,
    /// 0x401 — InvalidValueException.
    InvalidValue,
    /// 0x402 — ValueNotSetException.
    ValueNotSet,
    /// 0x407 — NotConnectedException.
    NotConnected,
    /// 0x408 — ParkedException.
    Parked,
    /// 0x409 — SlavedException.
    Slaved,
    /// 0x40B — InvalidOperationException.
    InvalidOperation,
    /// 0x40C — ActionNotImplementedException.
    ActionNotImplemented,
    /// 0x40E — OperationCancelledException (also `RPC_E_CALL_CANCELED`).
    Cancelled,
    /// 0x500..=0xFFF — DriverException, plus any other code inside the ASCOM range
    /// (drivers invent codes; see [`AscomErrorKind::from_ascom_code`]).
    Driver,
    /// Not an ASCOM error: COM or OS failure, including our own type-conversion
    /// failures (`DISP_E_TYPEMISMATCH`).
    Com,
    /// The driver process is gone or the object was released.
    Disconnected,
    /// The callee is busy; the call may be retried (idempotent operations only).
    Transient,
    /// No such ProgID / driver not installed.
    NotFound,
    /// Waiting for a completion property exceeded [`crate::wait::WaitSpec::timeout`].
    Timeout,
}

impl AscomErrorKind {
    /// Maps the low 16 bits of an HRESULT to an ASCOM exception class.
    ///
    /// Deliberately does not know about non-ASCOM codes: call it only when the code
    /// is known to come from the ASCOM range (a driver's `EXCEPINFO.scode`);
    /// everything else goes through [`AscomErrorKind::from_hresult`], which checks
    /// the facility prefix first.
    pub fn from_ascom_code(code: u16) -> Self {
        match code {
            0x400 => Self::Unsupported,
            0x401 => Self::InvalidValue,
            0x402 => Self::ValueNotSet,
            0x407 => Self::NotConnected,
            0x408 => Self::Parked,
            0x409 => Self::Slaved,
            0x40B => Self::InvalidOperation,
            0x40C => Self::ActionNotImplemented,
            0x40E => Self::Cancelled,
            0x500..=0xFFF => Self::Driver,
            // Any other code in the ASCOM range is still the driver talking, not the
            // binding layer. Verified live: a legacy filter wheel simulator reports an
            // out-of-range `Position` as `0x80040404`, a code the specification does not
            // define. Reporting that as `Com` would blame this wrapper for a driver's
            // non-standard answer and send a test chasing its own tail.
            0x400..=0x4FF => Self::Driver,
            _ => Self::Com,
        }
    }

    /// Full mapping: non-ASCOM COM/OS codes first, then the ASCOM table — but only
    /// for HRESULTs carrying the `0x8004_0000` prefix ASCOM ORs its codes with, so
    /// an OS failure with ASCOM-looking low bits stays a binding-layer error.
    ///
    /// All constants come from the `windows` crate rather than magic numbers.
    pub fn from_hresult(hresult: i32) -> Self {
        // A ProgID we cannot resolve at all.
        if hresult == CO_E_CLASSSTRING.0 || hresult == REGDB_E_CLASSNOTREG.0 {
            return Self::NotFound;
        }
        // The server is alive but too busy to take the call right now.
        if hresult == RPC_E_CALL_REJECTED.0
            || hresult == RPC_E_SERVERCALL_RETRYLATER.0
            || hresult == RPC_E_SERVERCALL_REJECTED.0
        {
            return Self::Transient;
        }
        // The server died, or the object was released under us.
        if hresult == RPC_E_DISCONNECTED.0
            || hresult == CO_E_OBJNOTCONNECTED.0
            || hresult == RPC_S_SERVER_UNAVAILABLE_HRESULT
        {
            return Self::Disconnected;
        }
        // Consequence of CoCancelCall.
        if hresult == RPC_E_CALL_CANCELED.0 {
            return Self::Cancelled;
        }
        // A plain COM object refuses a member it never implemented with E_NOTIMPL;
        // under late binding that is the same statement about the driver as
        // MethodNotImplementedException, and the crate treats both as a normal
        // answer (`try_get`, `probe`, the capability snapshot).
        if hresult == E_NOTIMPL.0 {
            return Self::Unsupported;
        }
        // The object has no member by that name. Under late binding this is how "this
        // driver does not implement the member" looks when the member is absent from
        // the interface rather than explicitly refused: verified live against a
        // camera reporting `InterfaceVersion 3`, whose `UTCDate` (a V4 member) fails
        // `GetIDsOfNames` with `DISP_E_UNKNOWNNAME` instead of raising
        // MethodNotImplementedException. Reporting it as `Com` would blame the
        // wrapper for a driver's missing member.
        if hresult == DISP_E_UNKNOWNNAME.0 || hresult == DISP_E_MEMBERNOTFOUND.0 {
            return Self::Unsupported;
        }
        // The prefix is part of the ASCOM contract, and trusting the low bits alone
        // misreads everything else: a Win32 failure folded by `HRESULT_FROM_WIN32`
        // (1058 becomes `0x8007_0422`) would claim a driver exception class purely
        // because its number happens to land in `0x400..=0xFFF`.
        let hr = hresult as u32;
        if hr & 0xFFFF_0000 != 0x8004_0000 {
            return Self::Com;
        }
        Self::from_ascom_code((hr & 0xFFFF) as u16)
    }

    /// One of the exception classes the ASCOM specification defines, as opposed to
    /// the binding-layer classes this crate adds. Only these carry a numeric ASCOM
    /// code, and only behind the `0x8004_0000` facility prefix.
    pub(crate) fn is_ascom_class(self) -> bool {
        matches!(
            self,
            Self::Unsupported
                | Self::InvalidValue
                | Self::ValueNotSet
                | Self::NotConnected
                | Self::Parked
                | Self::Slaved
                | Self::InvalidOperation
                | Self::ActionNotImplemented
                | Self::Cancelled
                | Self::Driver
        )
    }
}

/// `RPC_S_SERVER_UNAVAILABLE` as it reaches us when the local server vanished.
///
/// The crate exposes `RPC_S_SERVER_UNAVAILABLE` as `RPC_STATUS(1722)`, not as an
/// HRESULT, and there is no constant for `HRESULT_FROM_WIN32(1722)` = `0x800706BA`,
/// hence the local definition.
const RPC_S_SERVER_UNAVAILABLE_HRESULT: i32 = 0x8007_06BA_u32 as i32;

/// A single failure raised by a driver, by COM, or by this crate.
///
/// `Display` and `Error` are implemented by hand rather than derived with
/// `thiserror`: thiserror treats *any* field named `source` as the error chain
/// source and requires it to implement `Error`, while the ASCOM error model needs
/// `source` to be the `IErrorInfo::GetSource` string. The shape of this struct is
/// fixed by the specification, so the derive cannot be used.
#[derive(Debug, Clone)]
pub struct AscomError {
    pub kind: AscomErrorKind,
    /// ASCOM numeric error code, or 0 when the failure is not an ASCOM exception:
    /// the low 16 bits of any other HRESULT are a Win32 or COM number, not a code.
    pub code: u16,
    pub hresult: i32,
    /// Property or method the failure is attributed to.
    pub member: String,
    /// From `IErrorInfo::GetDescription`, else `EXCEPINFO.bstrDescription`.
    pub message: String,
    /// From `IErrorInfo::GetSource`.
    pub source: String,
}

impl std::fmt::Display for AscomError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {:?}", self.member, self.kind)?;
        // A crate-generated failure has no HRESULT; printing a zero would name
        // `S_OK` as the failure code.
        if self.hresult != 0 {
            write!(
                f,
                " (ascom code 0x{:03X}, hr 0x{:08X})",
                self.code,
                self.hresult as u32
            )?;
        }
        write!(
            f,
            " source=[{}] message=[{}]",
            if self.source.is_empty() { "-" } else { &self.source },
            if self.message.is_empty() { "-" } else { &self.message },
        )
    }
}

impl std::error::Error for AscomError {}

impl AscomError {
    /// Builds an error from an HRESULT, classifying it via [`AscomErrorKind::from_hresult`].
    ///
    /// Message and source stay empty; the COM layer fills them from `IErrorInfo`
    /// and `EXCEPINFO` where those are available.
    pub fn from_hresult(hresult: i32, member: impl Into<String>) -> Self {
        let kind = AscomErrorKind::from_hresult(hresult);
        // The numeric code exists only for an ASCOM failure; `Cancelled` in
        // particular also arrives from `RPC_E_CALL_CANCELED`, which carries none.
        let code = if kind.is_ascom_class() && (hresult as u32) & 0xFFFF_0000 == 0x8004_0000 {
            (hresult & 0xFFFF) as u16
        } else {
            0
        };
        Self {
            kind,
            code,
            hresult,
            member: member.into(),
            message: String::new(),
            source: String::new(),
        }
    }

    /// A failure produced by this crate rather than by a driver (no HRESULT involved).
    pub fn local(kind: AscomErrorKind, member: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            kind,
            code: 0,
            hresult: 0,
            member: member.into(),
            message: message.into(),
            source: "ascom".to_string(),
        }
    }

    /// Our own type-conversion failure. Mirrors what a .NET client would see as
    /// `InvalidCastException`, reported under the same code the drivers use for it.
    pub fn type_mismatch(member: impl Into<String>, detail: impl Into<String>) -> Self {
        Self::from_hresult(DISP_E_TYPEMISMATCH.0, member).with_message(detail)
    }

    /// A type-conversion failure that names the `VARENUM` actually received.
    pub fn vartype(member: impl Into<String>, vt: impl std::fmt::Display) -> Self {
        Self::type_mismatch(member, format!("got VARENUM({vt})"))
    }

    pub fn timeout(member: impl Into<String>, detail: impl Into<String>) -> Self {
        Self::local(AscomErrorKind::Timeout, member, detail)
    }

    pub fn with_message(mut self, message: impl Into<String>) -> Self {
        self.message = message.into();
        self
    }

    pub fn with_source(mut self, source: impl Into<String>) -> Self {
        self.source = source.into();
        self
    }

    /// The member is not implemented by this driver. Not a fault: a driver is
    /// allowed to leave any member unimplemented.
    pub fn is_unsupported(&self) -> bool {
        matches!(
            self.kind,
            AscomErrorKind::Unsupported | AscomErrorKind::ActionNotImplemented
        )
    }

    /// Safe to retry, but only for idempotent operations (property reads).
    pub fn is_transient(&self) -> bool {
        self.kind == AscomErrorKind::Transient
    }

    /// The device is gone; polling a completion property should stop immediately.
    pub fn is_disconnected(&self) -> bool {
        matches!(
            self.kind,
            AscomErrorKind::Disconnected | AscomErrorKind::NotFound
        )
    }

    pub fn is_timeout(&self) -> bool {
        self.kind == AscomErrorKind::Timeout
    }

    pub fn is_cancelled(&self) -> bool {
        self.kind == AscomErrorKind::Cancelled
    }
}

/// Convenience alias used throughout the crate.
pub type Result<T> = std::result::Result<T, AscomError>;

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::Foundation::E_NOTIMPL;

    fn hr(code: u16) -> i32 {
        0x8004_0000_u32 as i32 | i32::from(code)
    }

    #[test]
    fn every_ascom_code_in_the_spec_table_maps() {
        let table = [
            (0x400u16, AscomErrorKind::Unsupported),
            (0x401, AscomErrorKind::InvalidValue),
            (0x402, AscomErrorKind::ValueNotSet),
            (0x407, AscomErrorKind::NotConnected),
            (0x408, AscomErrorKind::Parked),
            (0x409, AscomErrorKind::Slaved),
            (0x40B, AscomErrorKind::InvalidOperation),
            (0x40C, AscomErrorKind::ActionNotImplemented),
            (0x40E, AscomErrorKind::Cancelled),
            (0x500, AscomErrorKind::Driver),
            (0xFFF, AscomErrorKind::Driver),
        ];
        for (code, kind) in table {
            assert_eq!(AscomErrorKind::from_ascom_code(code), kind, "code 0x{code:03X}");
            assert_eq!(AscomErrorKind::from_hresult(hr(code)), kind, "hr 0x{:08X}", hr(code) as u32);
        }
    }

    #[test]
    fn codes_outside_the_ascom_ranges_are_not_ascom() {
        assert_eq!(AscomErrorKind::from_ascom_code(0x000), AscomErrorKind::Com);
        assert_eq!(AscomErrorKind::from_ascom_code(0x100), AscomErrorKind::Com);
        // A plain COM facility code must not be mistaken for a driver error.
        assert_eq!(AscomErrorKind::from_hresult(DISP_E_TYPEMISMATCH.0), AscomErrorKind::Com);
    }

    /// An undefined code inside the ASCOM range is still the driver speaking: a legacy
    /// filter wheel simulator answers an out-of-range `Position` with `0x80040404`.
    #[test]
    fn an_undefined_ascom_code_is_a_driver_error_not_a_binding_failure() {
        for code in [0x403u16, 0x404, 0x405, 0x40A, 0x40D, 0x4FF] {
            assert_eq!(
                AscomErrorKind::from_ascom_code(code),
                AscomErrorKind::Driver,
                "code 0x{code:03X}"
            );
        }
        assert_eq!(AscomErrorKind::from_hresult(hr(0x404)), AscomErrorKind::Driver);
    }

    #[test]
    fn not_found_covers_both_activation_failures() {
        // CLSIDFromProgID and CoCreateInstance report different codes for the same situation.
        assert_eq!(AscomErrorKind::from_hresult(CO_E_CLASSSTRING.0), AscomErrorKind::NotFound);
        assert_eq!(AscomErrorKind::from_hresult(REGDB_E_CLASSNOTREG.0), AscomErrorKind::NotFound);
        assert!(AscomError::from_hresult(REGDB_E_CLASSNOTREG.0, "x").is_disconnected());
    }

    #[test]
    fn transient_rpc_codes_are_distinguished() {
        for c in [RPC_E_CALL_REJECTED, RPC_E_SERVERCALL_RETRYLATER, RPC_E_SERVERCALL_REJECTED] {
            let e = AscomError::from_hresult(c.0, "get");
            assert_eq!(e.kind, AscomErrorKind::Transient, "hr 0x{:08X}", c.0 as u32);
            assert!(e.is_transient());
        }
    }

    #[test]
    fn disconnected_and_cancelled_are_not_confused() {
        assert_eq!(AscomErrorKind::from_hresult(RPC_E_DISCONNECTED.0), AscomErrorKind::Disconnected);
        assert_eq!(AscomErrorKind::from_hresult(CO_E_OBJNOTCONNECTED.0), AscomErrorKind::Disconnected);
        assert_eq!(AscomErrorKind::from_hresult(RPC_E_CALL_CANCELED.0), AscomErrorKind::Cancelled);
        assert_eq!(
            AscomErrorKind::from_hresult(RPC_S_SERVER_UNAVAILABLE_HRESULT),
            AscomErrorKind::Disconnected
        );
        let cancelled = AscomError::from_hresult(RPC_E_CALL_CANCELED.0, "x");
        assert!(!cancelled.is_disconnected());
        assert!(cancelled.is_cancelled());
    }

    #[test]
    fn helper_predicates() {
        assert!(AscomError::from_hresult(hr(0x400), "Rates").is_unsupported());
        assert!(AscomError::from_hresult(hr(0x40C), "Action").is_unsupported());
        assert!(!AscomError::from_hresult(hr(0x40B), "Move").is_unsupported());
        assert!(AscomError::timeout("Slewing", "no").is_timeout());
        assert!(!AscomError::timeout("Slewing", "no").is_disconnected());
    }

    #[test]
    fn a_member_the_object_does_not_expose_is_unsupported() {
        // Live case: a V3 driver has no `UTCDate`, so late binding cannot resolve
        // the name. That is the driver not implementing the member, not our bug.
        for code in [DISP_E_UNKNOWNNAME, DISP_E_MEMBERNOTFOUND] {
            let error = AscomError::from_hresult(code.0, "UTCDate");
            assert_eq!(error.kind, AscomErrorKind::Unsupported, "hr 0x{:08X}", code.0 as u32);
            assert!(error.is_unsupported());
        }
        // A genuine conversion failure still stays a COM-level error.
        assert_eq!(AscomErrorKind::from_hresult(DISP_E_TYPEMISMATCH.0), AscomErrorKind::Com);
    }

    /// Win32 codes 1024..=4095 folded by `HRESULT_FROM_WIN32` look like ASCOM codes
    /// in their low 16 bits. Measured before the facility gate existed:
    /// `0x80070400` read as `Unsupported`, `0x80070407` as `NotConnected`,
    /// `0x80070422` (`ERROR_SERVICE_DISABLED`) as `Driver`. An OS failure must not
    /// claim a domain class.
    #[test]
    fn os_failures_with_ascom_looking_low_bits_are_not_ascom_classes() {
        for hresult in [0x8007_0400_u32 as i32, 0x8007_0407_u32 as i32, 0x8007_0422_u32 as i32] {
            let e = AscomError::from_hresult(hresult, "RegEnumKeyExW");
            assert_eq!(e.kind, AscomErrorKind::Com, "hr 0x{:08X}", hresult as u32);
            assert!(!e.is_unsupported(), "hr 0x{:08X}", hresult as u32);
            assert_eq!(e.code, 0, "fabricated ascom code for hr 0x{:08X}", hresult as u32);
        }
        // A real ASCOM prefix still reaches the table, undefined driver codes
        // included.
        assert_eq!(AscomErrorKind::from_hresult(hr(0x400)), AscomErrorKind::Unsupported);
        assert_eq!(AscomErrorKind::from_hresult(hr(0x404)), AscomErrorKind::Driver);
    }

    /// A plain COM object says "this member is not implemented" with `E_NOTIMPL`;
    /// under late binding that is the same statement about the driver as
    /// ASCOM's `MethodNotImplementedException`.
    #[test]
    fn e_notimpl_says_the_member_is_not_implemented() {
        let e = AscomError::from_hresult(E_NOTIMPL.0, "NewEnum");
        assert_eq!(e.kind, AscomErrorKind::Unsupported);
        assert!(e.is_unsupported());
        // Not an ASCOM exception, so there is no ASCOM numeric code to report.
        assert_eq!(e.code, 0);
    }

    /// The field is the ASCOM numeric code, so only failures that carry one fill
    /// it; the low 16 bits of any other HRESULT are a Win32 or COM number.
    #[test]
    fn the_code_field_only_carries_real_ascom_codes() {
        for hresult in [
            REGDB_E_CLASSNOTREG.0,
            DISP_E_TYPEMISMATCH.0,
            RPC_E_CALL_CANCELED.0,
            E_NOTIMPL.0,
        ] {
            let e = AscomError::from_hresult(hresult, "x");
            assert_eq!(
                e.code,
                0,
                "hr 0x{:08X} fabricated ascom code 0x{:03X}",
                hresult as u32,
                e.code
            );
        }
        // ASCOM failures keep their code, including the classes a non-ASCOM code
        // can also produce (`Cancelled` arrives from `RPC_E_CALL_CANCELED` too).
        assert_eq!(AscomError::from_hresult(hr(0x408), "x").code, 0x408);
        assert_eq!(AscomError::from_hresult(hr(0x40E), "x").code, 0x40E);
    }

    /// A crate-generated failure never went through COM, so there is no HRESULT to
    /// print; printing zero would claim `S_OK` as the failure code.
    #[test]
    fn crate_errors_without_an_hresult_do_not_claim_s_ok() {
        let s = AscomError::timeout("Slewing", "gave up").to_string();
        assert!(s.contains("Slewing") && s.contains("gave up"), "{s}");
        assert!(!s.contains("hr 0x00000000"), "{s}");
        assert!(!s.contains("ascom code"), "{s}");
        // A real failure still prints both numbers.
        let s = AscomError::from_hresult(hr(0x407), "Slewing").to_string();
        assert!(s.contains("ascom code 0x407") && s.contains("hr 0x80040407"), "{s}");
    }

    /// A raw .NET exception is not an ASCOM failure, so there is no ASCOM code to
    /// print: the field stays 0 and the HRESULT is what identifies the answer. The
    /// message below and `0x80131500` are what the OmniSim filter wheel really answers
    /// when a second `Position` write lands mid-motion.
    #[test]
    fn a_raw_managed_exception_prints_the_hresult_without_an_ascom_code() {
        let e = AscomError::from_hresult(0x8013_1500_u32 as i32, "Invoke(Position)")
            .with_source("ASCOM.Com")
            .with_message("The FilterWheel is already moving");
        assert_eq!(e.kind, AscomErrorKind::Com);
        assert_eq!(e.code, 0, "the URT facility carries no ASCOM code");
        assert_eq!(
            e.to_string(),
            "Invoke(Position): Com (ascom code 0x000, hr 0x80131500) \
             source=[ASCOM.Com] message=[The FilterWheel is already moving]"
        );
    }

    #[test]
    fn display_carries_member_code_and_hresult() {
        let e = AscomError::from_hresult(hr(0x407), "Slewing").with_message("Not connected");
        let s = e.to_string();
        assert!(s.contains("Slewing"), "{s}");
        assert!(s.contains("0x407"), "{s}");
        assert!(s.contains("0x80040407"), "{s}");
        assert!(s.contains("Not connected"), "{s}");
    }
}
