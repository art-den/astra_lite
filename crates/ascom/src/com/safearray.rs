//! SAFEARRAY access.
//!
//! Three rules from the ASCOM COM spec drive this module:
//! 1. Bounds are read **only** through `SafeArrayGetLBound`/`SafeArrayGetUBound` per
//!    dimension — legacy VB drivers hand back 1-based arrays.
//! 2. The element type comes from `SafeArrayGetVartype`, never from "what we called".
//! 3. SAFEARRAY memory is **dimension-1 major**: the first declared dimension (the one
//!    `rgsabound[0]` describes) varies fastest — verified experimentally on this
//!    machine (see `one_based_array_memory_order_is_dim1_fastest`). A .NET driver
//!    declaring `Array[NumX, NumY]` (row-major, Y fastest) marshals to a SAFEARRAY
//!    with the dimensions *reversed*: dim1 = NumY, dim2 = NumX. The ASCOM app-view
//!    index `idx = x * NumY + y` therefore matches the raw buffer, and `dims[0]` is
//!    the **height**, not the width.

use std::ffi::c_void;

use windows::Win32::System::Com::SAFEARRAY;
use windows::Win32::System::Ole::{
    SafeArrayAccessData, SafeArrayGetDim, SafeArrayGetElement, SafeArrayGetElemsize,
    SafeArrayGetLBound, SafeArrayGetUBound, SafeArrayGetVartype, SafeArrayUnaccessData,
};
use windows::Win32::System::Variant::{
    VT_BSTR, VT_DISPATCH, VT_EMPTY, VT_I1, VT_I2, VT_I4, VT_R4, VT_R8, VT_UI1, VT_UI2, VT_UI4,
    VT_UNKNOWN, VT_VARIANT, VARENUM, VARIANT,
};

use crate::com::fail;
use crate::com::variant::{float_as_i32, narrow_i32, Variant};
use crate::error::{AscomError, AscomErrorKind, Result};

/// One dimension of a SAFEARRAY, as reported by the array itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Dim {
    pub lower: i32,
    pub upper: i32,
}

impl Dim {
    /// The span in `i64`: driver-supplied bounds can be wider than `i32` can hold.
    fn span(&self) -> i64 {
        i64::from(self.upper) - i64::from(self.lower) + 1
    }

    /// Elements in this dimension; `upper < lower` counts as empty.
    ///
    /// Saturates on bounds too wide to address; `len_checked` is the fallible
    /// form and everything that sizes a buffer uses it.
    pub fn len(&self) -> usize {
        usize::try_from(i64::max(self.span(), 0)).unwrap_or(usize::MAX)
    }

    /// [`Dim::len`] that refuses bounds too wide to address instead of saturating.
    fn len_checked(&self) -> Result<usize> {
        usize::try_from(self.span()).map_err(|_| bogus_shape("dimension is too wide to address"))
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn contains(&self, index: i32) -> bool {
        index >= self.lower && index <= self.upper
    }
}

/// A shape the driver described but no buffer could hold.
fn bogus_shape(detail: impl Into<String>) -> AscomError {
    AscomError::local(AscomErrorKind::Driver, "SafeArray", detail)
}

/// Product of the dimension lengths, refusing a shape too wide to count.
///
/// The count is later used both as a slice length and as an allocation size, so a
/// wrapped product would mean an out-of-bounds read or an absurd allocation.
fn dim_product(dims: &[Dim]) -> Result<usize> {
    dims.iter().try_fold(1usize, |total, dim| {
        let len = dim.len_checked()?;
        len.checked_mul(total)
            .ok_or_else(|| bogus_shape("element count is too large to address"))
    })
}

/// Element types whose `SafeArrayGetElement` copy owns a reference: for a string,
/// object or variant element the function copies the element "in the correct way",
/// which means the caller receives a value it must release.
fn carries_a_reference(vt: VARENUM) -> bool {
    matches!(vt, VT_BSTR | VT_VARIANT | VT_UNKNOWN | VT_DISPATCH)
}

/// A locked, accessed SAFEARRAY borrowed from a [`Variant`].
///
/// Sound because the owning `Variant` outlives the view: the borrow checker enforces
/// that through the `&Variant` parameter of [`SafeArrayView::from_variant`].
pub struct SafeArrayView<'v> {
    psa: *mut SAFEARRAY,
    data: *mut c_void,
    vt: VARENUM,
    elem_size: usize,
    dims: Vec<Dim>,
    _borrow: std::marker::PhantomData<&'v Variant>,
}

/// Undoes `SafeArrayAccessData` when `from_variant` gives up before a view exists.
///
/// `Drop` of the view itself cannot help there — `Self` was never constructed — so
/// without this guard a failed shape read (or a panic in it) would leave a driver's
/// array locked for the rest of the process.
struct AccessGuard {
    psa: *mut SAFEARRAY,
}

