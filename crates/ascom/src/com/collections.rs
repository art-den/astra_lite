//! Enumerating .NET collections that arrive over COM as `VT_UNKNOWN`.
//!
//! `SupportedActions` (an `ArrayList`), `DeviceState` (a `List[StateValue]`) and
//! `TrackingRates` are all exposed this way. Two paths, in the order the ASOM COM
//! guide prescribes:
//!
//! 1. `NewEnum` (`DISPID_NEWENUM`) → `IEnumVARIANT`;
//! 2. if that fails, the dispatch properties `Count` + `Item(i)`.
//!
//! A missing collection is *not* an error — a driver may legitimately expose none, and
//! that absence is expressed by the caller: `Dispatch::try_get` answers `None` for a
//! member the driver does not implement, and a device with nothing to list answers with
//! a real empty collection. A value that arrives as `VT_NULL` or `VT_EMPTY` is neither,
//! so the typed readers refuse it. A collection that answers with a hole, a nonsense
//! `Count` or a stream that ends early *is* an error: in both paths a partial list must
//! never look like a complete one.

use windows::Win32::Foundation::S_OK;
use windows::Win32::System::Com::{DISPATCH_METHOD, DISPATCH_PROPERTYGET};
use windows::core::Interface;
use windows::Win32::System::Ole::{DISPID_NEWENUM, IEnumVARIANT};
use windows::Win32::System::Variant::VARIANT;

use crate::com::variant::{Variant, VariantKind};
use crate::com::Dispatch;
use crate::error::{AscomError, AscomErrorKind, Result};

/// Upper bound so a driver returning a nonsense `Count` cannot make us allocate
/// forever. Both enumeration paths *reject* a collection over it: reading part of
/// one would look like a complete answer.
const MAX_COLLECTION: usize = 1_000_000;

/// The shared answer for a collection too large to read, so neither path can
/// report a partial list as a success.
fn too_large(member: &str) -> AscomError {
    AscomError::local(
        AscomErrorKind::Driver,
        member,
        format!("collection exceeded {MAX_COLLECTION} elements"),
    )
}

/// A member declared as a collection that answered with no collection at all.
///
/// `VT_NULL` is a .NET `null` reference and `VT_EMPTY` is the absence of a reply
/// (`Dispatch::get` reports the same thing for a plain member). Neither is the spec's
/// "this device has nothing here", which is a real empty collection, so turning one
/// into an empty `Vec` would make a missing answer look like a complete one.
fn no_collection(shape: &str, vt: &str) -> AscomError {
    AscomError::local(
        AscomErrorKind::ValueNotSet,
        shape,
        format!("the driver answered {vt} where a collection was declared"),
    )
}

/// Enumerates a collection object into owned variants.
pub fn elements(collection: &Dispatch) -> Result<Vec<Variant>> {
    match new_enum(collection)? {
        Some(enumerator) => match enumerate(enumerator) {
            Ok(items) => Ok(items),
            // A broken enumerator must not lose the collection outright.
            Err(enum_err) => count_and_item(collection).map_err(|_| enum_err),
        },
        None => count_and_item(collection),
    }
}

/// "This object offers no enumerator", which is the only answer that justifies the
/// `Count` + `Item` fallback.
///
/// This covers `E_NOTIMPL`, with which a plain COM object refuses the hidden member
/// instead of raising ASCOM's `PropertyNotImplementedException`: `from_hresult`
/// classifies it as `Unsupported` for exactly that reason.
fn offers_no_enumerator(err: &AscomError) -> bool {
    err.is_unsupported()
}

/// Turns whatever `NewEnum` returns into an `IEnumVARIANT`, if there is one.
///
/// `Ok(None)` means "no enumerator here", the driver's or COM's answer. A binding
/// failure (`Disconnected`, `NotConnected`, an exhausted transient rejection) is
/// returned instead of being traded for the fallback: swallowing it would let a
/// dead device look like a driver that simply has no `NewEnum`.
fn new_enum(collection: &Dispatch) -> Result<Option<IEnumVARIANT>> {
    // METHOD | PROPERTYGET is the documented OLE idiom for retrieving the hidden
    // NewEnum member; it is not a substitute for an ordinary property read.
    let raw = match collection.invoke_dispid(
        "NewEnum",
        DISPID_NEWENUM,
        DISPATCH_METHOD | DISPATCH_PROPERTYGET,
        &[],
        &mut [],
        true,
    ) {
        // An empty reply is how a wrapper whose enumerator returns `null` says "no
        // enumerator", so it falls back rather than failing the whole read.
        Ok(Some(raw)) => raw,
        Ok(None) => return Ok(None),
        Err(err) if offers_no_enumerator(&err) => return Ok(None),
        Err(err) => return Err(err),
    };
    // A member that answers with something un-enumerable is the driver's shape,
    // not a binding failure, so the fallback still gets its chance.
    let Ok(unknown) = raw.as_unknown() else {
        return Ok(None);
    };
    Ok(unknown.cast().ok())
}

