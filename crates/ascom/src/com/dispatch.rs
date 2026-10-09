//! Late-bound `IDispatch` client with a DISPID cache.
//!
//! ASCOM drivers only guarantee late binding, so every member access is
//! `GetIDsOfNames` + `Invoke` with VARIANT arguments.

use std::cell::RefCell;
use std::collections::HashMap;
use std::time::Duration;

use windows::core::{GUID, Interface, PCWSTR};
use windows::Win32::Foundation::{DISP_E_EXCEPTION, SysFreeString};
use windows::Win32::System::Com::{
    CLSIDFromProgID, CLSCTX_ALL, CoCreateInstance, DISPATCH_FLAGS, DISPATCH_METHOD,
    DISPATCH_PROPERTYGET, DISPATCH_PROPERTYPUT, DISPPARAMS, EXCEPINFO, IDispatch,
};
use windows::Win32::System::Ole::DISPID_PROPERTYPUT;
use windows::Win32::System::Variant::{VariantClear, VARIANT};

use crate::com::fail;
use crate::com::variant::{self, Variant};
use crate::com::wide;
use crate::error::{AscomError, AscomErrorKind, Result};

/// Backoff for transient COM rejections: 50, 150, 450 ms, then give up.
const TRANSIENT_RETRY_DELAYS: [Duration; 3] =
    [Duration::from_millis(50), Duration::from_millis(150), Duration::from_millis(450)];

/// What [`Dispatch::probe`] reports for a member the driver does not implement.
///
/// Public so that callers — and the live-driver tests — can tell "not implemented"
/// apart from a real value without hard-coding the text.
pub const NOT_IMPLEMENTED: &str = "<not implemented>";

/// Builds `rgvarg` from arguments given in source order.
///
/// `IDispatch` requires them reversed, so `argv[0]` is the *last* declared argument.
/// Getting this wrong silently swaps `Move(pos)` and `SlewToCoordinates(ra, dec)`.
///
/// The copies are shallow on purpose (the callee must not receive a second owned
/// reference), but `windows` 0.62 gives `VARIANT` a `Drop` impl that runs
/// `VariantClear`, so dropping plain `VARIANT`s here would free payloads the
/// `Variant` arguments still own (double `SysFreeString`/`Release`). `ManuallyDrop`
/// removes exactly that glue and nothing else: it is `#[repr(transparent)]`, so the
/// buffer is still a contiguous array of valid `VARIANT`s for `DISPPARAMS`.
fn build_rgvarg(args: &[&Variant]) -> Vec<core::mem::ManuallyDrop<VARIANT>> {
    let mut argv: Vec<core::mem::ManuallyDrop<VARIANT>> = args
        .iter()
        .map(|v| core::mem::ManuallyDrop::new(unsafe { core::ptr::read(v.raw()) }))
        .collect();
    argv.reverse();
    argv
}

/// Runs `call` under the transient-retry policy: 50/150/450 ms, then give up.
///
/// Only idempotent calls (`idempotent == true`, i.e. property reads) ever retry;
/// retrying `Move`/`Slew`/`StartExposure` could duplicate an accepted command.
fn retrying<T>(idempotent: bool, mut call: impl FnMut() -> Result<T>) -> Result<T> {
    let mut attempt = 0usize;
    loop {
        match call() {
            ok @ Ok(_) => return ok,
            Err(err) => {
                if !idempotent || !err.is_transient() || attempt >= TRANSIENT_RETRY_DELAYS.len() {
                    return Err(err);
                }
                std::thread::sleep(TRANSIENT_RETRY_DELAYS[attempt]);
                attempt += 1;
            }
        }
    }
}

/// A late-bound COM object plus a cache of member DISPIDs.
///
/// Deliberately not `Send`: an `IDispatch` must never be handed between threads,
/// which is why every device handle routes calls through a dedicated COM thread
/// ([`crate::actor`]).
pub struct Dispatch {
    inner: IDispatch,
    ids: RefCell<HashMap<String, i32>>,
}