impl Drop for AccessGuard {
    fn drop(&mut self) {
        unsafe {
            let _ = SafeArrayUnaccessData(self.psa);
        }
    }
}

impl<'v> SafeArrayView<'v> {
    /// Takes the data pointer and reads shape and element type.
    ///
    /// Lock discipline: `SafeArrayAccessData` documents that it *increments the lock
    /// count* and that `SafeArrayUnaccessData` is the matching release, so it is the
    /// one and only lock a view takes. Adding `SafeArrayLock` on top of it would raise
    /// `cLocks` by 2 and lower it by 1, and a driver that hands back a cached array
    /// would meet `DISP_E_ARRAYISLOCKED` the next time it reshapes or frees it.
    pub fn from_variant(v: &'v Variant) -> Result<Self> {
        let psa: *mut SAFEARRAY = v.array_ptr()?.cast();
        let vt = unsafe { SafeArrayGetVartype(psa) }
            .map_err(|e| fail(e.code().0, "SafeArrayGetVartype"))?;

        let mut data: *mut c_void = core::ptr::null_mut();
        if let Err(e) = unsafe { SafeArrayAccessData(psa, &mut data) } {
            // A failed access took no lock, so there is nothing to release: calling
            // `SafeArrayUnaccessData` here would drop a lock the driver holds itself.
            return Err(fail(e.code().0, "SafeArrayAccessData"));
        }
        // From here the array is accessed and `Drop` of the view cannot run yet.
        let guard = AccessGuard { psa };

        let dims = (1..=unsafe { SafeArrayGetDim(psa) })
            .map(|ndim| {
                let lower = unsafe { SafeArrayGetLBound(psa, ndim) }
                    .map_err(|e| fail(e.code().0, "SafeArrayGetLBound"))?;
                let upper = unsafe { SafeArrayGetUBound(psa, ndim) }
                    .map_err(|e| fail(e.code().0, "SafeArrayGetUBound"))?;
                Ok(Dim { lower, upper })
            })
            .collect::<Result<Vec<Dim>>>()?;

        let elem_size = unsafe { SafeArrayGetElemsize(psa) } as usize;
        // The view takes the unlock over from the guard.
        core::mem::forget(guard);

        Ok(Self {
            psa,
            data,
            vt,
            elem_size,
            dims,
            _borrow: std::marker::PhantomData,
        })
    }

    pub fn dims(&self) -> &[Dim] {
        &self.dims
    }

    pub fn ndim(&self) -> u32 {
        self.dims.len() as u32
    }

    pub fn vartype(&self) -> VARENUM {
        self.vt
    }

    pub fn elem_size(&self) -> usize {
        self.elem_size
    }

    /// Total number of elements the bounds describe.
    ///
    /// An error rather than a wrapped product: this length is what the bulk readers
    /// hand to `slice::from_raw_parts`.
    pub fn element_count(&self) -> Result<usize> {
        dim_product(&self.dims)
    }

    /// Linear memory position of `indices`, honouring each dimension's lower bound.
    ///
    /// SAFEARRAY memory varies dimension 1 fastest, so the stride accumulates
    /// forward: `pos = i1 + i2 * n1 + i3 * n1 * n2`.
    pub fn linear_index(&self, indices: &[i32]) -> Result<usize> {
        if indices.len() != self.dims.len() {
            return Err(AscomError::local(
                AscomErrorKind::Com,
                "SafeArray",
                format!("expected {} indices, got {}", self.dims.len(), indices.len()),
            ));
        }
        let mut pos = 0usize;
        let mut stride = 1usize;
        for (dim, &index) in self.dims.iter().zip(indices) {
            if !dim.contains(index) {
                return Err(AscomError::local(
                    AscomErrorKind::InvalidValue,
                    "SafeArray",
                    format!("index {index} outside [{}, {}]", dim.lower, dim.upper),
                ));
            }
            // `contains` guarantees a non-negative offset; `i64` keeps absurd bounds
            // from overflowing the subtraction itself.
            let offset = usize::try_from(i64::from(index) - i64::from(dim.lower))
                .map_err(|_| bogus_shape("index offset is too large to address"))?;
            pos = offset
                .checked_mul(stride)
                .and_then(|at| pos.checked_add(at))
                .ok_or_else(|| bogus_shape("linear index is too large to address"))?;
            stride = dim
                .len_checked()?
                .checked_mul(stride)
                .ok_or_else(|| bogus_shape("stride is too large to address"))?;
        }
        Ok(pos)
    }

