//! `Camera.ImageArray` → [`Image`], and a FITS writer that needs no transpose.
//!
//! The spec describes the array as `Array[NumX, NumY]` and, from an application point
//! of view, concludes that the height index varies fastest: `idx = x * NumY + y`, with
//! the plane rightmost for full-color frames. A SAFEARRAY however varies `dim1`
//! fastest, and drivers do not agree on which axis they put in `dim1` — the OmniSim
//! camera returns `dim1 == NumX`, i.e. plain row-major (verified live). So the
//! orientation is **detected** from the array's dimensions against `NumX`/`NumY`,
//! recorded on [`Image::layout`], and never guessed. The buffer is copied verbatim and
//! never transposed; FITS gets its axes in fastest-first order, whatever that means
//! for the frame at hand.
//!
//! Verify against a simulator with a **non-square** frame: on a square one a
//! transposition mistake is invisible, which is the classic trap here.

use std::io::{BufWriter, Write};

use crate::camera::SensorType;
use crate::com::safearray::{Dim, SafeArrayView};
use crate::com::variant::Variant;
use crate::error::{AscomError, AscomErrorKind, Result};

/// Pixel values, in the driver's own memory order (x major, then y, then plane).
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum Pixels {
    I16(Vec<i16>),
    I32(Vec<i32>),
    F32(Vec<f32>),
    /// Only produced by the `VT_VARIANT` degradation path.
    F64(Vec<f64>),
}

impl Pixels {
    pub fn len(&self) -> usize {
        match self {
            Self::I16(v) => v.len(),
            Self::I32(v) => v.len(),
            Self::F32(v) => v.len(),
            Self::F64(v) => v.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Big-endian transfer width and FITS `BITPIX`.
    pub fn bitpix(&self) -> i32 {
        match self {
            Self::I16(_) => 16,
            Self::I32(_) => 32,
            Self::F32(_) => -32,
            Self::F64(_) => -64,
        }
    }

    /// Element at a raw buffer index (not an `(x, y, plane)` coordinate), as `f64`.
    pub fn value(&self, index: usize) -> Option<f64> {
        match self {
            Self::I16(v) => v.get(index).map(|&x| f64::from(x)),
            Self::I32(v) => v.get(index).map(|&x| f64::from(x)),
            Self::F32(v) => v.get(index).map(|&x| f64::from(x)),
            Self::F64(v) => v.get(index).copied(),
        }
    }
}

/// What the driver's other members say about the frame we are about to read.
#[derive(Debug, Clone, Copy)]
pub struct FrameGeometry {
    /// `Camera.NumX` at the time `StartExposure` was called.
    pub num_x: i32,
    /// `Camera.NumY`.
    pub num_y: i32,
    /// `Camera.MaxADU`, recorded so a saved frame can be scaled without the camera.
    pub adu_max: i32,
    /// `Camera.SensorType`.
    pub sensor: SensorType,
    /// `Camera.BayerOffsetX`/`BayerOffsetY`.
    pub bayer_offset_x: i32,
    pub bayer_offset_y: i32,
    /// `Camera.BinX`/`BinY`, for the FITS header.
    pub bin_x: i32,
    pub bin_y: i32,
}

impl Default for FrameGeometry {
    fn default() -> Self {
        Self {
            num_x: 0,
            num_y: 0,
            adu_max: 0,
            sensor: SensorType::Monochrome,
            bayer_offset_x: 0,
            bayer_offset_y: 0,
            bin_x: 1,
            bin_y: 1,
        }
    }
}

/// Which frame axis a stride belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Axis {
    X,
    Y,
    Plane,
}

/// Which dimension the driver put first inside the SAFEARRAY.
///
/// A SAFEARRAY varies `dim1` fastest, so this single fact decides the memory order.
/// The ASCOM spec describes `Array[NumX, NumY]` from an application point of view and
/// concludes the height varies fastest; a .NET array marshals with its dimensions
/// reversed, which agrees. Not every driver does: the OmniSim camera returns
/// `dim1 == NumX`, i.e. ordinary row-major. Both are decoded here, and the choice is
/// made from the array's own dimensions rather than assumed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Orientation {
    /// `dim1` (fastest) is `NumY`: the spec's app view, `idx = x * NumY + y`.
    SpecAppView,
    /// `dim1` (fastest) is `NumX`: row-major, `idx = y * NumX + x`.
    RowMajor,
}