impl Dispatch {
    /// Instantiates a driver by ProgID, e.g. `ASCOM.OmniSim.Telescope`.
    pub fn from_prog_id(prog_id: &str) -> Result<Self> {
        let name = wide(prog_id);
        unsafe {
            // Both activation failures mean "no such driver", but they come from
            // different calls (CLSIDFromProgID vs CoCreateInstance), so the mapping
            // checks both constants.
            let clsid = CLSIDFromProgID(PCWSTR(name.as_ptr()))
                .map_err(|e| fail(e.code().0, format!("CLSIDFromProgID({prog_id})")))?;
            let obj: IDispatch = CoCreateInstance(&clsid, None, CLSCTX_ALL)
                .map_err(|e| fail(e.code().0, format!("CoCreateInstance({prog_id})")))?;
            Ok(Self::new(obj))
        }
    }

    /// Wraps a nested COM object that arrived inside a VARIANT (`Rate`,
    /// `StateValue`, the `TrackingRates` collection).
    pub fn from_variant(v: &Variant) -> Result<Self> {
        let unknown = v.as_unknown()?;
        let disp: IDispatch = unknown
            .cast()
            .map_err(|e| fail(e.code().0, "QueryInterface(IDispatch)"))?;
        Ok(Self::new(disp))
    }

    fn new(inner: IDispatch) -> Self {
        Self { inner, ids: RefCell::new(HashMap::new()) }
    }

    pub fn inner(&self) -> &IDispatch {
        &self.inner
    }

    /// 0 means the driver exposes no `ITypeInfo` (pure late binding, nothing to introspect).
    pub fn type_info_count(&self) -> Result<u32> {
        unsafe { self.inner.GetTypeInfoCount() }.map_err(|e| fail(e.code().0, "GetTypeInfoCount"))
    }

    fn dispid(&self, member: &str) -> Result<i32> {
        if let Some(&id) = self.ids.borrow().get(member) {
            return Ok(id);
        }
        let name = wide(member);
        let names = [PCWSTR(name.as_ptr())];
        let mut id = 0i32;
        unsafe {
            // lcid 0 = neutral locale; ASCOM member names are locale independent.
            self.inner
                .GetIDsOfNames(&GUID::zeroed(), names.as_ptr(), 1, 0, &mut id)
                .map_err(|e| fail(e.code().0, format!("GetIDsOfNames({member})")))?;
        }
        self.ids.borrow_mut().insert(member.to_string(), id);
        Ok(id)
    }