    /// Typed view over the accessed buffer, for homogeneous element types.
    ///
    /// The `VARENUM` has to match as well as the element size: an `i32` slice over a
    /// `VT_R4` array is exactly as wide and completely wrong in value (rule 2).
    fn slice_as<T>(&self, expected: VARENUM) -> Result<&[T]> {
        if self.vt != expected {
            return Err(AscomError::vartype("SafeArray element type", self.vt.0));
        }
        if self.data.is_null() {
            return Err(AscomError::local(AscomErrorKind::Com, "SafeArray", "null data pointer"));
        }
        let size = core::mem::size_of::<T>();
        if self.elem_size != size {
            return Err(AscomError::type_mismatch(
                "SafeArray",
                format!("element size {} is not {size}", self.elem_size),
            ));
        }
        let len = self.element_count()?;
        Ok(unsafe { core::slice::from_raw_parts(self.data.cast::<T>(), len) })
    }

    /// One element through the per-element path. Correct for any element whose value
    /// is exactly `T`; the bulk readers above are preferred.
    ///
    /// `SafeArrayGetElement` documents that the caller must provide storage of the
    /// correct size, so a `T` the size of which differs from the array's element size
    /// is refused instead of letting the call write past a stack slot. Element types
    /// that carry a reference are refused too — the copy is ours to release and this
    /// function has nowhere to do it, so use [`variant_element`](Self::variant_element)
    /// or [`strings`](Self::strings) for those.
    pub fn element<T: Copy + Default>(&self, indices: &[i32]) -> Result<T> {
        if carries_a_reference(self.vt) {
            return Err(AscomError::type_mismatch(
                "SafeArray",
                format!(
                    "VARENUM({}) elements arrive as an owning copy; use variant_element or strings",
                    self.vt.0
                ),
            ));
        }
        let size = core::mem::size_of::<T>();
        if self.elem_size != size {
            return Err(AscomError::type_mismatch(
                "SafeArray",
                format!("element size {} is not {size}", self.elem_size),
            ));
        }
        self.linear_index(indices)?; // bounds check; GetElement takes logical indices
        let mut out = T::default();
        let mut idx: Vec<i32> = indices.to_vec();
        unsafe {
            SafeArrayGetElement(self.psa, idx.as_mut_ptr(), core::ptr::addr_of_mut!(out).cast::<c_void>())
                .map_err(|e| fail(e.code().0, "SafeArrayGetElement"))?;
        }
        Ok(out)
    }

    /// A `VT_VARIANT` element, taken by value: the copy is ours to clear.
    ///
    /// The index is checked here first, so a bad index of ours stays an
    /// `InvalidValue` instead of arriving as the binding layer's `DISP_E_BADINDEX`;
    /// it also keeps a wrong-arity slice away from `rgIndices`, which oleaut32 reads
    /// `cDims` entries deep.
    pub fn variant_element(&self, indices: &[i32]) -> Result<Variant> {
        if self.vt != VT_VARIANT {
            return Err(AscomError::vartype("VT_VARIANT element", self.vt.0));
        }
        self.linear_index(indices)?;
        let mut raw = VARIANT::default();
        let mut idx: Vec<i32> = indices.to_vec();
        unsafe {
            SafeArrayGetElement(
                self.psa,
                idx.as_mut_ptr(),
                core::ptr::addr_of_mut!(raw).cast::<c_void>(),
            )
            .map_err(|e| fail(e.code().0, "SafeArrayGetElement"))?;
        }
        Ok(unsafe { Variant::from_raw(raw) })
    }

    // Bulk readers -----------------------------------------------------------

    pub fn i16s(&self) -> Result<&[i16]> {
        self.slice_as::<i16>(VT_I2)
    }

    pub fn i32s(&self) -> Result<&[i32]> {
        self.slice_as::<i32>(VT_I4)
    }

    pub fn f32s(&self) -> Result<&[f32]> {
        self.slice_as::<f32>(VT_R4)
    }

    pub fn f64s(&self) -> Result<&[f64]> {
        self.slice_as::<f64>(VT_R8)
    }

    /// The strings of a `VT_ARRAY | VT_BSTR`, read straight out of the array's own
    /// cells rather than one `SafeArrayGetElement` at a time.
    ///
    /// No BSTR is duplicated by this reader, so nothing here needs `SysFreeString`
    /// (the array owns its cells); the returned `String`s are, of course, owned
    /// copies of them.
    pub fn strings(&self) -> Result<Vec<String>> {
        if self.vt != VT_BSTR {
            return Err(AscomError::vartype("VT_ARRAY|VT_BSTR", self.vt.0));
        }
        Ok(self.slice_as::<windows::core::BSTR>(VT_BSTR)?
            .iter()
            .map(ToString::to_string)
            .collect())
    }
}

impl Drop for SafeArrayView<'_> {
    fn drop(&mut self) {
        // The single lock `from_variant` took through `SafeArrayAccessData`.
        unsafe {
            let _ = SafeArrayUnaccessData(self.psa);
        }
    }
}