impl Orientation {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SpecAppView => "SPEC-APP-VIEW",
            Self::RowMajor => "ROW-MAJOR",
        }
    }
}

/// The frame's shape and strides, derived from the array itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameLayout {
    pub width: usize,
    pub height: usize,
    /// 1 for monochrome and Bayer sensors, 3 for RGB, 4 for LRGB.
    pub planes: usize,
    /// Element stride of each axis in the driver's buffer.
    pub stride_x: usize,
    pub stride_y: usize,
    pub stride_plane: usize,
    pub orientation: Orientation,
    /// `true` when `NumX == NumY` left the orientation undetermined by lengths alone;
    /// the spec's convention was used. A square frame cannot prove which one it is.
    pub ambiguous: bool,
}

impl FrameLayout {
    /// Linear index of `(x, y, plane)` in the driver's memory order.
    pub fn index(&self, x: usize, y: usize, plane: usize) -> usize {
        x * self.stride_x + y * self.stride_y + plane * self.stride_plane
    }

    pub fn stride_of(&self, axis: Axis) -> usize {
        match axis {
            Axis::X => self.stride_x,
            Axis::Y => self.stride_y,
            Axis::Plane => self.stride_plane,
        }
    }

    /// Length of an axis.
    pub fn len_of(&self, axis: Axis) -> usize {
        match axis {
            Axis::X => self.width,
            Axis::Y => self.height,
            Axis::Plane => self.planes,
        }
    }

    /// The real axes, fastest varying first — which is the order FITS declares them.
    pub fn axes_fastest_first(&self) -> Vec<Axis> {
        let mut axes = vec![Axis::X, Axis::Y];
        if self.planes > 1 {
            axes.push(Axis::Plane);
        }
        axes.sort_by_key(|axis| self.stride_of(*axis));
        axes
    }
}

/// One exposure.
#[derive(Debug, Clone, PartialEq)]
pub struct Image {
    pub data: Pixels,
    /// Shape and memory strides of the buffer, worked out from the array itself.
    pub layout: FrameLayout,
    /// Width in pixels (`NumX`).
    pub width: usize,
    /// Height in pixels (`NumY`).
    pub height: usize,
    /// 1 for monochrome and Bayer sensors, 3 for RGB, 4 for LRGB.
    pub planes: usize,
    /// ADC depth reported by the driver (`MaxADU` rounded up to a bit count).
    pub bit_depth: u32,
    pub sensor: SensorType,
    pub bayer_offset: (i32, i32),
    pub adu_max: i32,
}

impl Image {
    /// Decodes an `ImageArray` VARIANT.
    ///
    /// Element type priority is `VT_I2` → `VT_I4` → `VT_R4` → `VT_VARIANT`. The last
    /// one is a degradation: 16 bytes per pixel (a 4000×3000 frame is ~200 MB and
    /// takes seconds to unpack), so it is reported through [`Image::wide_elements`].
    /// `ImageArrayVariant` is not used at all, per the spec's advice.
    pub fn from_variant(value: &Variant, geometry: FrameGeometry) -> Result<Self> {
        let view = SafeArrayView::from_variant(value)?;
        let dims = view.dims();
        if !(2..=3).contains(&dims.len()) {
            return Err(AscomError::local(
                AscomErrorKind::Driver,
                "ImageArray",
                format!("expected 2 or 3 dimensions, got {}", dims.len()),
            ));
        }
        // SAFEARRAY dim1 varies fastest. Which frame axis the driver put there is not
        // safe to assume, so it is read off the array's own dimensions and cross-checked
        // against NumX/NumY; see `resolve_layout`.
        let layout = resolve_layout(dims, &geometry)?;
        let (width, height, planes) = (layout.width, layout.height, layout.planes);

        let data = match view.vartype() {
            windows::Win32::System::Variant::VT_I2 => Pixels::I16(view.i16s()?.to_vec()),
            windows::Win32::System::Variant::VT_I4 => Pixels::I32(view.i32s()?.to_vec()),
            windows::Win32::System::Variant::VT_R4 => Pixels::F32(view.f32s()?.to_vec()),
            windows::Win32::System::Variant::VT_R8 => Pixels::F64(view.f64s()?.to_vec()),
            windows::Win32::System::Variant::VT_VARIANT => {
                Pixels::F64(unpack_variants(&view, dims)?)
            }
            other => {
                return Err(AscomError::type_mismatch(
                    "ImageArray",
                    format!("unsupported element type VARENUM({})", other.0),
                ));
            }
        };

        let expected = width * height * planes;
        if data.len() != expected {
            return Err(AscomError::local(
                AscomErrorKind::Driver,
                "ImageArray",
                format!("array holds {} elements, shape says {expected}", data.len()),
            ));
        }

        Ok(Self {
            data,
            layout,
            width,
            height,
            planes,
            bit_depth: bits_for_adu_max(geometry.adu_max),
            sensor: geometry.sensor,
            bayer_offset: (geometry.bayer_offset_x, geometry.bayer_offset_y),
            adu_max: geometry.adu_max,
        })
    }

