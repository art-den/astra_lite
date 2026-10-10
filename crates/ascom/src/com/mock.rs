//! In-process COM mocks standing in for the .NET collections that ASCOM drivers
//! hand back as `VT_UNKNOWN` (`ArrayList`, `List<T>`, the VB `Collection` family).
//!
//! They exist so [`crate::com::collections`] and [`crate::com::dispatch`] can be
//! unit-tested with no driver installed: both enumeration paths (`NewEnum` →
//! `IEnumVARIANT` and `Count` + `Item`), the 0-/1-based split, broken enumerators and
//! runaway collections are all reproducible here, which is not something a live
//! simulator can be asked to do on demand.
//!
//! A mock that lies about COM makes those tests prove nothing, so the mocks reproduce
//! the server's contract rather than the easiest possible answer: a driver exception
//! is `DISP_E_EXCEPTION` plus `EXCEPINFO.scode`, a put must carry `DISPID_PROPERTYPUT`
//! and yields no value, an optional `pVarResult` is never written, and an enumerator
//! reports what it fetched even when it fails.
//!
//! Like the rest of `com`, everything unsafe lives in this file.

use std::cell::Cell;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use windows::core::{
    BSTR, Error, GUID, HRESULT, IUnknown, IUnknownImpl, Interface, PCWSTR, implement,
};
use windows::Win32::Foundation::{
    DISP_E_BADINDEX, DISP_E_EXCEPTION, DISP_E_MEMBERNOTFOUND, DISP_E_PARAMNOTFOUND,
    DISP_E_TYPEMISMATCH, DISP_E_UNKNOWNNAME, E_FAIL, E_NOTIMPL, S_FALSE, S_OK,
};
use windows::Win32::System::Com::{
    DISPPARAMS, DISPATCH_FLAGS, DISPATCH_PROPERTYPUT, DISPATCH_PROPERTYPUTREF, EXCEPINFO,
    IDispatch, IDispatch_Impl, ITypeInfo,
};
use windows::Win32::System::Ole::{
    DISPID_NEWENUM, DISPID_PROPERTYPUT, IEnumVARIANT, IEnumVARIANT_Impl,
};
use windows::Win32::System::Variant::VARIANT;

use crate::com::variant::{self, Variant};

// DISPIDs the mocks hand out. Only `DISPID_NEWENUM` is fixed by OLE; the rest are
// ours, because late binding never depends on the numeric value.
pub const DISP_COUNT: i32 = 1;
pub const DISP_ITEM: i32 = 2;
pub const DISP_NAME: i32 = 3;
pub const DISP_VALUE: i32 = 4;

/// The `EXCEPINFO` text a mocked driver raises an exception with.
///
/// A test asserts on these constants, so the suite proves the binding keeps the
/// server's own words instead of only the numeric code.
pub const EXCEPTION_SOURCE: &str = "ASCOM.OmniSim.Telescope";
pub const EXCEPTION_DESCRIPTION: &str = "Device not connected";