    fn invoke_once(
        &self,
        member: &str,
        dispid: i32,
        flags: DISPATCH_FLAGS,
        args: &[&Variant],
        named: &mut [i32],
    ) -> Result<Option<Variant>> {
        let mut argv = build_rgvarg(args);
        // Bitwise copies that own nothing (see `build_rgvarg`): the DISPPARAMS array
        // must be contiguous, and ownership of every payload stays with the `Variant`
        // argument, which is the only thing allowed to clear it.
        let params = DISPPARAMS {
            rgvarg: if argv.is_empty() { core::ptr::null_mut() } else { argv.as_mut_ptr().cast::<VARIANT>() },
            rgdispidNamedArgs: if named.is_empty() { core::ptr::null_mut() } else { named.as_mut_ptr() },
            cArgs: argv.len() as u32,
            cNamedArgs: named.len() as u32,
        };
        let mut result = VARIANT::default();
        let mut ex = EXCEPINFO::default();
        let mut arg_err = u32::MAX;
        let hr = unsafe {
            self.inner.Invoke(
                dispid,
                &GUID::zeroed(),
                0,
                flags,
                &params,
                Some(&mut result),
                Some(&mut ex),
                Some(&mut arg_err),
            )
        };

        if let Err(e) = hr {
            // `IDispatch::Invoke` reports a driver-raised exception as the generic
            // `DISP_E_EXCEPTION`; the exception's own code arrives in `EXCEPINFO.scode`.
            // ASCOM encodes its exception class in the low 16 bits of that HRESULT
            // (`0x80040000 + code`, e.g. 0x8004040B = InvalidOperationException), so
            // ignoring `scode` would collapse every `ActionNotImplemented`,
            // `NotConnected` or `Unsupported` into a generic COM error.
            let from_scode = e.code() == DISP_E_EXCEPTION && ex.scode != 0;
            let reported = if from_scode { ex.scode } else { e.code().0 };
            let mut err = fail(reported, format!("Invoke({member})"));
            if from_scode && (reported as u32) & 0xFFFF_0000 == 0 {
                // Some drivers put the bare ASCOM code into `scode`, without the
                // `0x8004` facility. `from_hresult` gates on the prefix and would
                // read that as `Com`; here the source is known to be a driver, so
                // the low bits may consult the ASCOM table directly.
                let code = (reported & 0xFFFF) as u16;
                let kind = AscomErrorKind::from_ascom_code(code);
                if kind.is_ascom_class() {
                    err.kind = kind;
                    err.code = code;
                }
            }
            if let Some(desc) = excepinfo_text(&mut ex) {
                if err.message.is_empty() {
                    err.message = desc.1;
                }
                if err.source.is_empty() {
                    err.source = desc.0;
                }
            }
            if let Some(source_order) = bad_argument_source_order(args.len(), arg_err) {
                err.message = format!("{} (argument #{source_order})", err.message);
            }
            // A failing callee may still have written a value into `pVarResult`; we own
            // it either way. Clearing explicitly keeps ownership independent of the
            // `Drop` glue `windows` adds to `VARIANT` (which a `VT_EMPTY` makes a no-op).
            unsafe {
                let _ = VariantClear(&mut result);
            }
            return Err(err);
        }

        // Property puts and void methods leave the result Empty.
        if variant::is_raw_empty(&result) {
            unsafe {
                let _ = VariantClear(&mut result);
            }
            Ok(None)
        } else {
            Ok(Some(unsafe { Variant::from_raw(result) }))
        }
    }

    /// `Invoke` with the transient-code retry policy.
    ///
    /// Retries happen **only** for idempotent calls (property reads). Retrying
    /// `Move`/`Slew`/`StartExposure` would duplicate a command the driver may
    /// already have accepted, so those get the error straight back.
    fn invoke(
        &self,
        member: &str,
        flags: DISPATCH_FLAGS,
        args: &[&Variant],
        named: &mut [i32],
        idempotent: bool,
    ) -> Result<Option<Variant>> {
        // `GetIDsOfNames` is part of the same logical call and can be rejected
        // transiently too, so the DISPID is resolved inside the retry loop; the cache
        // makes every attempt after the first one free.
        retrying(idempotent, || {
            let dispid = self.dispid(member)?;
            self.invoke_once(member, dispid, flags, args, named)
        })
    }

    /// `Invoke`s a well-known DISPID directly, skipping `GetIDsOfNames`.
    ///
    /// Needed for `NewEnum` (`DISPID_NEWENUM` = -4): collection objects frequently
    /// refuse to expose that hidden member by name. The `METHOD | PROPERTYGET` flag
    /// pair is the documented OLE idiom for retrieving it, not a substitute for a
    /// ordinary property read.
    pub(crate) fn invoke_dispid(
        &self,
        member: &str,
        dispid: i32,
        flags: DISPATCH_FLAGS,
        args: &[&Variant],
        named: &mut [i32],
        idempotent: bool,
    ) -> Result<Option<Variant>> {
        retrying(idempotent, || self.invoke_once(member, dispid, flags, args, named))
    }

    /// Generic member read; the caller owns the returned VARIANT.
    ///
    /// `VT_EMPTY` in reply to a get is a protocol violation by the driver, reported
    /// as an error rather than turned into a default value.
    pub fn get(&self, member: &str) -> Result<Variant> {
        self.invoke(member, DISPATCH_PROPERTYGET, &[], &mut [], true)?.ok_or_else(|| {
            AscomError::local(
                AscomErrorKind::ValueNotSet,
                member,
                format!("{member} returned VT_EMPTY"),
            )
        })
    }

    pub fn put(&self, member: &str, value: &Variant) -> Result<()> {
        self.invoke(member, DISPATCH_PROPERTYPUT, &[value], &mut [DISPID_PROPERTYPUT], false)?;
        Ok(())
    }