    /// Linear index of `(x, y, plane)` in the driver's memory order.
    ///
    /// The strides come from [`Image::layout`], so this is correct for either the
    /// spec's app view (`idx = (x * height + y) * planes + plane`) or row-major
    /// (`idx = (y * width + x) * planes + plane`).
    pub fn index(&self, x: usize, y: usize, plane: usize) -> Option<usize> {
        if x >= self.width || y >= self.height || plane >= self.planes {
            return None;
        }
        Some(self.layout.index(x, y, plane))
    }

    /// Pixel value as `f64`, or `None` for coordinates outside the frame.
    pub fn pixel(&self, x: usize, y: usize, plane: usize) -> Option<f64> {
        self.index(x, y, plane).and_then(|i| self.data.value(i))
    }

    /// Value on row `y` of column `x` of plane `plane`, as the driver produced it.
    pub fn row(&self, x: usize, plane: usize) -> Option<Vec<f64>> {
        (0..self.height).map(|y| self.pixel(x, y, plane)).collect()
    }

    pub fn width(&self) -> usize {
        self.width
    }

    pub fn height(&self) -> usize {
        self.height
    }

    pub fn planes(&self) -> usize {
        self.planes
    }

    /// `true` when the pixels arrived as 64-bit elements (`VT_R8`, or the
    /// `VT_VARIANT` degradation), which is slow and memory-hungry: 8 bytes per pixel
    /// against 2 for the common `VT_I2`.
    pub fn wide_elements(&self) -> bool {
        matches!(self.data, Pixels::F64(_))
    }

    /// Total/min/max/mean over every plane, handy for asserting a simulator frame.
    pub fn stats(&self) -> Option<ImageStats> {
        let mut count = 0usize;
        let mut sum = 0.0f64;
        let mut min = f64::INFINITY;
        let mut max = f64::NEG_INFINITY;
        for index in 0..self.data.len() {
            let Some(value) = self.data.value(index) else { continue };
            count += 1;
            sum += value;
            min = f64::min(min, value);
            max = f64::max(max, value);
        }
        if count == 0 {
            return None;
        }
        Some(ImageStats { count, min, max, mean: sum / count as f64 })
    }

    /// Writes the frame as a FITS primary HDU, without transposing.
    ///
    /// FITS lists the fastest varying axis first, so the axes are emitted in the
    /// order [`FrameLayout::axes_fastest_first`] gives. That keeps the pixel bytes
    /// untouched whatever orientation the driver used.
    pub fn write_fits(&self, path: impl AsRef<std::path::Path>) -> std::io::Result<()> {
        let mut file = BufWriter::new(std::fs::File::create(path)?);
        let mut header = String::new();
        push_card(&mut header, &card_num("SIMPLE", "T"));
        push_card(&mut header, &card_num("BITPIX", &self.data.bitpix().to_string()));
        let axes = self.layout.axes_fastest_first();
        push_card(&mut header, &card_num("NAXIS", &axes.len().to_string()));
        for (i, axis) in axes.iter().enumerate() {
            push_card(&mut header, &card_num(&format!("NAXIS{}", i + 1), &self.layout.len_of(*axis).to_string()));
        }
        push_card(&mut header, &card_str("BUNIT", "ADU"));
        push_card(&mut header, &card_num("MAXADU", &self.adu_max.to_string()));
        push_card(&mut header, &card_num("BITDEPTH", &self.bit_depth.to_string()));
        push_card(&mut header, &card_str("ASCCDSEN", self.sensor.name()));
        push_card(&mut header, &card_str("ASCMEMOR", self.layout.orientation.as_str()));
        if self.layout.ambiguous {
            push_card(&mut header, &card_str("ASCAMBIG", "SQUARE-FRAME"));
        }
        if self.bayer_offset != (0, 0) {
            push_card(&mut header, &card_num("BAYERX", &self.bayer_offset.0.to_string()));
            push_card(&mut header, &card_num("BAYERY", &self.bayer_offset.1.to_string()));
        }
        push_card(&mut header, &card_str("ASCOMSRC", "ascom-rust"));
        push_card(&mut header, &card_end());

        file.write_all(header.as_bytes())?;
        // Pad the header to a whole 2880-byte block.
        let remainder = header.len() % FITS_BLOCK;
        if remainder != 0 {
            file.write_all(&vec![b' '; FITS_BLOCK - remainder])?;
        }

        write_pixels_be(&mut file, &self.data)?;
        file.flush()
    }
}