/// One element of a mocked collection.
#[derive(Clone, Debug, PartialEq)]
pub enum Element {
    Str(&'static str),
    Int(i32),
    /// A nested `IDispatch`, i.e. what a `Rate` or `StateValue` looks like.
    Nested { name: &'static str, value: i32 },
    /// A hole: `VT_EMPTY`, which a real enumerator may legitimately hand back.
    Empty,
}

impl Element {
    /// Builds the VARIANT for this element. `None` means "leave `VT_EMPTY`".
    fn to_variant(&self) -> Option<Variant> {
        match self {
            Element::Str(text) => Some(Variant::from_str(text)),
            Element::Int(value) => Some(Variant::from_i32(*value)),
            Element::Empty => None,
            Element::Nested { name, value } => {
                let item: IDispatch = MockItem { name: *name, value: Cell::new(*value) }.into();
                // A mock that cannot answer this is a bug in the mock. Turning it into
                // `None` would fake a `VT_EMPTY` hole, i.e. a missing element, and the
                // test would read the mock's failure as the driver's data.
                let unknown: IUnknown = item
                    .cast()
                    .expect("the mock answers QueryInterface(IUnknown)");
                Some(Variant::from_unknown(&unknown))
            }
        }
    }
}

/// Writes an element to an out-parameter, leaving `VT_EMPTY` when there is none.
///
/// # Safety
/// `dest` must be null or a valid, uninitialised `VARIANT` slot owned by the callee.
unsafe fn write_element(dest: *mut VARIANT, element: &Element) {
    match element.to_variant() {
        Some(value) => unsafe { give(dest, value) },
        None if !dest.is_null() => unsafe { core::ptr::write(dest, VARIANT::default()) },
        // A hole with nowhere to be written: a hole is still the honest answer.
        None => {}
    }
}

/// Moves a `Variant` into an out-parameter, transferring ownership of its payload.
///
/// `pVarResult` is documented optional: a client that discards the value passes NULL,
/// and a server that writes there anyway corrupts the caller's stack. The payload
/// stays ours and is cleared by `value`'s `Drop`.
///
/// # Safety
/// `dest` must be null or a valid, uninitialised `VARIANT` slot whose new owner will
/// clear it (which is exactly what `Dispatch` does when it wraps an `Invoke` result).
unsafe fn give(dest: *mut VARIANT, value: Variant) {
    if dest.is_null() {
        return;
    }
    unsafe { core::ptr::write(dest, core::ptr::read(value.raw())) };
    core::mem::forget(value);
}

/// What `wFlags` asked the server to do.
///
/// A real server must not answer a property put as a get. `IDispatch` requires a put
/// to carry exactly one named argument, `DISPID_PROPERTYPUT`, and a server that sees
/// none answers `DISP_E_PARAMNOTFOUND`; without that check the crate's own rule (a
/// put is sent with `DISPID_PROPERTYPUT`) could never be observed in-process.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Call {
    Get,
    Put,
}

/// # Safety
/// `params` must be null or point at a `DISPPARAMS` whose `rgdispidNamedArgs` really
/// has `cNamedArgs` entries, which is what `Invoke` promises.
unsafe fn call_kind(
    flags: DISPATCH_FLAGS,
    params: *const DISPPARAMS,
) -> core::result::Result<Call, HRESULT> {
    // The constants are separate bits, so `METHOD | PROPERTYGET` (3) is not a put (4).
    let put = flags.contains(DISPATCH_PROPERTYPUT) || flags.contains(DISPATCH_PROPERTYPUTREF);
    if !put {
        return Ok(Call::Get);
    }
    let named = !params.is_null() && {
        let params = unsafe { &*params };
        params.cNamedArgs == 1
            && !params.rgdispidNamedArgs.is_null()
            && unsafe { *params.rgdispidNamedArgs } == DISPID_PROPERTYPUT
    };
    if named {
        Ok(Call::Put)
    } else {
        Err(DISP_E_PARAMNOTFOUND)
    }
}

/// Refuses a put on a member that cannot be written, which is how late binding
/// reports a member the object does not expose for writing.
fn refuse_put(kind: Call) -> windows::core::Result<()> {
    if kind == Call::Put {
        return Err(Error::from_hresult(DISP_E_MEMBERNOTFOUND));
    }
    Ok(())
}

/// Raises a driver exception the way a .NET `IDispatch` server does: `Invoke` answers
/// the generic `DISP_E_EXCEPTION` and the real ASCOM code goes to `EXCEPINFO.scode`.
///
/// The BSTRs become the caller's property, as the COM contract demands; `Dispatch`
/// reads them and releases them.
///
/// # Safety
/// `excepinfo` must be null or point at a caller-owned `EXCEPINFO`.
unsafe fn raise_exception(excepinfo: *mut EXCEPINFO, scode: HRESULT) -> Error {
    if !excepinfo.is_null() {
        unsafe {
            (*excepinfo).scode = scode.0;
            core::ptr::write(
                core::ptr::addr_of_mut!((*excepinfo).bstrSource),
                core::mem::ManuallyDrop::new(BSTR::from(EXCEPTION_SOURCE)),
            );
            core::ptr::write(
                core::ptr::addr_of_mut!((*excepinfo).bstrDescription),
                core::mem::ManuallyDrop::new(BSTR::from(EXCEPTION_DESCRIPTION)),
            );
        }
    }
    Error::from_hresult(DISP_E_EXCEPTION)
}

/// Reads the single `VT_I4` argument of a call: the index of `Item(index)` or the
/// value of a property put.
///
/// `rgvarg[0]` is the *last* declared argument; with one argument that is it.
unsafe fn first_i32_arg(params: *const DISPPARAMS) -> Option<i32> {
    if params.is_null() {
        return None;
    }
    let params = unsafe { &*params };
    if params.cArgs == 0 || params.rgvarg.is_null() {
        return None;
    }
    variant::peek_i32(unsafe { &*params.rgvarg })
}

/// Reads the single `VT_BOOL` argument of a property put.
///
/// # Safety
/// `params` must be null or point at a `DISPPARAMS` whose `rgvarg` really has
/// `cArgs` entries, which is what `Invoke` promises.
unsafe fn first_bool_arg(params: *const DISPPARAMS) -> Option<bool> {
    if params.is_null() {
        return None;
    }
    let params = unsafe { &*params };
    if params.cArgs == 0 || params.rgvarg.is_null() {
        return None;
    }
    variant::peek_bool(unsafe { &*params.rgvarg })
}

/// Reports how many elements were delivered and hands back the HRESULT.
///
/// Every exit of `Next` goes through here, because `pceltfetched` must be set on the
/// failure paths too: it is the only thing that tells the caller which slots it owns.
fn report(fetched: u32, pceltfetched: *mut u32, hr: HRESULT) -> HRESULT {
    if !pceltfetched.is_null() {
        unsafe { *pceltfetched = fetched };
    }
    hr
}

/// How a mocked collection answers the hidden `NewEnum` member.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NewEnumMode {
    /// Not exposed at all: the client must fall back to `Count` + `Item`.
    Absent,
    /// Present but answers with a plain integer instead of an `IEnumVARIANT`.
    NotAnEnumerator,
    /// The normal .NET behaviour.
    Enumerator,
    /// Returns an enumerator that fails on its second element.
    Broken,
    /// Returns an enumerator that never reaches the end.
    Endless,
    /// Refuses the member with an ASCOM exception: `Invoke` answers
    /// `DISP_E_EXCEPTION` and `hr` goes to `EXCEPINFO.scode`, which is exactly what a
    /// .NET driver does when it is not connected rather than lacking an enumerator.
    Failing(HRESULT),
    /// Refuses the member with `hr` as the `Invoke` HRESULT itself, which is how a
    /// plain COM object turns the hidden member down (`E_NOTIMPL`,
    /// `DISP_E_MEMBERNOTFOUND`) instead of raising a managed exception.
    FailingRaw(HRESULT),
    /// Reaches the end while still answering `S_OK`, which the contract reserves
    /// for "the requested elements were delivered".
    OkAtEnd,
}