fn enumerate(enumerator: IEnumVARIANT) -> Result<Vec<Variant>> {
    // One at a time: element counts are not always known up front.
    let mut out = Vec::new();
    loop {
        let mut slot = [VARIANT::default()];
        let mut fetched = 0u32;
        let hr = unsafe { enumerator.Next(&mut slot, &mut fetched) };
        // S_OK means the request was fully satisfied, S_FALSE only partly; both succeed.
        if hr.is_err() {
            // Clear whatever the server may have written before failing.
            for value in slot {
                drop(unsafe { Variant::from_raw(value) });
            }
            return Err(crate::com::fail(hr.0, "IEnumVARIANT::Next"));
        }
        if fetched == 0 {
            // The end of the stream is `S_FALSE` with nothing delivered. `S_OK`
            // promises the requested elements, so an empty `S_OK` is a driver
            // breaking the contract; ending here would return a short list that
            // reads as a complete collection.
            if hr.0 == S_OK.0 {
                return Err(AscomError::local(
                    AscomErrorKind::Driver,
                    "IEnumVARIANT::Next",
                    "S_OK with 0 elements fetched",
                ));
            }
            return Ok(out);
        }
        for value in slot {
            out.push(unsafe { Variant::from_raw(value) });
        }
        if out.len() > MAX_COLLECTION {
            return Err(too_large("NewEnum"));
        }
    }
}

/// Second path: `Count` plus `Item(i)`.
///
/// .NET `ArrayList`/`List` are 0-based while a VB `Collection` is 1-based, so the
/// base is probed rather than assumed.
fn count_and_item(collection: &Dispatch) -> Result<Vec<Variant>> {
    let count = collection.get_i32("Count")?;
    if count <= 0 {
        return Ok(Vec::new());
    }
    // Same guard as the enumerator: clamping a lying `Count` would spend a million
    // cross-process calls and hand back a truncated collection as a success.
    if usize::try_from(count).unwrap_or(usize::MAX) > MAX_COLLECTION {
        return Err(too_large("Count"));
    }
    let count = count as usize;
    let base = match collection.get_indexed("Item", 0) {
        Ok(_) => 0,
        Err(_) => 1,
    };
    let mut out = Vec::with_capacity(count.min(4096));
    for offset in 0..count {
        let index = base + offset as i32;
        out.push(collection.get_indexed("Item", index)?);
    }
    Ok(out)
}

/// `Vec<String>` from a collection or an array, whichever the driver exposed.
///
/// Used by `SupportedActions`, `Gains`, `Offsets`, `ReadoutModes`.
pub fn strings(value: &Variant) -> Result<Vec<String>> {
    match value.kind() {
        VariantKind::Empty => Err(no_collection("string collection", "VT_EMPTY")),
        VariantKind::Null => Err(no_collection("string collection", "VT_NULL")),
        VariantKind::Array => value.as_string_array(),
        VariantKind::Object => {
            let collection = Dispatch::from_variant(value)?;
            let mut out = Vec::new();
            for item in elements(&collection)? {
                // A hole is an error, not a skipped index: `Names` is read by slot
                // number, so dropping an element would misalign every later name.
                out.push(item.as_str()?);
            }
            Ok(out)
        }
        // A lone string where a list was declared: one element, not an error.
        VariantKind::Str(s) => Ok(vec![s]),
        other => Err(AscomError::type_mismatch("string collection", format!("{other:?}"))),
    }
}

/// `Vec<i32>` from a collection or an array (`TrackingRates` can be either).
pub fn ints(value: &Variant) -> Result<Vec<i32>> {
    match value.kind() {
        VariantKind::Empty => Err(no_collection("integer collection", "VT_EMPTY")),
        VariantKind::Null => Err(no_collection("integer collection", "VT_NULL")),
        VariantKind::Array => value.as_i32_array(),
        VariantKind::Object => {
            let collection = Dispatch::from_variant(value)?;
            let mut out = Vec::new();
            for item in elements(&collection)? {
                // As in `strings`: a hole shifts every later entry, so it must fail.
                out.push(item.as_i32()?);
            }
            Ok(out)
        }
        other => Err(AscomError::type_mismatch("integer collection", format!("{other:?}"))),
    }
}