/// Min/max/mean of a frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ImageStats {
    pub count: usize,
    pub min: f64,
    pub max: f64,
    pub mean: f64,
}

const FITS_BLOCK: usize = 2880;

/// The `VT_VARIANT` slow path: one `SafeArrayGetElement` per pixel.
fn unpack_variants(view: &SafeArrayView<'_>, dims: &[Dim]) -> Result<Vec<f64>> {
    let positions = crate::com::safearray::flat_positions(dims)?;
    let mut out = Vec::with_capacity(positions.len());
    for position in positions {
        let value = view.variant_element(&position)?;
        out.push(value.as_f64()?);
    }
    Ok(out)
}

/// Works out the frame's shape and strides from the array's own dimensions.
///
/// A SAFEARRAY varies `dim1` fastest (verified experimentally), so the entire memory
/// layout follows from which frame axis the driver placed in `dim1`. Two conventions
/// exist and real drivers use both:
///
/// * **Spec app view** — the spec describes `Array[NumX, NumY(, plane)]` and a .NET
///   array marshals to COM with its dimensions reversed, so `dim1` ends up holding
///   `NumY` (or the plane). Memory then matches `idx = (x * NumY + y) * planes + p`.
/// * **Row-major** — the driver declares `dim1 == NumX`, giving `idx = y * NumX + x`.
///   Verified live: the OmniSim camera returns this orientation.
///
/// The two are told apart by matching the dimension lengths against `NumX`/`NumY`.
/// A square frame is genuinely ambiguous; the spec's convention wins and
/// [`FrameLayout::ambiguous`] records that the choice could not be proven.
fn resolve_layout(dims: &[Dim], geometry: &FrameGeometry) -> Result<FrameLayout> {
    // SAFEARRAY strides: dim1 is contiguous, each later dim strides by the product
    // of the earlier lengths. Checked: driver-supplied spans must never panic or
    // wrap here (a forged 3-D array can multiply two full i32 spans past usize).
    let mut dim_strides = vec![1usize; dims.len()];
    for i in 1..dims.len() {
        let Some(stride) = dim_strides[i - 1].checked_mul(dims[i - 1].len()) else {
            let shape: Vec<usize> = dims.iter().map(Dim::len).collect();
            return Err(AscomError::local(
                AscomErrorKind::Driver,
                "ImageArray",
                format!("array dimensions {shape:?} imply strides too large to address"),
            ));
        };
        dim_strides[i] = stride;
    }

    // Axis order per dim, fastest dim first, for each candidate convention.
    let candidates: &[(Orientation, &[Axis])] = match dims.len() {
        2 => &[
            (Orientation::SpecAppView, &[Axis::Y, Axis::X]),
            (Orientation::RowMajor, &[Axis::X, Axis::Y]),
        ],
        _ => &[
            (Orientation::SpecAppView, &[Axis::Plane, Axis::Y, Axis::X]),
            (Orientation::RowMajor, &[Axis::X, Axis::Y, Axis::Plane]),
        ],
    };

    let knows_shape = geometry.num_x > 0 && geometry.num_y > 0;
    let mut ambiguous = false;
    let mut chosen = None;
    let mut bad_planes = None;
    for (orientation, axes) in candidates {
        if axes.len() != dims.len() {
            continue;
        }
        let fits = dims.iter().zip(axes.iter()).all(|(dim, axis)| match axis {
            // Without NumX/NumY to compare against, only the plane count is checkable.
            Axis::X => !knows_shape || dim.len() == geometry.num_x as usize,
            Axis::Y => !knows_shape || dim.len() == geometry.num_y as usize,
            Axis::Plane => {
                // Spec 2.11: the plane axis appears only for colour frames and holds
                // 3 planes (RGB) or 4 (LRGB); monochrome is 1. A 0-plane axis also
                // zeroes the element-count check, letting write_fits declare a full
                // frame over an empty block, so refuse anything else.
                let len = dim.len();
                if matches!(len, 1 | 3 | 4) {
                    true
                } else {
                    bad_planes = Some(len);
                    false
                }
            }
        });
        if fits {
            if chosen.is_none() {
                chosen = Some((*orientation, *axes));
            } else {
                // Both conventions fit: the frame cannot tell them apart.
                ambiguous = true;
            }
        }
    }
    let Some((orientation, axes)) = chosen else {
        let shape: Vec<usize> = dims.iter().map(Dim::len).collect();
        let detail = match bad_planes {
            Some(planes) => format!(
                "array dimensions {shape:?} declare a plane axis of {planes}; the spec allows 1, 3 (RGB) or 4 (LRGB) planes"
            ),
            None => format!(
                "array dimensions {shape:?} match neither NumX={} nor NumY={} in either known orientation",
                geometry.num_x, geometry.num_y
            ),
        };
        return Err(AscomError::local(
            AscomErrorKind::Driver,
            "ImageArray",
            detail,
        ));
    };

    let mut layout = FrameLayout {
        width: geometry.num_x.max(0) as usize,
        height: geometry.num_y.max(0) as usize,
        planes: 1,
        stride_x: 0,
        stride_y: 0,
        stride_plane: 0,
        orientation,
        ambiguous,
    };
    for ((dim, stride), axis) in dims.iter().zip(&dim_strides).zip(axes.iter()) {
        match axis {
            Axis::X => {
                layout.width = dim.len();
                layout.stride_x = *stride;
            }
            Axis::Y => {
                layout.height = dim.len();
                layout.stride_y = *stride;
            }
            Axis::Plane => {
                layout.planes = dim.len();
                layout.stride_plane = *stride;
            }
        }
    }
    if dims.len() == 2 {
        // No plane axis: every sample is one plane, contiguous.
        layout.stride_plane = 0;
        layout.planes = 1;
    }

    if knows_shape && (layout.width != geometry.num_x as usize || layout.height != geometry.num_y as usize) {
        // Unreachable: a mismatch fails the `fits` test above. Kept as a guard so a
        // future edit cannot quietly produce a frame whose shape lies.
        return Err(AscomError::local(
            AscomErrorKind::Driver,
            "ImageArray",
            format!(
                "resolved shape {}x{} disagrees with NumX={} NumY={}",
                layout.width, layout.height, geometry.num_x, geometry.num_y
            ),
        ));
    }
    Ok(layout)
}