/// How `Item(i)` is indexed: .NET collections are 0-based, VB ones 1-based.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ItemBase {
    Zero,
    One,
    /// The collection exposes no indexer at all.
    Absent,
}

/// A `VT_UNKNOWN` collection, configurable along the axes real drivers vary on.
#[implement(IDispatch)]
pub struct MockCollection {
    pub items: Vec<Element>,
    pub new_enum: NewEnumMode,
    pub item_base: ItemBase,
    /// Hides `Count`, which is how a collection without one behaves.
    pub hide_count: bool,
    /// Answers `Count` with a value the items cannot back: a driver whose `Count`
    /// is a lie, which no real collection can be asked to produce on demand.
    pub bogus_count: Option<i32>,
}

impl MockCollection {
    /// A well-behaved .NET-style collection: `NewEnum` works, `Item` is 0-based.
    pub fn new(items: Vec<Element>) -> Self {
        Self {
            items,
            new_enum: NewEnumMode::Enumerator,
            item_base: ItemBase::Zero,
            hide_count: false,
            bogus_count: None,
        }
    }

    /// A collection that only offers `Count` + `Item(i)`.
    pub fn indexed(items: Vec<Element>, base: ItemBase) -> Self {
        Self {
            items,
            new_enum: NewEnumMode::Absent,
            item_base: base,
            hide_count: false,
            bogus_count: None,
        }
    }

    /// A collection that exposes nothing usable.
    pub fn opaque(items: Vec<Element>) -> Self {
        Self {
            items,
            new_enum: NewEnumMode::Absent,
            item_base: ItemBase::Absent,
            hide_count: true,
            bogus_count: None,
        }
    }

    /// Wraps the collection in a `Variant`, which is how a driver returns it.
    pub fn into_variant(self) -> Variant {
        // Checked here rather than in `Next`: a panic crossing the COM thunk would
        // abort the test process instead of failing the test.
        assert!(
            !(self.new_enum == NewEnumMode::Endless && self.items.is_empty()),
            "an endless mock needs an element to re-deliver"
        );
        let dispatch: IDispatch = self.into();
        let unknown: IUnknown = dispatch
            .cast()
            .expect("the mock answers QueryInterface(IUnknown)");
        Variant::from_unknown(&unknown)
    }

    fn enumerator(&self) -> MockEnumerator {
        MockEnumerator {
            items: self.items.clone(),
            cursor: Cell::new(0),
            // A broken enumerator delivers exactly one element, which is enough to
            // prove that a fallback restarted from scratch rather than resumed.
            broken_at: (self.new_enum == NewEnumMode::Broken).then_some(1),
            endless: self.new_enum == NewEnumMode::Endless,
            ok_at_end: self.new_enum == NewEnumMode::OkAtEnd,
        }
    }
}

impl IDispatch_Impl for MockCollection_Impl {
    /// Pure late binding, no type library — like the real ASCOM COM proxies.
    fn GetTypeInfoCount(&self) -> windows::core::Result<u32> {
        Ok(0)
    }

    fn GetTypeInfo(&self, _itinfo: u32, _lcid: u32) -> windows::core::Result<ITypeInfo> {
        Err(E_NOTIMPL.into())
    }