    pub fn call(&self, member: &str, args: &[&Variant]) -> Result<Option<Variant>> {
        self.invoke(member, DISPATCH_METHOD, args, &mut [], false)
    }

    pub fn call_void(&self, member: &str, args: &[&Variant]) -> Result<()> {
        self.call(member, args)?;
        Ok(())
    }

    /// Reads an indexed property (`Item(i)` on a .NET collection), then falls back to
    /// a method call, because wrappers disagree about which kind `Item` is.
    ///
    /// Only "this member is not a property here" falls through: any other answer to the
    /// property get is the driver's real reply and must surface unchanged.
    pub(crate) fn get_indexed(&self, member: &str, index: i32) -> Result<Variant> {
        let idx = Variant::from_i32(index);
        let dispid = self.dispid(member)?;
        match self.invoke_dispid(member, dispid, DISPATCH_PROPERTYGET, &[&idx], &mut [], true) {
            Ok(Some(v)) => return Ok(v),
            // `VT_EMPTY` and `DISP_E_MEMBERNOTFOUND` are the two ways a wrapper says
            // "`Item` is a method, not a property"; try the method call below.
            Ok(None) => {}
            Err(err) if err.is_unsupported() => {}
            Err(err) => return Err(err),
        }
        self.invoke_dispid(member, dispid, DISPATCH_METHOD, &[&idx], &mut [], true)?
            .ok_or_else(|| {
                AscomError::local(
                    AscomErrorKind::ValueNotSet,
                    member,
                    format!("{member}({index}) returned VT_EMPTY"),
                )
            })
    }

    /// Reads a member, mapping "the driver does not implement it" to `None`.
    /// `Unsupported` is a normal state of a driver, not a failure.
    pub fn try_get(&self, member: &str) -> Result<Option<Variant>> {
        match self.get(member) {
            Ok(v) => Ok(Some(v)),
            Err(e) if e.is_unsupported() => Ok(None),
            Err(e) => Err(e),
        }
    }

    // Typed conveniences -----------------------------------------------------

    pub fn get_bool(&self, member: &str) -> Result<bool> {
        self.get(member)?.as_bool()
    }

    pub fn get_i16(&self, member: &str) -> Result<i16> {
        self.get(member)?.as_i16()
    }

    pub fn get_i32(&self, member: &str) -> Result<i32> {
        self.get(member)?.as_i32()
    }

    pub fn get_f64(&self, member: &str) -> Result<f64> {
        self.get(member)?.as_f64()
    }

    pub fn get_string(&self, member: &str) -> Result<String> {
        self.get(member)?.as_str()
    }

    pub fn get_string_array(&self, member: &str) -> Result<Vec<String>> {
        self.get(member)?.as_string_array()
    }

    pub fn set_bool(&self, member: &str, value: bool) -> Result<()> {
        self.put(member, &Variant::from_bool(value))
    }

    pub fn set_i32(&self, member: &str, value: i32) -> Result<()> {
        self.put(member, &Variant::from_i32(value))
    }

    pub fn set_f64(&self, member: &str, value: f64) -> Result<()> {
        self.put(member, &Variant::from_f64(value))
    }

    pub fn set_string(&self, member: &str, value: &str) -> Result<()> {
        self.put(member, &Variant::from_str(value))
    }

    /// Reads a member, reporting "not implemented" as a value instead of an error,
    /// which is how one walks an unknown device type generically.
    pub fn probe(&self, member: &str) -> Result<String> {
        match self.get(member) {
            Ok(v) => Ok(v.describe()),
            Err(e) if e.is_unsupported() => Ok(NOT_IMPLEMENTED.to_string()),
            Err(e) => Err(e),
        }
    }
}