/// ADC depth implied by `MaxADU`; 0 when the driver does not say.
pub fn bits_for_adu_max(adu_max: i32) -> u32 {
    if adu_max <= 0 {
        return 0;
    }
    32 - u32::try_from(adu_max).unwrap_or(u32::MAX).leading_zeros()
}

// ---------------------------------------------------------------------------
// FITS cards
// ---------------------------------------------------------------------------

fn push_card(header: &mut String, card: &str) {
    debug_assert_eq!(card.chars().count(), 80, "FITS cards are exactly 80 columns");
    header.push_str(card);
}

fn pad(card: String) -> String {
    let mut out = card;
    while out.chars().count() < 80 {
        out.push(' ');
    }
    out
}

fn card_num(key: &str, value: &str) -> String {
    pad(format!("{key:<8}= {:>20}", value))
}

/// The terminator card: `END` in columns 1-3, blank for the rest.
fn card_end() -> String {
    pad("END".to_string())
}

/// FITS string values are single-quoted and padded to at least 8 characters.
fn card_str(key: &str, value: &str) -> String {
    let quoted = format!("'{value:<8}'");
    pad(format!("{key:<8}= {:<20}", quoted))
}

fn write_pixels_be(file: &mut impl Write, data: &Pixels) -> std::io::Result<()> {
    // FITS is big-endian regardless of host byte order, and the data block is padded
    // to a multiple of 2880 bytes.
    let mut bytes = Vec::with_capacity(data.len() * 4);
    match data {
        Pixels::I16(values) => {
            for value in values {
                bytes.extend_from_slice(&value.to_be_bytes());
            }
        }
        Pixels::I32(values) => {
            for value in values {
                bytes.extend_from_slice(&value.to_be_bytes());
            }
        }
        Pixels::F32(values) => {
            for value in values {
                bytes.extend_from_slice(&value.to_be_bytes());
            }
        }
        Pixels::F64(values) => {
            for value in values {
                bytes.extend_from_slice(&value.to_be_bytes());
            }
        }
    }
    file.write_all(&bytes)?;
    let remainder = bytes.len() % FITS_BLOCK;
    if remainder != 0 {
        file.write_all(&vec![0u8; FITS_BLOCK - remainder])?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::com::safearray::test_support::{
        test_i16_1d, test_i16_2d, test_i16_2d_row_major, test_r4_3d,
    };

    fn geometry(num_x: i32, num_y: i32) -> FrameGeometry {
        FrameGeometry {
            num_x,
            num_y,
            adu_max: 4095,
            ..Default::default()
        }
    }

    #[test]
    fn non_square_frame_keeps_x_major_memory_order() {
        // A 3 (wide) by 5 (tall) frame, 1-based, so a transposition cannot hide.
        let array = test_i16_2d(1, 1, 3, 5, |x, y| i16::try_from(x * 100 + y).unwrap());
        let image = Image::from_variant(&array, geometry(3, 5)).unwrap();

        assert_eq!(image.width(), 3);
        assert_eq!(image.height(), 5);
        assert_eq!(image.planes(), 1);
        assert_eq!(image.layout.orientation, Orientation::SpecAppView);
        assert!(!image.layout.ambiguous);
        // idx = x * NumY + y
        assert_eq!(image.index(0, 0, 0), Some(0));
        assert_eq!(image.index(0, 4, 0), Some(4));
        assert_eq!(image.index(1, 0, 0), Some(5));
        assert_eq!(image.index(2, 4, 0), Some(14));
        assert_eq!(image.index(3, 0, 0), None);

        assert_eq!(image.pixel(0, 0, 0), Some(101.0));
        assert_eq!(image.pixel(0, 4, 0), Some(105.0));
        assert_eq!(image.pixel(2, 4, 0), Some(305.0));
        // Column 0 top-to-bottom, i.e. consecutive in memory.
        assert_eq!(image.row(0, 0).unwrap(), vec![101.0, 102.0, 103.0, 104.0, 105.0]);
    }

    /// The orientation the OmniSim camera really returns: `dim1 == NumX`.
    #[test]
    fn row_major_driver_is_detected_not_transposed() {
        let array = test_i16_2d_row_major(1, 1, 3, 5, |x, y| i16::try_from(x * 100 + y).unwrap());
        let image = Image::from_variant(&array, geometry(3, 5)).unwrap();

        assert_eq!(image.width(), 3);
        assert_eq!(image.height(), 5);
        assert_eq!(image.layout.orientation, Orientation::RowMajor);
        assert!(!image.layout.ambiguous);
        // idx = y * NumX + x
        assert_eq!(image.index(0, 0, 0), Some(0));
        assert_eq!(image.index(2, 0, 0), Some(2));
        assert_eq!(image.index(0, 1, 0), Some(3));

        assert_eq!(image.pixel(0, 0, 0), Some(101.0));
        assert_eq!(image.pixel(2, 4, 0), Some(305.0));
        // Row 0 left-to-right is now the consecutive run.
        let row: Vec<f64> = (0..3).map(|x| image.pixel(x, 0, 0).unwrap()).collect();
        assert_eq!(row, vec![101.0, 201.0, 301.0]);
    }

    #[test]
    fn fits_axes_follow_the_actual_memory_order() {
        let fastest = |path: &std::path::Path| -> (String, String) {
            let bytes = std::fs::read(path).unwrap();
            let header = String::from_utf8_lossy(&bytes[..2880]).into_owned();
            (fits_value(&header, "NAXIS1"), fits_value(&header, "NAXIS2"))
        };

        // Height varies fastest -> NAXIS1 is the height.
        let spec = test_i16_2d(0, 0, 3, 5, |x, y| i16::try_from(x * 10 + y).unwrap());
        let spec_image = Image::from_variant(&spec, geometry(3, 5)).unwrap();
        let spec_path = std::env::temp_dir().join("ascom_orient_spec.fits");
        spec_image.write_fits(&spec_path).unwrap();
        assert_eq!(fastest(&spec_path), ("5".into(), "3".into()));

        // Width varies fastest -> NAXIS1 is the width. Same frame, other orientation.
        let row = test_i16_2d_row_major(0, 0, 3, 5, |x, y| i16::try_from(x * 10 + y).unwrap());
        let row_image = Image::from_variant(&row, geometry(3, 5)).unwrap();
        let row_path = std::env::temp_dir().join("ascom_orient_row.fits");
        row_image.write_fits(&row_path).unwrap();
        assert_eq!(fastest(&row_path), ("3".into(), "5".into()));

        let _ = std::fs::remove_file(&spec_path);
        let _ = std::fs::remove_file(&row_path);
    }

    #[test]
    fn fits_declares_height_as_the_fast_axis() {
        let array = test_i16_2d(0, 0, 3, 5, |x, y| i16::try_from(x * 10 + y).unwrap());
        let image = Image::from_variant(&array, geometry(3, 5)).unwrap();
        let path = std::env::temp_dir().join("ascom_non_square.fits");
        image.write_fits(&path).unwrap();

        let bytes = std::fs::read(&path).unwrap();
        let header = String::from_utf8_lossy(&bytes[..2880]).into_owned();
        let naxis1 = fits_value(&header, "NAXIS1");
        let naxis2 = fits_value(&header, "NAXIS2");
        // The buffer varies y fastest, so NAXIS1 must be the height.
        assert_eq!(naxis1, "5", "NAXIS1 must be NumY");
        assert_eq!(naxis2, "3", "NAXIS2 must be NumX");

        // And the first pixel on disk must be memory index 0, big-endian.
        let data = &bytes[2880..];
        assert_eq!(i16::from_be_bytes([data[0], data[1]]), 0);
        assert_eq!(i16::from_be_bytes([data[2], data[3]]), 1);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn three_planes_put_the_plane_index_rightmost() {
        let array = test_r4_3d(0, 0, 0, 2, 3, 3, |x, y, p| {
            f32::from(i16::try_from(x * 100 + y * 10 + p).unwrap())
        });
        let image = Image::from_variant(&array, geometry(2, 3)).unwrap();
        assert_eq!(image.planes(), 3);
        assert_eq!(image.data.bitpix(), -32);
        // idx = (x * NumY + y) * planes + p
        assert_eq!(image.index(0, 0, 0), Some(0));
        assert_eq!(image.index(0, 0, 2), Some(2));
        assert_eq!(image.index(0, 1, 0), Some(3));
        assert_eq!(image.index(1, 0, 0), Some(9));
        assert_eq!(image.pixel(1, 2, 2), Some(122.0));
        assert_eq!(image.pixel(0, 0, 3), None);
    }

    #[test]
    fn zero_based_and_one_based_arrays_agree() {
        // Same values at the same apparent pixel positions, only the bounds differ.
        let zero = test_i16_2d(0, 0, 2, 3, |x, y| i16::try_from(x * 10 + y).unwrap());
        let one = test_i16_2d(1, 1, 2, 3, |x, y| i16::try_from((x - 1) * 10 + (y - 1)).unwrap());
        let a = Image::from_variant(&zero, geometry(2, 3)).unwrap();
        let b = Image::from_variant(&one, geometry(2, 3)).unwrap();
        assert_eq!(a.data, b.data);
        assert_eq!(a.pixel(1, 2, 0), b.pixel(1, 2, 0));
    }

    #[test]
    fn a_flat_array_is_not_a_frame() {
        // 1 dimension: the spec's array is [NumX, NumY], so refuse rather than guess.
        let array = test_i16_1d(6, |i| i16::try_from(i).unwrap());
        let err = Image::from_variant(&array, geometry(6, 1)).unwrap_err();
        assert_eq!(err.member, "ImageArray");
        assert!(err.message.contains("dimensions"), "{}", err.message);
    }

    #[test]
    fn an_unmatchable_array_is_rejected() {
        // Neither NumX nor NumY explains dim1: report it rather than guess a shape.
        let array = test_i16_2d(0, 0, 4, 7, |x, y| i16::try_from(x * 10 + y).unwrap());
        let err = Image::from_variant(&array, geometry(3, 5)).unwrap_err();
        assert!(err.message.contains("match neither"), "{}", err.message);
    }

    #[test]
    fn a_square_frame_is_flagged_as_ambiguous() {
        // Square frames cannot prove their orientation; the spec wins and says so.
        let array = test_i16_2d(0, 0, 4, 4, |x, y| i16::try_from(x * 10 + y).unwrap());
        let image = Image::from_variant(&array, geometry(4, 4)).unwrap();
        assert_eq!(image.layout.orientation, Orientation::SpecAppView);
        assert!(image.layout.ambiguous, "a square frame must be flagged ambiguous");
    }

    #[test]
    fn implausible_plane_counts_are_refused() {
        // Spec 2.11: a colour frame adds a plane axis carrying 3 planes (RGB) or 4
        // (LRGB); monochrome is 1. 0 used to slip through: a 3x5x0 shape matches an
        // empty buffer, and write_fits then declared a 3x5 frame over no pixel data.
        for planes in [0, 2, 5] {
            let array = test_r4_3d(0, 0, 0, 3, 5, planes, |x, y, p| {
                f32::from(i16::try_from(x * 100 + y * 10 + p).unwrap())
            });
            let err = Image::from_variant(&array, geometry(3, 5)).unwrap_err();
            assert_eq!(err.kind, AscomErrorKind::Driver, "planes={planes}");
            assert_eq!(err.member, "ImageArray");
            assert!(err.message.contains("plane"), "planes={planes}: {}", err.message);
        }
    }

    #[test]
    fn forged_huge_dimensions_error_instead_of_overflowing() {
        // Two full i32 spans multiply past usize::MAX: the unchecked stride math
        // panicked in debug and wrapped in release before any shape check ran.
        let wide = Dim { lower: i32::MIN, upper: i32::MAX };
        let dims = [wide, wide, Dim { lower: 0, upper: 1 }];
        let err = resolve_layout(&dims, &geometry(0, 0)).unwrap_err();
        assert_eq!(err.kind, AscomErrorKind::Driver);
        assert_eq!(err.member, "ImageArray");
        assert!(err.message.contains("too large to address"), "{}", err.message);
    }

    #[test]
    fn a_scalar_is_not_a_frame_either() {
        let err = Image::from_variant(&Variant::from_i32(7), geometry(1, 1)).unwrap_err();
        assert_eq!(err.kind, AscomErrorKind::Com);
    }

    #[test]
    fn adu_max_maps_to_a_bit_depth() {
        assert_eq!(bits_for_adu_max(0), 0);
        assert_eq!(bits_for_adu_max(255), 8);
        assert_eq!(bits_for_adu_max(256), 9);
        assert_eq!(bits_for_adu_max(4095), 12);
        assert_eq!(bits_for_adu_max(65_535), 16);
    }

    #[test]
    fn header_cards_are_exactly_eighty_columns() {
        for card in [
            card_num("NAXIS1", "600"),
            card_str("BUNIT", "ADU"),
            card_str("ASCCDSEN", "RGGB"),
        ] {
            assert_eq!(card.chars().count(), 80, "{card}");
            assert_eq!(&card[8..10], "= ", "value marker must sit at column 9");
        }
        // FITS string values are padded to a minimum of 8 characters inside the quotes.
        assert_eq!(&card_str("BUNIT", "ADU")[10..20], "'ADU     '");
        assert_eq!(card_end().as_str(), "END".to_owned() + &" ".repeat(77));
    }

    #[test]
    fn stats_cover_every_plane() {
        let array = test_i16_2d(0, 0, 2, 2, |x, y| i16::try_from(x * 10 + y).unwrap());
        let image = Image::from_variant(&array, geometry(2, 2)).unwrap();
        let stats = image.stats().unwrap();
        assert_eq!(stats.count, 4);
        assert_eq!(stats.min, 0.0);
        assert_eq!(stats.max, 11.0);
        assert_eq!(stats.mean, 5.5);
    }

    /// Reads a header value out of a rendered FITS card.
    fn fits_value(header: &str, key: &str) -> String {
        for chunk in header.as_bytes().chunks(80) {
            let card = String::from_utf8_lossy(chunk).into_owned();
            // Keyword occupies columns 1-8, the `= ` marker columns 9-10.
            if card.starts_with(key) && card.len() >= 30 && &card[8..10] == "= " {
                return card[10..30].trim().to_string();
            }
        }
        panic!("{key} not found in header");
    }
}