/// `VT_ARRAY | VT_BSTR` → `Vec<String>`, flattened in declared-dimension order.
///
/// An absent list (`VT_EMPTY`) is not an error: a driver may legitimately expose no
/// `Gains`/`SupportedActions` at all.
pub fn read_strings(v: &Variant) -> Result<Vec<String>> {
    if v.vt() == VT_EMPTY {
        return Ok(Vec::new());
    }
    let view = SafeArrayView::from_variant(v)?;
    if view.vartype() == VT_BSTR {
        return view.strings();
    }
    // Some drivers expose their name lists as numbers; stringify rather than fail.
    let mut out = Vec::with_capacity(view.element_count()?);
    for pos in flat_positions(view.dims())? {
        out.push(match view.vartype() {
            VT_I1 => i64::from(view.element::<i8>(&pos)?).to_string(),
            VT_I2 => i64::from(view.element::<i16>(&pos)?).to_string(),
            VT_I4 => i64::from(view.element::<i32>(&pos)?).to_string(),
            VT_R4 => view.element::<f32>(&pos)?.to_string(),
            VT_R8 => view.element::<f64>(&pos)?.to_string(),
            VT_VARIANT => view.variant_element(&pos)?.describe(),
            other => return Err(AscomError::vartype("string list", other.0)),
        });
    }
    Ok(out)
}

/// `VT_ARRAY | VT_I2/I4/...` → `Vec<i32>` (`TrackingRates` when it arrives as an array).
pub fn read_ints(v: &Variant) -> Result<Vec<i32>> {
    if v.vt() == VT_EMPTY {
        return Ok(Vec::new());
    }
    let view = SafeArrayView::from_variant(v)?;
    let mut out = Vec::with_capacity(view.element_count()?);
    for pos in flat_positions(view.dims())? {
        out.push(match view.vartype() {
            VT_I1 => i32::from(view.element::<i8>(&pos)?),
            VT_I2 => i32::from(view.element::<i16>(&pos)?),
            VT_I4 => view.element::<i32>(&pos)?,
            VT_UI1 => i32::from(view.element::<u8>(&pos)?),
            VT_UI2 => i32::from(view.element::<u16>(&pos)?),
            // Narrowing goes through the same checks as `Variant::as_i32`: these
            // values become rates, and `as i32` would wrap 3_000_000_000 onto a
            // negative one and truncate 1.5 onto 1.
            VT_UI4 => narrow_i32(i64::from(view.element::<u32>(&pos)?))?,
            VT_R4 => float_as_i32(f64::from(view.element::<f32>(&pos)?))?,
            VT_R8 => float_as_i32(view.element::<f64>(&pos)?)?,
            VT_VARIANT => view.variant_element(&pos)?.as_i32()?,
            other => return Err(AscomError::vartype("integer list", other.0)),
        });
    }
    Ok(out)
}

/// Every index tuple of `dims`, in memory order (dimension 1 varies fastest).
pub fn flat_positions(dims: &[Dim]) -> Result<Vec<Vec<i32>>> {
    let total = dim_product(dims)?;
    let mut out = Vec::with_capacity(total);
    let mut current: Vec<i32> = dims.iter().map(|dim| dim.lower).collect();
    for _ in 0..total {
        out.push(current.clone());
        // Odometer with carry starting at dimension 1 (the fastest one).
        for depth in 0..dims.len() {
            if current[depth] < dims[depth].upper {
                current[depth] += 1;
                break;
            }
            current[depth] = dims[depth].lower;
        }
    }
    Ok(out)
}

/// Test-only SAFEARRAY builders.
///
/// They live here (inside `com`) rather than in the modules that use them, so that
/// `unsafe` stays out of the public device modules even in their tests.
#[cfg(test)]
pub(crate) mod test_support {
    use super::*;
    use windows::Win32::System::Com::SAFEARRAYBOUND;
    use windows::Win32::System::Ole::{SafeArrayCreate, SafeArrayPutElement};
    use windows::Win32::System::Variant::{VT_I2, VT_R4};

    /// 2D `VT_I2` array laid out exactly like a marshaled .NET `Array[nx, ny]`:
    /// SAFEARRAY dim1 is the *app-view rightmost* dimension (`y`), so the raw buffer
    /// reads `x * ny + y`, matching the ASCOM spec's app-view index formula.
    pub(crate) fn test_i16_2d(
        lower_x: i32,
        lower_y: i32,
        nx: i32,
        ny: i32,
        fill: impl Fn(i32, i32) -> i16,
    ) -> Variant {
        let bounds = [
            SAFEARRAYBOUND { cElements: ny.max(0) as u32, lLbound: lower_y },
            SAFEARRAYBOUND { cElements: nx.max(0) as u32, lLbound: lower_x },
        ];
        let psa = unsafe { SafeArrayCreate(VT_I2, 2, bounds.as_ptr()) };
        assert!(!psa.is_null());
        for x in lower_x..lower_x + nx {
            for y in lower_y..lower_y + ny {
                let value = fill(x, y);
                let idx = [y, x];
                unsafe {
                    SafeArrayPutElement(psa, idx.as_ptr(), core::ptr::addr_of!(value).cast::<c_void>())
                        .expect("SafeArrayPutElement");
                }
            }
        }
        unsafe { Variant::from_owned_array(psa, VT_I2) }
    }