    fn GetIDsOfNames(
        &self,
        _riid: *const GUID,
        names: *const PCWSTR,
        count: u32,
        _lcid: u32,
        ids: *mut i32,
    ) -> windows::core::Result<()> {
        let collection = self.get_impl();
        for slot in 0..count as usize {
            let name = unsafe { (*names.add(slot)).to_string() }
                .map_err(|_| Error::from_hresult(DISP_E_UNKNOWNNAME))?;
            let id = match name.as_str() {
                "Count" if !collection.hide_count => DISP_COUNT,
                "Item" if collection.item_base != ItemBase::Absent => DISP_ITEM,
                // Note that `Dispatch` reaches `NewEnum` by DISPID, never by name,
                // which is why resolving it here is not enough to use it.
                "NewEnum" if collection.new_enum != NewEnumMode::Absent => DISPID_NEWENUM,
                _ => return Err(DISP_E_UNKNOWNNAME.into()),
            };
            unsafe { *ids.add(slot) = id };
        }
        Ok(())
    }

    fn Invoke(
        &self,
        dispid: i32,
        _riid: *const GUID,
        _lcid: u32,
        flags: DISPATCH_FLAGS,
        params: *const DISPPARAMS,
        result: *mut VARIANT,
        excepinfo: *mut EXCEPINFO,
        _argerr: *mut u32,
    ) -> windows::core::Result<()> {
        let collection = self.get_impl();
        let kind = unsafe { call_kind(flags, params) }.map_err(Error::from_hresult)?;
        match dispid {
            DISP_COUNT => {
                refuse_put(kind)?;
                let count = match collection.bogus_count {
                    Some(count) => count,
                    None => i32::try_from(collection.items.len()).unwrap_or(i32::MAX),
                };
                unsafe { give(result, Variant::from_i32(count)) };
                Ok(())
            }
            DISP_ITEM => {
                refuse_put(kind)?;
                let base = match collection.item_base {
                    ItemBase::Zero => 0,
                    ItemBase::One => 1,
                    ItemBase::Absent => return Err(DISP_E_MEMBERNOTFOUND.into()),
                };
                let index =
                    unsafe { first_i32_arg(params) }.ok_or(Error::from_hresult(DISP_E_BADINDEX))?;
                let position = index - base;
                let element = collection
                    .items
                    .get(usize::try_from(position).unwrap_or(usize::MAX))
                    .ok_or(Error::from_hresult(DISP_E_BADINDEX))?;
                unsafe { write_element(result, element) };
                Ok(())
            }
            DISPID_NEWENUM => {
                refuse_put(kind)?;
                match collection.new_enum {
                    NewEnumMode::Absent => Err(DISP_E_MEMBERNOTFOUND.into()),
                    NewEnumMode::Failing(scode) => Err(unsafe { raise_exception(excepinfo, scode) }),
                    NewEnumMode::FailingRaw(hr) => Err(Error::from_hresult(hr)),
                    NewEnumMode::NotAnEnumerator => {
                        unsafe { give(result, Variant::from_i32(-1)) };
                        Ok(())
                    }
                    _ => {
                        let enumerator: IEnumVARIANT = collection.enumerator().into();
                        let unknown: IUnknown = enumerator
                            .cast()
                            .map_err(|_| Error::from_hresult(E_FAIL))?;
                        unsafe { give(result, Variant::from_unknown(&unknown)) };
                        Ok(())
                    }
                }
            }
            _ => Err(DISP_E_MEMBERNOTFOUND.into()),
        }
    }
}

/// A nested collection element that is itself an `IDispatch` with a read-only `Name`
/// and a writable `Value`, i.e. the shape of `Rate` and `StateValue`.
#[implement(IDispatch)]
pub struct MockItem {
    pub name: &'static str,
    /// Writable, so a property put has somewhere to land (`Cell` because `Invoke`
    /// receives `&self`).
    pub value: Cell<i32>,
}

impl IDispatch_Impl for MockItem_Impl {
    fn GetTypeInfoCount(&self) -> windows::core::Result<u32> {
        Ok(0)
    }

    fn GetTypeInfo(&self, _itinfo: u32, _lcid: u32) -> windows::core::Result<ITypeInfo> {
        Err(E_NOTIMPL.into())
    }

    fn GetIDsOfNames(
        &self,
        _riid: *const GUID,
        names: *const PCWSTR,
        count: u32,
        _lcid: u32,
        ids: *mut i32,
    ) -> windows::core::Result<()> {
        for slot in 0..count as usize {
            let name = unsafe { (*names.add(slot)).to_string() }
                .map_err(|_| Error::from_hresult(DISP_E_UNKNOWNNAME))?;
            let id = match name.as_str() {
                "Name" => DISP_NAME,
                "Value" => DISP_VALUE,
                _ => return Err(DISP_E_UNKNOWNNAME.into()),
            };
            unsafe { *ids.add(slot) = id };
        }
        Ok(())
    }