/// Reads `EXCEPINFO` text and releases the BSTRs, which `Invoke` made our property.
///
/// Forgetting `SysFreeString` here is the classic small leak on every error path.
/// Returns `(source, description)`.
fn excepinfo_text(ex: &mut EXCEPINFO) -> Option<(String, String)> {
    // Some servers fill the text lazily through this callback; COM expects the
    // caller to invoke it before reading the strings.
    if let Some(deferred) = ex.pfnDeferredFillIn {
        unsafe {
            let _ = deferred(core::ptr::addr_of_mut!(*ex));
        }
    }
    let text = (ex.bstrSource.to_string(), ex.bstrDescription.to_string());
    unsafe {
        SysFreeString(&ex.bstrSource);
        SysFreeString(&ex.bstrDescription);
        SysFreeString(&ex.bstrHelpFile);
        ex.pfnDeferredFillIn = None;
    }
    Some(text)
}

/// Maps the `argerr` index (which counts the *reversed* `rgvarg`) back to the
/// argument position in source order. `u32::MAX` means "the server did not say".
fn bad_argument_source_order(nargs: usize, arg_err: u32) -> Option<usize> {
    if arg_err == u32::MAX || arg_err as usize >= nargs {
        return None;
    }
    Some(nargs - 1 - arg_err as usize)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::com::mock::{
        Element, EXCEPTION_DESCRIPTION, EXCEPTION_SOURCE, ItemBase, MockCollection, MockItem,
        NewEnumMode,
    };
    use std::cell::Cell;
    use windows::core::{HRESULT, IUnknown};
    use windows::Win32::Foundation::E_NOTIMPL;

    /// An ASCOM exception HRESULT, e.g. code `0x407` = NotConnected.
    fn ascom_hr(code: u16) -> HRESULT {
        HRESULT(0x8004_0000_u32 as i32 | i32::from(code))
    }

    #[test]
    fn rgvarg_is_built_in_reverse_declaration_order() {
        let a = Variant::from_f64(6.0);
        let b = Variant::from_f64(1.5);
        let argv = build_rgvarg(&[&a, &b]);
        assert_eq!(argv.len(), 2);
        // SlewToCoordinates(ra, dec) must reach the server as rgvarg[0] = dec.
        assert_eq!(variant::peek_f64(&argv[0]), Some(1.5));
        assert_eq!(variant::peek_f64(&argv[1]), Some(6.0));
        // A non-numeric VARIANT must not be mis-read as a float.
        let plain = Variant::from_i32(3);
        assert_eq!(variant::peek_f64(plain.raw()), None);
    }

    #[test]
    fn argerr_maps_back_to_source_order() {
        // SlewToCoordinates(ra, dec): argerr 0 points at the last declared argument.
        assert_eq!(bad_argument_source_order(2, 0), Some(1));
        assert_eq!(bad_argument_source_order(2, 1), Some(0));
        assert_eq!(bad_argument_source_order(2, u32::MAX), None);
        assert_eq!(bad_argument_source_order(2, 7), None);
        assert_eq!(bad_argument_source_order(0, 0), None);
    }

    #[test]
    fn transient_retry_table_matches_the_spec() {
        assert_eq!(
            TRANSIENT_RETRY_DELAYS,
            [Duration::from_millis(50), Duration::from_millis(150), Duration::from_millis(450)]
        );
    }

    // The following run against an in-process `IDispatch` (see `com::mock`),
    // so the binding layer itself is covered without a driver installed.

    fn mock_dispatch() -> (Variant, Dispatch) {
        let mock = MockCollection::indexed(vec![Element::Empty, Element::Int(42)], ItemBase::Zero);
        let variant = mock.into_variant();
        let handle = Dispatch::from_variant(&variant).expect("the mock is dispatchable");
        (variant, handle)
    }

    /// The mock's strong reference count, read through a balanced AddRef/Release probe.
    /// The returned number includes the temporary probe reference, which is why only
    /// differences between two readings are meaningful.
    fn strong_count(unknown: &IUnknown) -> u32 {
        let raw = unknown.as_raw();
        let count = unsafe { (unknown.vtable().AddRef)(raw) };
        unsafe {
            (unknown.vtable().Release)(raw);
        }
        count
    }

    #[test]
    fn an_interface_argument_survives_the_call_it_is_passed_to() {
        // A `VT_UNKNOWN`/`VT_DISPATCH` argument owns exactly one reference, and `rgvarg`
        // only borrows it. Releasing it twice for that single owned reference makes the
        // object die under the handles that still point at it, so the count before and
        // after the call must be identical.
        let item: IDispatch = MockItem { name: "Rate", value: Cell::new(7) }.into();
        let unknown: IUnknown = item.cast().expect("the mock answers QueryInterface(IUnknown)");
        let (_keep, handle) = mock_dispatch();
        let arg = Variant::from_unknown(&unknown);

        let before = strong_count(&unknown);
        handle.call("Count", &[&arg]).expect("the mock ignores the extra argument");
        assert_eq!(strong_count(&unknown), before, "the call changed the argument's refcount");

        // The argument's own reference is still the only one it lost when it goes away.
        drop(arg);
        assert_eq!(strong_count(&unknown), before - 1, "the argument released more than it owned");
        assert!(unknown.cast::<IDispatch>().is_ok(), "the mock died while we still hold it");
    }

    #[test]
    fn variant_drop_glue_is_part_of_this_build() {
        // Tripwire for the reasoning in `build_rgvarg`: the shallow `rgvarg` copies are
        // wrapped in `ManuallyDrop` because dropping a `VARIANT` clears it. If a future
        // windows-rs ever removes that glue, the argument handling stays correct but the
        // comments must be revisited, so fail loudly instead of drifting silently.
        assert!(core::mem::needs_drop::<VARIANT>());
    }

    #[test]
    fn a_late_bound_object_reports_no_type_info() {
        let (_keep, handle) = mock_dispatch();
        assert_eq!(handle.type_info_count().unwrap(), 0);
    }

    #[test]
    fn an_unknown_member_is_reported_as_not_implemented() {
        // Late binding cannot resolve the name, which means the object does not
        // expose the member; it must not look like a COM failure of the wrapper.
        let (_keep, handle) = mock_dispatch();
        let error = handle.get("NoSuchMember").map(|v| v.describe()).expect_err("not exposed");
        assert_eq!(error.kind, AscomErrorKind::Unsupported);
        assert!(error.is_unsupported());
        assert!(error.member.contains("NoSuchMember"), "member lost: {error}");
        // `try_get` is the accessor that turns exactly this into `None`.
        assert!(handle.try_get("NoSuchMember").expect("classified, not raised").is_none());
        assert_eq!(handle.probe("NoSuchMember").unwrap(), NOT_IMPLEMENTED);
    }

    #[test]
    fn an_indexed_property_returning_empty_is_value_not_set() {
        // The mock answers `Item(0)` with `VT_EMPTY` for both a property get and a
        // method call, which is the case `get_indexed` must report rather than swallow.
        let (_keep, handle) = mock_dispatch();
        let error = handle.get_indexed("Item", 0).map(|v| v.describe()).expect_err("empty result");
        assert_eq!(error.kind, AscomErrorKind::ValueNotSet);
        // The second element does have a value, so the path works when it should.
        assert_eq!(handle.get_indexed("Item", 1).and_then(|v| v.as_i32()).unwrap(), 42);
    }

    #[test]
    fn a_driver_exception_is_read_from_excepinfo_not_from_the_invoke_hresult() {
        // A .NET server answers `DISP_E_EXCEPTION` and puts the ASCOM code in
        // `EXCEPINFO.scode` (`docs/KNOWN_DRIVER_QUIRKS.md` §2.1). The mock raises it
        // the same way, so this is the only in-process cover for that branch: drop it
        // and every domain error collapses into a generic `Com`.
        for (code, kind) in [
            (0x407u16, AscomErrorKind::NotConnected),
            (0x408, AscomErrorKind::Parked),
            (0x40C, AscomErrorKind::ActionNotImplemented),
        ] {
            let mock = MockCollection {
                items: vec![Element::Int(1)],
                new_enum: NewEnumMode::Failing(ascom_hr(code)),
                item_base: ItemBase::Zero,
                hide_count: false,
                bogus_count: None,
            };
            let variant = mock.into_variant();
            let handle = Dispatch::from_variant(&variant).expect("the mock is dispatchable");
            let error = handle
                .get("NewEnum")
                .map(|v| v.describe())
                .expect_err("the mock refuses the member");
            assert_eq!(error.kind, kind, "code 0x{code:03X}");
            assert_eq!(error.code, code, "the ASCOM code was lost");
            // What the error carries is the driver's code, not the generic
            // `DISP_E_EXCEPTION` that `Invoke` answered with.
            assert_eq!(
                error.hresult,
                ascom_hr(code).0,
                "a generic COM failure was reported instead of the driver's code"
            );
            // The server's own words are in `EXCEPINFO` too, and its BSTRs became ours
            // to free, which is what `excepinfo_text` is for.
            assert_eq!(error.source, EXCEPTION_SOURCE, "EXCEPINFO source lost");
            assert_eq!(error.message, EXCEPTION_DESCRIPTION, "EXCEPINFO description lost");
        }
    }

    /// Some drivers put the bare ASCOM code into `EXCEPINFO.scode`, without the
    /// `0x8004` facility. The source is known to be a driver there, so the low bits
    /// consult the ASCOM table directly even though an HRESULT with a different
    /// prefix is no longer trusted (`from_hresult` gates on the prefix).
    #[test]
    fn a_bare_ascom_scode_is_still_read_as_an_ascom_code() {
        for (scode, kind) in [
            (0x408_i32, AscomErrorKind::Parked),
            (0x404, AscomErrorKind::Driver),
            (0x400, AscomErrorKind::Unsupported),
        ] {
            let mock = MockCollection {
                items: vec![Element::Int(1)],
                new_enum: NewEnumMode::Failing(HRESULT(scode)),
                item_base: ItemBase::Zero,
                hide_count: false,
                bogus_count: None,
            };
            let variant = mock.into_variant();
            let handle = Dispatch::from_variant(&variant).expect("the mock is dispatchable");
            let error = handle
                .get("NewEnum")
                .map(|v| v.describe())
                .expect_err("the mock refuses the member");
            assert_eq!(error.kind, kind, "bare scode 0x{scode:03X}");
            assert_eq!(error.code, scode as u16, "bare scode 0x{scode:03X} lost");
            assert_eq!(error.hresult, scode, "the driver's scode was replaced");
        }
    }

    /// `E_NOTIMPL` as the `Invoke` HRESULT is a plain COM object's "this member does
    /// not exist". `is_unsupported()` is the crate's general question for that, so
    /// it must cover this shape end-to-end through the binding layer.
    #[test]
    fn a_raw_e_notimpl_refusal_is_an_unsupported_member() {
        let mock = MockCollection {
            items: vec![Element::Int(1)],
            new_enum: NewEnumMode::FailingRaw(E_NOTIMPL),
            item_base: ItemBase::Zero,
            hide_count: false,
            bogus_count: None,
        };
        let variant = mock.into_variant();
        let handle = Dispatch::from_variant(&variant).expect("the mock is dispatchable");
        let error = handle
            .get("NewEnum")
            .map(|v| v.describe())
            .expect_err("the mock refuses with E_NOTIMPL");
        assert!(error.is_unsupported(), "E_NOTIMPL arrived as {:?}", error.kind);
    }

    #[test]
    fn a_put_carries_dispid_propertyput_and_reaches_the_server() {
        // The other half of the crate's put rule. The mock insists on the named
        // argument (see `mock::call_kind`), so a round trip here proves `Dispatch::put`
        // sends it and that a put is not answered as a get.
        let item: IDispatch = MockItem { name: "Rate", value: Cell::new(3) }.into();
        let unknown: IUnknown = item.cast().expect("the mock answers QueryInterface(IUnknown)");
        let holder = Variant::from_unknown(&unknown);
        let handle = Dispatch::from_variant(&holder).expect("the mock is dispatchable");

        handle.set_i32("Value", 11).expect("a protocol-correct put");
        assert_eq!(handle.get_i32("Value").unwrap(), 11, "the put never reached the server");
        // A read-only member still refuses writes, so the mock has not simply started
        // to accept everything.
        let error = handle.set_string("Name", "x").expect_err("Name is read-only");
        assert_eq!(error.kind, AscomErrorKind::Unsupported, "unexpected error: {error}");
    }
}