    /// The other orientation: `dim1 == NumX`, i.e. ordinary row-major, which is what
    /// the OmniSim camera really returns (verified live).
    pub(crate) fn test_i16_2d_row_major(
        lower_x: i32,
        lower_y: i32,
        nx: i32,
        ny: i32,
        fill: impl Fn(i32, i32) -> i16,
    ) -> Variant {
        let bounds = [
            SAFEARRAYBOUND { cElements: nx.max(0) as u32, lLbound: lower_x },
            SAFEARRAYBOUND { cElements: ny.max(0) as u32, lLbound: lower_y },
        ];
        let psa = unsafe { SafeArrayCreate(VT_I2, 2, bounds.as_ptr()) };
        assert!(!psa.is_null());
        for x in lower_x..lower_x + nx {
            for y in lower_y..lower_y + ny {
                let value = fill(x, y);
                let idx = [x, y];
                unsafe {
                    SafeArrayPutElement(psa, idx.as_ptr(), core::ptr::addr_of!(value).cast::<c_void>())
                        .expect("SafeArrayPutElement");
                }
            }
        }
        unsafe { Variant::from_owned_array(psa, VT_I2) }
    }

    /// 1D `VT_I2` array, to assert that a frame with too few dimensions is rejected.
    pub(crate) fn test_i16_1d(len: i32, fill: impl Fn(i32) -> i16) -> Variant {
        let bounds = [SAFEARRAYBOUND { cElements: len.max(0) as u32, lLbound: 0 }];
        let psa = unsafe { SafeArrayCreate(VT_I2, 1, bounds.as_ptr()) };
        assert!(!psa.is_null());
        for x in 0..len {
            let value = fill(x);
            let idx = [x];
            unsafe {
                SafeArrayPutElement(psa, idx.as_ptr(), core::ptr::addr_of!(value).cast::<c_void>())
                    .expect("SafeArrayPutElement");
            }
        }
        unsafe { Variant::from_owned_array(psa, VT_I2) }
    }