    fn Invoke(
        &self,
        dispid: i32,
        _riid: *const GUID,
        _lcid: u32,
        flags: DISPATCH_FLAGS,
        params: *const DISPPARAMS,
        result: *mut VARIANT,
        _excepinfo: *mut EXCEPINFO,
        _argerr: *mut u32,
    ) -> windows::core::Result<()> {
        let item = self.get_impl();
        let kind = unsafe { call_kind(flags, params) }.map_err(Error::from_hresult)?;
        match dispid {
            DISP_NAME => {
                refuse_put(kind)?;
                unsafe { give(result, Variant::from_str(item.name)) };
            }
            DISP_VALUE => match kind {
                Call::Get => unsafe { give(result, Variant::from_i32(item.value.get())) },
                // A put answers with no value at all, which is why every write here
                // has to tolerate a NULL `pVarResult`.
                Call::Put => {
                    let value = unsafe { first_i32_arg(params) }
                        .ok_or(Error::from_hresult(DISP_E_BADINDEX))?;
                    item.value.set(value);
                }
            },
            _ => return Err(DISP_E_MEMBERNOTFOUND.into()),
        }
        Ok(())
    }
}

/// How one named member of a `MockDevice` answers.
#[derive(Clone, Debug)]
pub enum Member {
    /// Answers with this element. `Element::Empty` leaves `pVarResult` as `VT_EMPTY`,
    /// which is how a call that returns no value at all looks on the wire.
    Value(Element),
    /// Raises a driver exception whose `EXCEPINFO.scode` is this HRESULT: how a .NET
    /// driver refuses a member it does have (e.g. an ASCOM `NotConnected`).
    Refuses(HRESULT),
    /// The only writable member of a `MockDevice`: a get answers the current value,
    /// a put stores it. Shared with the test thread so a driver's `Connected` flag stays
    /// observable while `Invoke` runs on the device's actor thread.
    Flag(Arc<AtomicBool>),
}

/// DISPIDs handed out by [`MockDevice`]. Late binding never depends on the numeric
/// value, so these only have to stay clear of the collection mocks'.
const FIRST_MEMBER_DISPID: i32 = 100;

/// A device-shaped `IDispatch`: members addressed by name, each with its own answer.
///
/// The collection mocks cannot express what the shared members need — a driver that
/// answers `InterfaceVersion` but refuses `Name` while disconnected, or a
/// `CommandString` that returns no value — so those get their own mock.
#[implement(IDispatch)]
pub struct MockDevice {
    pub members: Vec<(&'static str, Member)>,
}

impl MockDevice {
    pub fn new(members: Vec<(&'static str, Member)>) -> Self {
        Self { members }
    }

    /// Wraps the device in a `Variant`, which is how a driver object is handed out.
    pub fn into_variant(self) -> Variant {
        let dispatch: IDispatch = self.into();
        let unknown: IUnknown = dispatch
            .cast()
            .expect("the mock answers QueryInterface(IUnknown)");
        Variant::from_unknown(&unknown)
    }
}

impl IDispatch_Impl for MockDevice_Impl {
    fn GetTypeInfoCount(&self) -> windows::core::Result<u32> {
        Ok(0)
    }

    fn GetTypeInfo(&self, _itinfo: u32, _lcid: u32) -> windows::core::Result<ITypeInfo> {
        Err(E_NOTIMPL.into())
    }

    fn GetIDsOfNames(
        &self,
        _riid: *const GUID,
        names: *const PCWSTR,
        count: u32,
        _lcid: u32,
        ids: *mut i32,
    ) -> windows::core::Result<()> {
        let device = self.get_impl();
        for slot in 0..count as usize {
            let name = unsafe { (*names.add(slot)).to_string() }
                .map_err(|_| Error::from_hresult(DISP_E_UNKNOWNNAME))?;
            // A name the device does not carry is refused, which the binding reads as
            // "this driver does not implement the member".
            let index = device
                .members
                .iter()
                .position(|(member, _)| *member == name)
                .ok_or_else(|| Error::from_hresult(DISP_E_UNKNOWNNAME))?;
            unsafe { *ids.add(slot) = FIRST_MEMBER_DISPID + index as i32 };
        }
        Ok(())
    }