/// Nested objects of a dispatch collection (`Rate`, `StateValue`).
///
/// Unlike [`strings`] and [`ints`] there is no SAFEARRAY arm: the specification
/// exposes these members as `List<T>` (`VT_UNKNOWN`) and no driver is known to
/// answer them with an array, so an array is reported as a type mismatch instead
/// of going down a path no test can cover.
pub fn objects(value: &Variant) -> Result<Vec<Dispatch>> {
    match value.kind() {
        VariantKind::Empty => Err(no_collection("object collection", "VT_EMPTY")),
        VariantKind::Null => Err(no_collection("object collection", "VT_NULL")),
        VariantKind::Object => {
            let collection = Dispatch::from_variant(value)?;
            let mut out = Vec::new();
            for item in elements(&collection)? {
                // A non-object element must not vanish quietly: an empty `Vec` here
                // is what `AxisRates` reports as "this axis has no rates".
                out.push(Dispatch::from_variant(&item)?);
            }
            Ok(out)
        }
        other => Err(AscomError::type_mismatch("object collection", format!("{other:?}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::com::mock::{Element, ItemBase, MockCollection, NewEnumMode};
    use windows::Win32::System::Variant::{VARIANT_0_0, VT_EMPTY, VT_NULL};
    use windows::core::HRESULT;
    use windows::Win32::Foundation::{DISP_E_MEMBERNOTFOUND, E_NOTIMPL};

    /// An ASCOM exception HRESULT, e.g. code `0x407` = NotConnected.
    fn ascom_hr(code: u16) -> HRESULT {
        HRESULT(0x8004_0000_u32 as i32 | i32::from(code))
    }

    /// Wraps a mock collection the way a driver returns one: inside a VARIANT.
    ///
    /// The returned `Variant` is kept alive so the tests cannot accidentally depend
    /// on the extra reference `Dispatch::from_variant` takes for itself.
    fn collection(mock: MockCollection) -> (Variant, Dispatch) {
        let variant = mock.into_variant();
        let dispatch = Dispatch::from_variant(&variant).expect("the mock is dispatchable");
        (variant, dispatch)
    }

    fn strings_of(items: &[Element]) -> Result<Vec<String>> {
        strings(&MockCollection::new(items.to_vec()).into_variant())
    }

    fn as_strings(items: Vec<Variant>) -> Vec<String> {
        items.iter().map(|v| v.as_str().expect("string element")).collect()
    }

    /// `VT_ARRAY | VT_BSTR`, the other shape a driver may answer with.
    fn bstr_array(items: &[&str]) -> Variant {
        use windows::Win32::System::Com::SAFEARRAYBOUND;
        use windows::Win32::System::Ole::{SafeArrayCreate, SafeArrayPutElement};
        use windows::Win32::System::Variant::VT_BSTR;

        let bounds = [SAFEARRAYBOUND { cElements: items.len() as u32, lLbound: 0 }];
        let psa = unsafe { SafeArrayCreate(VT_BSTR, 1, bounds.as_ptr()) };
        assert!(!psa.is_null());
        for (index, text) in items.iter().enumerate() {
            let bstr = windows::core::BSTR::from(*text);
            // For VT_BSTR, oleaut32 expects the BSTR itself (not a pointer to it)
            // and duplicates it, so the local BSTR stays ours to free.
            let cell = (&*bstr).as_ptr();
            let index = [index as i32];
            unsafe {
                SafeArrayPutElement(psa, index.as_ptr(), cell.cast()).expect("SafeArrayPutElement");
            }
        }
        unsafe { Variant::from_owned_array(psa, VT_BSTR) }
    }

    // --- the NewEnum path -------------------------------------------------

    #[test]
    fn new_enum_is_used_when_it_is_the_only_path() {
        // No `Count`, no `Item`: only `NewEnum` can possibly succeed here.
        let mock = MockCollection {
            items: vec![Element::Str("Focus"), Element::Str("Calibrate")],
            new_enum: NewEnumMode::Enumerator,
            item_base: ItemBase::Absent,
            hide_count: true,
            bogus_count: None,
        };
        let (_keep, handle) = collection(mock);
        let items = elements(&handle).expect("NewEnum enumeration");
        assert_eq!(as_strings(items), ["Focus", "Calibrate"]);
    }

    #[test]
    fn an_empty_collection_yields_no_items() {
        let (_keep, handle) = collection(MockCollection::new(Vec::new()));
        assert!(elements(&handle).expect("empty enumeration").is_empty());
    }

    #[test]
    fn nested_objects_arrive_as_dispatchables() {
        let mock = MockCollection::new(vec![
            Element::Nested { name: "sidereal", value: 0 },
            Element::Nested { name: "lunar", value: 1 },
        ]);
        let variant = mock.into_variant();
        let rates = objects(&variant).expect("object collection");
        assert_eq!(rates.len(), 2);
        assert_eq!(rates[0].get_string("Name").unwrap(), "sidereal");
        assert_eq!(rates[1].get_i32("Value").unwrap(), 1);
    }

    #[test]
    fn an_endless_enumerator_is_stopped_by_the_guard() {
        // Nothing to fall back to, so the guard's own error must surface rather
        // than an unbounded allocation.
        let mock = MockCollection {
            items: vec![Element::Str("tick")],
            new_enum: NewEnumMode::Endless,
            item_base: ItemBase::Absent,
            hide_count: true,
            bogus_count: None,
        };
        let (_keep, handle) = collection(mock);
        let error = elements(&handle).map(|items| items.len()).expect_err("a runaway collection must be rejected");
        assert!(error.message.contains("exceeded"), "unexpected error: {error}");
    }

    #[test]
    fn an_enumerator_that_ends_with_s_ok_is_a_driver_error() {
        // `S_OK` promises the requested element was delivered, so an empty `S_OK`
        // is a contract violation, not the end of a shorter collection.
        let mock = MockCollection {
            items: vec![Element::Str("a"), Element::Str("b")],
            new_enum: NewEnumMode::OkAtEnd,
            item_base: ItemBase::Absent,
            hide_count: true,
            bogus_count: None,
        };
        let (_keep, handle) = collection(mock);
        let error = elements(&handle)
            .map(|items| items.len())
            .expect_err("a short S_OK must not look like a complete collection");
        assert_eq!(error.kind, AscomErrorKind::Driver, "unexpected error: {error}");
    }

    #[test]
    fn a_new_enum_refused_by_the_device_is_not_fallback_bait() {
        // NotConnected is the device talking, not "this driver has no enumerator";
        // swallowing it would let a dead device fall back to a second read. The mock
        // reports it the way a .NET server does, so the code has to come out of
        // `EXCEPINFO.scode` for this to classify at all.
        let mock = MockCollection {
            items: vec![Element::Str("a")],
            new_enum: NewEnumMode::Failing(ascom_hr(0x407)),
            item_base: ItemBase::Zero,
            hide_count: false,
            bogus_count: None,
        };
        let (_keep, handle) = collection(mock);
        let error = elements(&handle)
            .map(|items| items.len())
            .expect_err("a binding failure must not be traded for the fallback");
        assert_eq!(error.kind, AscomErrorKind::NotConnected, "unexpected error: {error}");
    }

    // --- the Count/Item path ---------------------------------------------

    #[test]
    fn a_nonsense_count_is_rejected_not_truncated() {
        // The enumerator path stops hard at the guard; `Count` must not clamp a lie
        // into a partial collection that reads as a complete one.
        for count in [MAX_COLLECTION as i32 + 1, i32::MAX] {
            let mock = MockCollection {
                items: vec![Element::Str("a")],
                new_enum: NewEnumMode::Absent,
                item_base: ItemBase::Zero,
                hide_count: false,
                bogus_count: Some(count),
            };
            let (_keep, handle) = collection(mock);
            let error = elements(&handle)
                .map(|items| items.len())
                .expect_err("an impossible Count must be rejected");
            assert_eq!(error.kind, AscomErrorKind::Driver, "unexpected error: {error}");
            assert!(error.message.contains("exceeded"), "unexpected error: {error}");
        }
    }

    #[test]
    fn count_and_item_is_used_when_new_enum_is_absent() {
        let mock = MockCollection::indexed(
            vec![Element::Str("a"), Element::Str("b"), Element::Str("c")],
            ItemBase::Zero,
        );
        let (_keep, handle) = collection(mock);
        assert_eq!(as_strings(elements(&handle).unwrap()), ["a", "b", "c"]);
    }

    #[test]
    fn the_index_base_is_probed_not_assumed() {
        // A VB-style `Collection`: `Item(0)` fails, `Item(1..=n)` works.
        let mock = MockCollection::indexed(
            vec![Element::Str("one"), Element::Str("two")],
            ItemBase::One,
        );
        let (_keep, handle) = collection(mock);
        assert_eq!(as_strings(elements(&handle).unwrap()), ["one", "two"]);
    }

    #[test]
    fn a_broken_enumerator_falls_back_to_count_and_item() {
        // The enumerator delivers one element and then fails; getting all three
        // items back proves the fallback restarted from scratch rather than resumed.
        let mock = MockCollection {
            items: vec![Element::Str("x"), Element::Str("y"), Element::Str("z")],
            new_enum: NewEnumMode::Broken,
            item_base: ItemBase::Zero,
            hide_count: false,
            bogus_count: None,
        };
        let (_keep, handle) = collection(mock);
        assert_eq!(as_strings(elements(&handle).unwrap()), ["x", "y", "z"]);
    }

    #[test]
    fn a_new_enum_refused_as_not_implemented_still_falls_back() {
        // A plain COM object refuses the hidden member with `E_NOTIMPL` or
        // `DISP_E_MEMBERNOTFOUND` as the `Invoke` HRESULT; ASCOM's
        // `PropertyNotImplementedException` arrives as `DISP_E_EXCEPTION` with the
        // code in `EXCEPINFO.scode`. All three mean "no enumerator here".
        for (label, mode) in [
            ("E_NOTIMPL", NewEnumMode::FailingRaw(E_NOTIMPL)),
            ("DISP_E_MEMBERNOTFOUND", NewEnumMode::FailingRaw(DISP_E_MEMBERNOTFOUND)),
            ("ASCOM 0x400 via EXCEPINFO", NewEnumMode::Failing(ascom_hr(0x400))),
        ] {
            let mock = MockCollection {
                items: vec![Element::Str("a"), Element::Str("b")],
                new_enum: mode,
                item_base: ItemBase::Zero,
                hide_count: false,
                bogus_count: None,
            };
            let (_keep, handle) = collection(mock);
            assert_eq!(
                as_strings(elements(&handle).expect("fallback enumeration")),
                ["a", "b"],
                "{label} must not fail the read"
            );
        }
    }

    #[test]
    fn a_new_enum_that_is_not_an_enumerator_falls_back() {
        let mock = MockCollection {
            items: vec![Element::Int(7), Element::Int(9)],
            new_enum: NewEnumMode::NotAnEnumerator,
            item_base: ItemBase::Zero,
            hide_count: false,
            bogus_count: None,
        };
        let (_keep, handle) = collection(mock);
        let items = elements(&handle).expect("fallback enumeration");
        let values: Vec<i32> = items.iter().map(|v| v.as_i32().unwrap()).collect();
        assert_eq!(values, [7, 9]);
    }

    #[test]
    fn a_collection_exposing_nothing_is_reported_as_an_error() {
        let mock = MockCollection::opaque(vec![Element::Str("unreachable")]);
        let (_keep, handle) = collection(mock);
        assert!(elements(&handle).is_err(), "no enumeration path must not look like an empty list");
    }

    // --- the typed wrappers ----------------------------------------------

    #[test]
    fn strings_reads_a_collection_of_bstrs() {
        let got = strings_of(&[Element::Str("Gain 100"), Element::Str("Gain 400")]).unwrap();
        assert_eq!(got, ["Gain 100", "Gain 400"]);
    }

    #[test]
    fn a_hole_in_a_string_collection_is_an_error_on_both_paths() {
        // `Names` is read by slot number, so dropping a hole would silently misalign
        // every later name; the `Item` path already refuses one, so both must fail.
        for mock in [
            MockCollection::new(vec![Element::Str("a"), Element::Empty, Element::Str("b")]),
            MockCollection::indexed(
                vec![Element::Str("a"), Element::Empty, Element::Str("b")],
                ItemBase::Zero,
            ),
        ] {
            let error = strings(&mock.into_variant())
                .map(|items| items.len())
                .expect_err("a hole must not shorten the list");
            assert!(
                matches!(error.kind, AscomErrorKind::Com | AscomErrorKind::ValueNotSet),
                "unexpected error: {error}"
            );
        }
    }

    #[test]
    fn a_hole_in_an_integer_collection_is_an_error() {
        for mock in [
            MockCollection::new(vec![Element::Int(1), Element::Empty, Element::Int(3)]),
            MockCollection::indexed(
                vec![Element::Int(1), Element::Empty, Element::Int(3)],
                ItemBase::Zero,
            ),
        ] {
            let error = ints(&mock.into_variant())
                .map(|items| items.len())
                .expect_err("a hole must not shorten the list");
            assert!(
                matches!(error.kind, AscomErrorKind::Com | AscomErrorKind::ValueNotSet),
                "unexpected error: {error}"
            );
        }
    }

    #[test]
    fn objects_refuses_non_object_elements_instead_of_dropping_them() {
        // An empty `Vec` here is what `AxisRates` reports as "this axis has no
        // rates", so a collection of scalars must fail rather than come back empty.
        for mock in [
            MockCollection::new(vec![Element::Str("sidereal"), Element::Str("lunar")]),
            MockCollection::indexed(vec![Element::Int(0), Element::Int(1)], ItemBase::Zero),
        ] {
            let error = objects(&mock.into_variant())
                .map(|rates| rates.len())
                .expect_err("scalars are not Rate objects");
            assert_eq!(error.kind, AscomErrorKind::Com, "unexpected error: {error}");
        }
    }

    #[test]
    fn strings_accepts_a_lone_string_or_a_bstr_array() {
        assert_eq!(strings(&Variant::from_str("solo")).unwrap(), ["solo"]);
        assert_eq!(strings(&Variant::from_str("")).unwrap(), [""], "one empty string, not none");
        assert_eq!(bstr_array(&["u", "g", "r", "i"]).kind(), VariantKind::Array);
        assert_eq!(strings(&bstr_array(&["u", "g", "r", "i"])).unwrap(), ["u", "g", "r", "i"]);
        assert_eq!(bstr_array(&[]).kind(), VariantKind::Array);
        assert!(strings(&bstr_array(&[])).unwrap().is_empty());
    }

    #[test]
    fn ints_reads_a_collection_of_integers() {
        let variant = MockCollection::new(vec![Element::Int(-2), Element::Int(3)]).into_variant();
        assert_eq!(ints(&variant).unwrap(), [-2, 3]);
    }

    /// A bare `VT_EMPTY`/`VT_NULL` where a collection was declared is the absence of an
    /// answer, not "this device has nothing to list": a driver that returned nothing
    /// must not look like a device with no entries.
    #[test]
    fn an_absent_collection_answer_is_an_error_not_an_empty_list() {
        for (vt, name) in [(VT_EMPTY, "VT_EMPTY"), (VT_NULL, "VT_NULL")] {
            // Neither variant carries a payload, so setting only `vt` is honest; the
            // field chain is a union, hence the same address cast `variant.rs` uses.
            let mut raw = VARIANT::default();
            unsafe {
                let h = &mut *(core::ptr::addr_of_mut!(raw).cast::<VARIANT_0_0>());
                h.vt = vt;
            }
            let value = unsafe { Variant::from_raw(raw) };
            for outcome in [
                strings(&value).map(|items| items.len()),
                ints(&value).map(|items| items.len()),
                objects(&value).map(|items| items.len()),
            ] {
                let error = outcome
                    .err()
                    .unwrap_or_else(|| panic!("{name} must not read as an empty list"));
                assert_eq!(error.kind, AscomErrorKind::ValueNotSet, "{name}: {error}");
                assert!(
                    error.message.contains(name),
                    "{name} is lost from the message: {error}"
                );
            }
        }
    }

    /// The honest "nothing here": a real collection object with zero elements, which is
    /// what a driver with no actions, rates or names answers.
    #[test]
    fn an_empty_collection_object_is_still_an_empty_list() {
        let empty = MockCollection::new(Vec::new()).into_variant();
        assert!(strings(&empty).expect("an empty collection is an answer").is_empty());
        assert!(ints(&empty).expect("an empty collection is an answer").is_empty());
        assert!(objects(&empty).expect("an empty collection is an answer").is_empty());
    }

    #[test]
    fn a_mismatched_variant_is_a_type_error_not_a_panic() {
        let number = Variant::from_f64(1.5);
        assert_eq!(strings(&number).err().map(|e| e.kind), Some(AscomErrorKind::Com));
        assert_eq!(ints(&number).err().map(|e| e.kind), Some(AscomErrorKind::Com));
        assert_eq!(objects(&number).err().map(|e| e.kind), Some(AscomErrorKind::Com));
    }
}