    /// 3D `VT_R4` array built like a marshaled .NET `Array[nx, ny, np]`
    /// (width, height, plane): SAFEARRAY dims are reversed, plane fastest.
    pub(crate) fn test_r4_3d(
        lower_x: i32,
        lower_y: i32,
        lower_p: i32,
        nx: i32,
        ny: i32,
        np: i32,
        fill: impl Fn(i32, i32, i32) -> f32,
    ) -> Variant {
        let bounds = [
            SAFEARRAYBOUND { cElements: np.max(0) as u32, lLbound: lower_p },
            SAFEARRAYBOUND { cElements: ny.max(0) as u32, lLbound: lower_y },
            SAFEARRAYBOUND { cElements: nx.max(0) as u32, lLbound: lower_x },
        ];
        let psa = unsafe { SafeArrayCreate(VT_R4, 3, bounds.as_ptr()) };
        assert!(!psa.is_null());
        for x in lower_x..lower_x + nx {
            for y in lower_y..lower_y + ny {
                for p in lower_p..lower_p + np {
                    let value = fill(x, y, p);
                    let idx = [p, y, x];
                    unsafe {
                        SafeArrayPutElement(
                            psa,
                            idx.as_ptr(),
                            core::ptr::addr_of!(value).cast::<c_void>(),
                        )
                        .expect("SafeArrayPutElement");
                    }
                }
            }
        }
        unsafe { Variant::from_owned_array(psa, VT_R4) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::System::Com::SAFEARRAYBOUND;
    use windows::Win32::System::Ole::{SafeArrayCreate, SafeArrayPutElement};
    use windows::Win32::System::Variant::VT_R4;

    /// Wraps a freshly created SAFEARRAY in a `Variant`, which takes ownership and
    /// destroys it in its own `Drop` (`VariantClear` on `VT_ARRAY | ...`).
    fn own_array(psa: *mut SAFEARRAY, element: VARENUM) -> Variant {
        unsafe { Variant::from_owned_array(psa, element) }
    }

    /// Builds a 2D SAFEARRAY with the given lower bounds, filled with `x * 100 + y`,
    /// so any index mix-up is immediately visible.
    fn make_2d_16(lb_x: i32, lb_y: i32, nx: i32, ny: i32) -> *mut SAFEARRAY {
        let bounds = [
            SAFEARRAYBOUND { cElements: nx as u32, lLbound: lb_x },
            SAFEARRAYBOUND { cElements: ny as u32, lLbound: lb_y },
        ];
        let psa = unsafe { SafeArrayCreate(VT_I2, 2, bounds.as_ptr()) };
        assert!(!psa.is_null());
        for x in lb_x..lb_x + nx {
            for y in lb_y..lb_y + ny {
                let mut idx = [x, y];
                let value: i16 = i16::try_from(x * 100 + y).unwrap();
                unsafe {
                    SafeArrayPutElement(psa, idx.as_mut_ptr(), core::ptr::addr_of!(value).cast::<c_void>())
                        .expect("PutElement");
                }
            }
        }
        psa
    }

    #[test]
    fn bounds_come_from_the_array_not_from_assumptions() {
        let v = own_array(make_2d_16(1, 1, 3, 2), VT_I2);
        let view = SafeArrayView::from_variant(&v).unwrap();
        assert_eq!(view.ndim(), 2);
        assert_eq!(view.vartype(), VT_I2);
        assert_eq!(view.dims(), &[Dim { lower: 1, upper: 3 }, Dim { lower: 1, upper: 2 }]);
        assert_eq!(view.element_count().unwrap(), 6);
    }

    #[test]
    fn one_based_array_memory_order_is_dim1_fastest() {
        let v = own_array(make_2d_16(1, 1, 3, 2), VT_I2);
        let view = SafeArrayView::from_variant(&v).unwrap();
        // Verified against oleaut32 on this machine: rgsabound[0] (dim 1) varies
        // fastest, so (1,1),(2,1),(3,1),(1,2),(2,2),(3,2) — *not* the .NET row-major
        // order, which is exactly why a .NET `Array[x, y]` marshals with reversed dims.
        assert_eq!(view.i16s().unwrap(), &[101, 201, 301, 102, 202, 302]);
        assert_eq!(view.linear_index(&[1, 1]).unwrap(), 0);
        assert_eq!(view.linear_index(&[3, 2]).unwrap(), 5);
        assert_eq!(view.linear_index(&[2, 1]).unwrap(), 1);
        assert_eq!(view.linear_index(&[1, 2]).unwrap(), 3);
        assert_eq!(view.element::<i16>(&[2, 1]).unwrap(), 201);
    }

    #[test]
    fn zero_based_arrays_work_the_same_way() {
        let v = own_array(make_2d_16(0, 0, 2, 4), VT_I2);
        let view = SafeArrayView::from_variant(&v).unwrap();
        assert_eq!(view.i16s().unwrap().len(), 8);
        assert_eq!(view.element::<i16>(&[0, 0]).unwrap(), 0);
        assert_eq!(view.element::<i16>(&[1, 3]).unwrap(), 103);
        assert!(view.linear_index(&[2, 0]).is_err(), "index above upper bound");
        assert!(view.linear_index(&[0, -1]).is_err(), "index below lower bound");
        assert!(view.linear_index(&[0]).is_err(), "wrong arity");
    }

    #[test]
    fn element_type_is_read_not_assumed() {
        let bounds = [
            SAFEARRAYBOUND { cElements: 1, lLbound: 0 },
            SAFEARRAYBOUND { cElements: 1, lLbound: 0 },
        ];
        let psa = unsafe { SafeArrayCreate(VT_R4, 2, bounds.as_ptr()) };
        let v = own_array(psa, VT_R4);
        let view = SafeArrayView::from_variant(&v).unwrap();
        assert_eq!(view.vartype(), VT_R4);
        assert_eq!(view.elem_size(), 4);
        // Reading an R4 array as i16 must fail loudly, not silently mis-read pixels.
        assert!(view.i16s().is_err());
    }

    #[test]
    fn non_array_variant_is_rejected() {
        assert!(SafeArrayView::from_variant(&Variant::from_i32(1)).is_err());
    }

    #[test]
    fn flat_positions_is_dim1_fastest() {
        let dims = [Dim { lower: 1, upper: 2 }, Dim { lower: 0, upper: 1 }];
        assert_eq!(
            flat_positions(&dims).unwrap(),
            vec![vec![1, 0], vec![2, 0], vec![1, 1], vec![2, 1]]
        );
    }

    #[test]
    fn string_arrays_are_read_without_leaking() {
        let bounds = [SAFEARRAYBOUND { cElements: 2, lLbound: 0 }];
        let psa = unsafe { SafeArrayCreate(VT_BSTR, 1, bounds.as_ptr()) };
        for (i, text) in ["High", "Low"].into_iter().enumerate() {
            // Verified on this machine's oleaut32: for VT_BSTR the `pv` argument is
            // used *as the BSTR* (not as a pointer to one) and the array stores its
            // own duplicate. Passing `&bstr` instead yields empty cells, which is the
            // trap this test documents.
            let bstr = windows::core::BSTR::from(text);
            let chars: *const u16 = (&*bstr).as_ptr();
            let idx = [i as i32];
            unsafe {
                SafeArrayPutElement(psa, idx.as_ptr(), chars.cast::<c_void>())
                    .expect("PutElement bstr")
            };
            // `bstr` drops here; the array owns its duplicate, `VariantClear` frees it.
        }
        let v = own_array(psa, VT_BSTR);
        assert_eq!(read_strings(&v).unwrap(), vec!["High".to_string(), "Low".to_string()]);
    }

    #[test]
    fn integer_arrays_are_read_as_i32() {
        let bounds = [SAFEARRAYBOUND { cElements: 3, lLbound: 1 }];
        let psa = unsafe { SafeArrayCreate(VT_I2, 1, bounds.as_ptr()) };
        for (i, value) in [0i16, 1, 3].iter().enumerate() {
            let mut idx = [1 + i as i32];
            unsafe {
                SafeArrayPutElement(psa, idx.as_mut_ptr(), core::ptr::addr_of!(*value).cast::<c_void>())
                    .expect("PutElement i16")
            };
        }
        let v = own_array(psa, VT_I2);
        assert_eq!(read_ints(&v).unwrap(), vec![0, 1, 3]);
    }

    /// The lock count oleaut32 keeps in the array descriptor.
    fn lock_count(v: &Variant) -> u32 {
        let psa: *mut SAFEARRAY = v.array_ptr().unwrap().cast();
        unsafe { (*psa).cLocks }
    }

    #[test]
    fn a_view_leaves_no_lock_behind() {
        // `SafeArrayAccessData` documents that it increments the lock count and that
        // `SafeArrayUnaccessData` is the matching release, so a view must take exactly
        // one lock. A leaked one makes a driver that caches its array fail the next
        // resize or destroy with `DISP_E_ARRAYISLOCKED`.
        let v = own_array(make_2d_16(0, 0, 2, 2), VT_I2);
        assert_eq!(lock_count(&v), 0);
        {
            let view = SafeArrayView::from_variant(&v).unwrap();
            assert_eq!(lock_count(&v), 1, "accessed for exactly as long as the view lives");
            assert_eq!(view.element_count().unwrap(), 4);
        }
        assert_eq!(lock_count(&v), 0, "the array must be as free as we found it");
    }

    /// 1D array of `element` type, filled from the values' own bytes.
    fn array_of<T: Copy>(element: VARENUM, values: &[T]) -> *mut SAFEARRAY {
        let bounds = [SAFEARRAYBOUND { cElements: values.len() as u32, lLbound: 0 }];
        let psa = unsafe { SafeArrayCreate(element, 1, bounds.as_ptr()) };
        assert!(!psa.is_null());
        for (index, value) in values.iter().enumerate() {
            let idx = [index as i32];
            unsafe {
                SafeArrayPutElement(psa, idx.as_ptr(), core::ptr::addr_of!(*value).cast::<c_void>())
                    .expect("PutElement");
            }
        }
        psa
    }

    #[test]
    fn element_refuses_a_slot_smaller_than_the_cell() {
        // `SafeArrayGetElement` documents that the caller must provide storage of the
        // correct size: a 2-byte stack slot cannot receive a 4-byte cell.
        let v = own_array(array_of(VT_I4, &[42i32, -7]), VT_I4);
        let view = SafeArrayView::from_variant(&v).unwrap();
        assert_eq!(view.element::<i32>(&[0]).unwrap(), 42);
        let error = view.element::<i16>(&[0]).unwrap_err();
        assert_eq!(error.kind, AscomErrorKind::Com, "our own refusal: {error}");
    }

    #[test]
    fn element_refuses_cells_that_carry_a_reference() {
        // For a string, object or variant element `GetElement` copies the element "in
        // the correct way", i.e. an owning copy this function has nowhere to release.
        // Same element size as u64 here, so only the type can catch it.
        let psa = unsafe { SafeArrayCreate(VT_BSTR, 1, [SAFEARRAYBOUND { cElements: 1, lLbound: 0 }].as_ptr()) };
        assert!(!psa.is_null());
        unsafe {
            let bstr = windows::core::BSTR::from("a");
            let idx = [0i32];
            SafeArrayPutElement(psa, idx.as_ptr(), (&*bstr).as_ptr().cast::<c_void>())
                .expect("PutElement bstr");
        }
        let v = own_array(psa, VT_BSTR);
        let view = SafeArrayView::from_variant(&v).unwrap();
        assert_eq!(view.elem_size(), core::mem::size_of::<u64>());
        assert!(view.element::<u64>(&[0]).is_err(), "an owning copy must not be handed out as bits");
        assert_eq!(view.strings().unwrap(), ["a"], "the sanctioned reader still works");
    }

    #[test]
    fn bulk_readers_check_the_element_type_not_only_its_size() {
        let v = own_array(array_of(VT_R4, &[1.5f32, -0.25]), VT_R4);
        let view = SafeArrayView::from_variant(&v).unwrap();
        assert_eq!(view.f32s().unwrap(), &[1.5, -0.25]);
        assert!(view.i32s().is_err(), "rule 2: R4 bits must not be reinterpreted as i32");
        assert!(view.i16s().is_err());
    }

    /// 1D `VT_ARRAY | VT_VARIANT` of integers: what the `VT_VARIANT` slow path reads.
    fn variant_array(values: &[i32]) -> *mut SAFEARRAY {
        let bounds = [SAFEARRAYBOUND { cElements: values.len() as u32, lLbound: 0 }];
        let psa = unsafe { SafeArrayCreate(VT_VARIANT, 1, bounds.as_ptr()) };
        assert!(!psa.is_null());
        for (index, value) in values.iter().enumerate() {
            let cell = Variant::from_i32(*value);
            let idx = [index as i32];
            unsafe {
                SafeArrayPutElement(
                    psa,
                    idx.as_ptr(),
                    core::ptr::addr_of!(*cell.raw()).cast::<c_void>(),
                )
                .expect("PutElement variant");
            }
        }
        psa
    }

    #[test]
    fn our_own_bad_index_is_not_blamed_on_com() {
        let v = own_array(variant_array(&[7, 9]), VT_VARIANT);
        let view = SafeArrayView::from_variant(&v).unwrap();
        assert_eq!(view.variant_element(&[1]).unwrap().as_i32().unwrap(), 9);
        let error = view.variant_element(&[7]).err().expect("an out-of-range index must fail");
        assert_eq!(error.kind, AscomErrorKind::InvalidValue, "a bad index is ours: {error}");
        assert!(view.variant_element(&[0, 0]).is_err(), "wrong arity must not reach GetElement");
    }

    #[test]
    fn integer_lists_refuse_values_that_do_not_fit_i32() {
        // These feed `TrackingRates` -> `DriveRate`; `as i32` would turn
        // 3_000_000_000 into a negative rate and 1.5 into 1.
        let too_wide = own_array(array_of(VT_UI4, &[3_000_000_000u32]), VT_UI4);
        assert_eq!(read_ints(&too_wide).unwrap_err().kind, AscomErrorKind::Com);
        let fractional = own_array(array_of(VT_R8, &[1.5f64]), VT_R8);
        assert_eq!(read_ints(&fractional).unwrap_err().kind, AscomErrorKind::Com);
        // Values that do fit still read, including the exact `i32::MAX` boundary.
        assert_eq!(read_ints(&own_array(array_of(VT_UI4, &[4_000u32]), VT_UI4)).unwrap(), [4_000]);
        assert_eq!(
            read_ints(&own_array(array_of(VT_UI4, &[u32::try_from(i32::MAX).unwrap()]), VT_UI4))
                .unwrap(),
            [i32::MAX]
        );
        assert_eq!(read_ints(&own_array(array_of(VT_R4, &[12.0f32]), VT_R4)).unwrap(), [12]);
    }

    #[test]
    fn absurd_bounds_are_an_error_not_a_panic() {
        // Bounds come from the driver; a span wider than i32 must neither panic in a
        // debug build nor wrap into a plausible-looking empty dimension.
        let wide = Dim { lower: i32::MIN, upper: i32::MAX };
        assert_eq!(wide.len(), 1 << 32);
        // Three such dimensions cannot be counted in a usize at all, and the count is
        // what the readers use as a slice length and an allocation size.
        let absurd = [wide, wide, wide];
        let error = flat_positions(&absurd).err().expect("an uncountable shape must fail");
        assert_eq!(error.kind, AscomErrorKind::Driver, "{error}");
        // An empty dimension stays legitimate: oleaut32 reports upper = lower - 1.
        assert!(flat_positions(&[Dim { lower: 0, upper: -1 }]).unwrap().is_empty());
    }

    #[test]
    fn empty_list_is_not_an_error() {
        let empty = Variant::from_i32(0);
        // VT_EMPTY must produce an empty list rather than a conversion error.
        let nothing = unsafe { Variant::from_raw(VARIANT::default()) };
        assert_eq!(nothing.vt(), VT_EMPTY);
        assert!(read_strings(&nothing).unwrap().is_empty());
        assert!(read_ints(&nothing).unwrap().is_empty());
        // and a non-array variant must still be an error
        assert!(read_strings(&empty).is_err());
        assert!(read_ints(&empty).is_err());
    }
}