    fn Invoke(
        &self,
        dispid: i32,
        _riid: *const GUID,
        _lcid: u32,
        flags: DISPATCH_FLAGS,
        params: *const DISPPARAMS,
        result: *mut VARIANT,
        excepinfo: *mut EXCEPINFO,
        _argerr: *mut u32,
    ) -> windows::core::Result<()> {
        let device = self.get_impl();
        let kind = unsafe { call_kind(flags, params) }.map_err(Error::from_hresult)?;
        let member = usize::try_from(dispid - FIRST_MEMBER_DISPID)
            .ok()
            .and_then(|index| device.members.get(index))
            .map(|(_, member)| member)
            .ok_or_else(|| Error::from_hresult(DISP_E_MEMBERNOTFOUND))?;
        // A `Flag` is the only member exposed for writing, so a put on any other is
        // refused the way a real driver refuses a member it does not write.
        if !matches!(member, Member::Flag(_)) {
            refuse_put(kind)?;
        }
        match member {
            Member::Flag(flag) => {
                if kind == Call::Put {
                    // `call_kind` folds a by-ref write into `Call::Put`, but a real
                    // server refuses a putref on a `bool`, so this member must too.
                    if flags.contains(DISPATCH_PROPERTYPUTREF) {
                        return Err(Error::from_hresult(DISP_E_MEMBERNOTFOUND));
                    }
                    let value = unsafe { first_bool_arg(params) }
                        .ok_or(Error::from_hresult(DISP_E_TYPEMISMATCH))?;
                    flag.store(value, Ordering::SeqCst);
                    // A property put answers with no value at all.
                    return Ok(());
                }
                unsafe { give(result, Variant::from_bool(flag.load(Ordering::SeqCst))) };
                Ok(())
            }
            Member::Refuses(scode) => Err(unsafe { raise_exception(excepinfo, *scode) }),
            Member::Value(element) => {
                unsafe { write_element(result, element) };
                Ok(())
            }
        }
    }
}

/// `IEnumVARIANT` over a snapshot of the collection.
#[implement(IEnumVARIANT)]
pub struct MockEnumerator {
    pub items: Vec<Element>,
    pub cursor: Cell<u32>,
    /// Fails when the cursor reaches here instead of walking to the end.
    pub broken_at: Option<u32>,
    /// Never reports the end, which is how a broken driver can hang a client.
    pub endless: bool,
    /// Reports the end with `S_OK` and nothing delivered, breaking the contract.
    pub ok_at_end: bool,
}

impl IEnumVARIANT_Impl for MockEnumerator_Impl {
    fn Next(&self, celt: u32, rgvar: *mut VARIANT, pceltfetched: *mut u32) -> HRESULT {
        let enumerator = self.get_impl();
        if celt == 0 {
            // Legal, and `rgvar` may even be NULL with it: the request is already
            // satisfied, so report 0 fetched instead of writing past the buffer.
            return report(0, pceltfetched, S_OK);
        }
        let mut fetched = 0u32;
        while fetched < celt {
            let index = enumerator.cursor.get();
            if enumerator.broken_at.is_some_and(|fails_at| index >= fails_at) {
                // Whatever was written before the failure belongs to the caller; only
                // `pceltfetched` can tell it which slots those are.
                return report(fetched, pceltfetched, E_FAIL);
            }
            // An endless enumerator re-delivers its last element forever.
            let element = if enumerator.endless {
                enumerator.items.last()
            } else {
                enumerator.items.get(index as usize)
            };
            let Some(element) = element else {
                if enumerator.ok_at_end {
                    // Contract violation: `S_OK` claims `celt` elements were fetched.
                    return report(fetched, pceltfetched, S_OK);
                }
                break
            };
            unsafe { write_element(rgvar.add(usize::try_from(fetched).unwrap_or(0)), element) };
            enumerator.cursor.set(index + 1);
            fetched += 1;
        }
        report(fetched, pceltfetched, if fetched == celt { S_OK } else { S_FALSE })
    }

    fn Skip(&self, celt: u32) -> HRESULT {
        let enumerator = self.get_impl();
        let wanted = enumerator.cursor.get().saturating_add(celt);
        // An endless enumerator has no end to fall short of; for every other one the
        // element count is the end, and falling short of it is `S_FALSE`.
        let end = if enumerator.endless {
            u32::MAX
        } else {
            u32::try_from(enumerator.items.len()).unwrap_or(u32::MAX)
        };
        enumerator.cursor.set(u32::min(wanted, end));
        if wanted <= end { S_OK } else { S_FALSE }
    }

    fn Reset(&self) -> windows::core::Result<()> {
        self.get_impl().cursor.set(0);
        Ok(())
    }

    /// Never called by `collections`, so there is nothing to implement honestly.
    fn Clone(&self) -> windows::core::Result<IEnumVARIANT> {
        Err(E_NOTIMPL.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::System::Com::DISPATCH_PROPERTYGET;

    /// An `IEnumVARIANT` over `items` with the requested defect.
    fn enumerator(items: Vec<Element>, broken_at: Option<u32>, endless: bool) -> IEnumVARIANT {
        MockEnumerator {
            items,
            cursor: Cell::new(0),
            broken_at,
            endless,
            ok_at_end: false,
        }
        .into()
    }

    /// Calls `Next` through the vtable with an explicit `celt`, which the windows-rs
    /// wrapper derives from the slice length and therefore cannot pin down.
    ///
    /// `slot` is deliberately longer than the request so an honest mock has room and
    /// a lying one still cannot smash the stack.
    unsafe fn next(enumerator: &IEnumVARIANT, celt: u32, slot: &mut [VARIANT]) -> (HRESULT, u32) {
        let mut fetched = u32::MAX;
        let hr = unsafe {
            (enumerator.vtable().Next)(
                enumerator.as_raw(),
                celt,
                slot.as_mut_ptr(),
                core::ptr::addr_of_mut!(fetched),
            )
        };
        (hr, fetched)
    }

    #[test]
    fn next_with_celt_zero_fetches_nothing() {
        // `celt == 0` is legal and `rgvar` may be NULL with it. Writing an element
        // here would land outside the caller's buffer, and reporting one fetched
        // would make the caller read memory it never received.
        let enumerator = enumerator(vec![Element::Int(1), Element::Int(2)], None, false);
        let mut slot = [VARIANT::default()];
        let (hr, fetched) = unsafe { next(&enumerator, 0, &mut slot) };
        assert_eq!(hr, S_OK, "a zero-element request is satisfied by doing nothing");
        assert_eq!(fetched, 0, "the enumerator claimed an element it did not fetch");
        assert!(variant::is_raw_empty(&slot[0]), "an element was written unasked");

        // The same call with no buffer at all, which the contract allows.
        let mut fetched = u32::MAX;
        let hr = unsafe {
            (enumerator.vtable().Next)(enumerator.as_raw(), 0, core::ptr::null_mut(), core::ptr::addr_of_mut!(fetched))
        };
        assert_eq!(hr, S_OK);
        assert_eq!(fetched, 0);
    }

    #[test]
    fn a_failure_reports_the_elements_it_already_wrote() {
        // Two requested, one delivered, then the driver breaks: `pceltfetched` is the
        // only thing that tells the caller slot 0 is its property (and slot 1 is not).
        let enumerator = enumerator(vec![Element::Int(1), Element::Int(2)], Some(1), false);
        let mut slot: [VARIANT; 2] = core::array::from_fn(|_| VARIANT::default());
        let (hr, fetched) = unsafe { next(&enumerator, 2, &mut slot) };
        assert_eq!(hr, E_FAIL);
        assert_eq!(fetched, 1, "the caller cannot know which slot it owns");
        assert!(!variant::is_raw_empty(&slot[0]));
        assert!(variant::is_raw_empty(&slot[1]));
    }

    #[test]
    fn skip_reports_reaching_the_end() {
        // `S_OK` means `celt` elements were skipped; falling short is `S_FALSE`.
        let enumerator = enumerator(vec![Element::Int(1), Element::Int(2)], None, false);
        unsafe {
            assert_eq!(enumerator.Skip(1), S_OK, "one of two elements");
            assert_eq!(enumerator.Skip(1), S_OK, "the last element, exactly");
            assert_eq!(enumerator.Skip(1), S_FALSE, "nothing left to skip");
        }
        let mut slot = [VARIANT::default()];
        let (hr, fetched) = unsafe { next(&enumerator, 1, &mut slot) };
        assert_eq!(hr, S_FALSE);
        assert_eq!(fetched, 0);
    }

    #[test]
    fn skipping_past_the_end_stops_at_it() {
        let enumerator = enumerator(vec![Element::Int(1)], None, false);
        assert_eq!(unsafe { enumerator.Skip(5) }, S_FALSE, "only one element exists");
        let mut slot = [VARIANT::default()];
        let (hr, fetched) = unsafe { next(&enumerator, 1, &mut slot) };
        assert_eq!(hr, S_FALSE, "the cursor must not run past the end");
        assert_eq!(fetched, 0);
    }

    #[test]
    fn an_endless_enumerator_never_reports_the_end() {
        let enumerator = enumerator(vec![Element::Int(7)], None, true);
        assert_eq!(unsafe { enumerator.Skip(100) }, S_OK, "an endless stream has no end");
        for _ in 0..3 {
            // A fresh slot per round: dropping it clears whatever was delivered.
            let mut slot = [VARIANT::default()];
            let (hr, fetched) = unsafe { next(&enumerator, 1, &mut slot) };
            assert_eq!((hr, fetched), (S_OK, 1));
            assert_eq!(variant::peek_i32(&slot[0]), Some(7));
        }
    }

    #[test]
    fn a_get_that_wants_no_value_is_still_answered() {
        // `pVarResult` is documented optional: a client that throws the value away
        // passes NULL, and a server that writes there anyway corrupts its stack.
        let collection: IDispatch = MockCollection::new(vec![Element::Int(7)]).into();
        unsafe {
            collection
                .Invoke(
                    DISP_COUNT,
                    &GUID::zeroed(),
                    0,
                    DISPATCH_PROPERTYGET,
                    core::ptr::null(),
                    None,
                    None,
                    None,
                )
                .expect("a get with NULL pVarResult must still succeed");
        }
    }

    #[test]
    fn a_put_without_the_named_argument_is_a_protocol_error() {
        // The mock has to be as strict as a real server, or the binding's own
        // `DISPID_PROPERTYPUT` rule could never be checked in-process.
        let item: IDispatch = MockItem { name: "Rate", value: Cell::new(3) }.into();
        let value = Variant::from_i32(9);
        // A borrowed copy for `rgvarg`; `value` stays the only owner of the payload.
        let mut argv = [core::mem::ManuallyDrop::new(unsafe { core::ptr::read(value.raw()) })];
        let params = DISPPARAMS {
            rgvarg: argv.as_mut_ptr().cast::<VARIANT>(),
            rgdispidNamedArgs: core::ptr::null_mut(),
            cArgs: 1,
            cNamedArgs: 0,
        };
        let error = unsafe {
            item.Invoke(
                DISP_VALUE,
                &GUID::zeroed(),
                0,
                DISPATCH_PROPERTYPUT,
                &params,
                None,
                None,
                None,
            )
        }
        .expect_err("a put with no DISPID_PROPERTYPUT must be refused");
        assert_eq!(error.code(), DISP_E_PARAMNOTFOUND, "the mock answered a put it must refuse");
    }

    #[test]
    fn a_flag_member_answers_its_value_and_keeps_a_put() {
        // A driver flag (`Connected`) has to survive a put and stay readable from the
        // asserting thread, or the activation-rollback tests prove nothing.
        let flag = Arc::new(AtomicBool::new(false));
        let keep =
            MockDevice::new(vec![("Connected", Member::Flag(Arc::clone(&flag)))]).into_variant();
        let dispatch =
            crate::com::Dispatch::from_variant(&keep).expect("the mock is dispatchable");

        assert!(!dispatch.get_bool("Connected").expect("a get answers the flag"));
        dispatch.set_bool("Connected", true).expect("a well-formed put must be accepted");
        assert!(dispatch.get_bool("Connected").expect("the written value must read back"));
        assert!(flag.load(Ordering::SeqCst), "the put never reached the shared flag");

        dispatch.set_bool("Connected", false).expect("clearing the flag must work");
        assert!(!flag.load(Ordering::SeqCst), "the flag kept the old value");
    }

    #[test]
    fn a_flag_putref_is_refused() {
        // A by-ref write is not a value put, and the writable member must not turn it
        // into one: every other member already gets `DISP_E_MEMBERNOTFOUND`.
        let flag = Arc::new(AtomicBool::new(false));
        let device: IDispatch =
            MockDevice::new(vec![("Connected", Member::Flag(Arc::clone(&flag)))]).into();
        let value = Variant::from_bool(true);
        let mut argv = [core::mem::ManuallyDrop::new(unsafe { core::ptr::read(value.raw()) })];
        let mut named = [DISPID_PROPERTYPUT];
        let params = DISPPARAMS {
            rgvarg: argv.as_mut_ptr().cast::<VARIANT>(),
            rgdispidNamedArgs: named.as_mut_ptr(),
            cArgs: 1,
            cNamedArgs: 1,
        };
        let error = unsafe {
            device.Invoke(
                FIRST_MEMBER_DISPID,
                &GUID::zeroed(),
                0,
                DISPATCH_PROPERTYPUTREF,
                &params,
                None,
                None,
                None,
            )
        }
        .expect_err("a by-ref write to a bool member must be refused");
        assert_eq!(error.code(), DISP_E_MEMBERNOTFOUND, "a putref was taken for a put");
        assert!(!flag.load(Ordering::SeqCst), "a refused putref wrote the flag");
    }

    #[test]
    fn a_flag_put_without_the_named_argument_is_still_refused() {
        // Making one member writable must not open a hole in the put protocol.
        let flag = Arc::new(AtomicBool::new(false));
        let device: IDispatch =
            MockDevice::new(vec![("Connected", Member::Flag(Arc::clone(&flag)))]).into();
        let value = Variant::from_bool(true);
        // A borrowed copy for `rgvarg`; `value` stays the only owner of the payload.
        let mut argv = [core::mem::ManuallyDrop::new(unsafe { core::ptr::read(value.raw()) })];
        let params = DISPPARAMS {
            rgvarg: argv.as_mut_ptr().cast::<VARIANT>(),
            rgdispidNamedArgs: core::ptr::null_mut(),
            cArgs: 1,
            cNamedArgs: 0,
        };
        let error = unsafe {
            device.Invoke(
                FIRST_MEMBER_DISPID,
                &GUID::zeroed(),
                0,
                DISPATCH_PROPERTYPUT,
                &params,
                None,
                None,
                None,
            )
        }
        .expect_err("a put with no DISPID_PROPERTYPUT must be refused");
        assert_eq!(error.code(), DISP_E_PARAMNOTFOUND, "the flag member bypassed the check");
        assert!(!flag.load(Ordering::SeqCst), "a refused put wrote the flag");
    }
}
