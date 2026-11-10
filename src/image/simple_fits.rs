use std::{io::*, str::FromStr};
use itertools::{Itertools, izip};

pub trait SeekNRead: Seek + Read {}
impl<T: Seek + Read> SeekNRead for T {}

pub trait SeekNWrite: Seek + Write {}
impl<T: Seek + Write> SeekNWrite for T {}

#[derive(Clone)]
struct Value {
    name: String,
    value: String,
    comment: Option<String>,
}

#[derive(Clone)]
pub struct Header {
    values:    Vec<Value>,
    bitpix:    i8,
    dims:      Vec<usize>,
    data_pos:  usize,
    data_len:  usize,
    bytes_len: usize,
}

const DEFAULT_BITPIX: i64 = 8;
const FITS_CARD_SIZE: usize = 80;
const FITS_BLOCK_SIZE: usize = 2880;
const FITS_CARDS_PER_BLOCK: usize = FITS_BLOCK_SIZE / FITS_CARD_SIZE;
const MAX_FITS_NAXIS: usize = 999;
const FITS_BLOCK_ZEROS: [u8; FITS_BLOCK_SIZE] = [0; FITS_BLOCK_SIZE];

fn invalid_data(msg: &str) -> Error {
    Error::new(ErrorKind::InvalidData, msg)
}

fn bit_bytes_per_value(bitpix: i8) -> usize {
    i8::unsigned_abs(bitpix) as usize / 8
}

// u16 ADU range and the margin by which a physical value still rounds into it
const ADU_MAX: f64 = 65535.0;
const ADU_ROUND_TOL: f64 = 0.5;

// Tolerances for the BITPIX = -32 range heuristic, see `FitsReader::read_data_f32`
const FLOAT_ADU_MAX: f64 = ADU_MAX + ADU_ROUND_TOL;
const FLOAT_NEG_ADU_TOL: f64 = 1000.0;
const FLOAT_NORM_HIGH_TOL: f64 = 0.001;
const FLOAT_NORM_NEG_TOL: f64 = 0.01;

// Streaming buffer size in elements for pixel data read/write loops
const BUF_DATA_LEN: usize = 512;

impl Header {
    pub fn new() -> Self {
        Self {
            values: Vec::new(),
            bitpix: 0,
            dims: Vec::new(),
            data_pos: 0,
            data_len: 0,
            bytes_len: 0,
        }
    }

    pub fn new_2d(width: usize, height: usize) -> Self {
        let dims = vec![width, height];
        Self {
            values: Vec::new(),
            bitpix: 0,
            dims,
            data_pos: 0,
            data_len: 0,
            bytes_len: 0,
        }
    }

    fn get_value_impl<T: FromStr>(values: &[Value], key: &str) -> Option<T> {
        values.iter()
            .find(|item| item.name.eq_ignore_ascii_case(key))?
            .value.parse()
            .ok()
    }

    fn set_value_impl(&mut self, key: &str, value: String) {
        if let Some(item) = self.values.iter_mut().find(|item| item.name.eq_ignore_ascii_case(key)) {
            item.value = value;
        } else {
            let value = Value {
                name: key.to_string(),
                value,
                comment: None,
            };
            self.values.push(value);
        }
    }

    pub fn set_i64(&mut self, key: &str, value: i64) {
        self.set_value_impl(key, value.to_string());
    }

    pub fn set_f64(&mut self, key: &str, value: f64) {
        self.set_value_impl(key, value.to_string());
    }

    pub fn set_bool(&mut self, key: &str, value: bool) {
        let value_str = if value { "T" } else { "F" };
        self.set_value_impl(key, value_str.to_string());
    }

    pub fn set_str(&mut self, key: &str, value: &str) {
        self.set_value_impl(key, format!("'{:<18}'", value));
    }

    pub fn bitpix(&self) -> i8 {
        self.bitpix
    }

    pub fn bytes_per_value(&self) -> usize {
        bit_bytes_per_value(self.bitpix)
    }

    pub fn get_i64(&self, key: &str) -> Option<i64> {
        Self::get_value_impl(&self.values, key)
    }

    pub fn get_f64(&self, key: &str) -> Option<f64> {
        Self::get_value_impl(&self.values, key)
    }

    /// Reads a real-valued card, both the plain (`32768`) and the exponent
    /// (`3.27680000000000E+04`) spelling. `None` means absent or unparsable; an unparsable
    /// card is logged rather than silently read as zero.
    fn real_card(&self, key: &str) -> Result<Option<f64>> {
        let Some(item) = self.values.iter().find(|item| item.name.eq_ignore_ascii_case(key)) else {
            return Ok(None);
        };
        let raw = item.value.trim();
        match raw.parse::<f64>() {
            Ok(value) if value.is_finite() => Ok(Some(value)),
            Ok(value) => Err(invalid_data(&format!("{key} = {value:?} is not finite"))),
            Err(_) => {
                log::warn!("FITS: unparsable {key} card `{raw}` is treated as absent");
                Ok(None)
            }
        }
    }

    /// String value without surrounding apostrophes, trimmed; unterminated quotes (corrupt file) are returned verbatim to avoid a panic.
    pub fn get_str<'b>(&'b self, key: &str) -> Option<&'b str> {
        let mut result = self.values.iter()
            .find(|item| item.name.eq_ignore_ascii_case(key))?
            .value.as_str();
        if let Some(stripped) = result.strip_prefix("'").and_then(|s| s.strip_suffix("'")) {
            result = stripped;
        }
        Some(result.trim())
    }

    pub fn dims(&self) -> &Vec<usize> {
        &self.dims
    }

    /// Number of values in the HDU, including PCOUNT/GCOUNT.
    pub fn data_len(&self) -> usize {
        self.data_len
    }

    /// Payload length without trailing 2880-byte padding.
    pub fn bytes_len(&self) -> usize {
        self.bytes_len
    }
}

///////////////////////////////////////////////////////////////////////////////

/// `BZERO` and `BSCALE` of an HDU, which define `physical = intrinsic * BSCALE + BZERO`.
/// Absent cards mean no mapping: 0 and 1.
fn mapping_cards(header: &Header) -> Result<(f64, f64)> {
    let bzero = header.real_card("BZERO")?.unwrap_or(0.0);
    let bscale = header.real_card("BSCALE")?.unwrap_or(1.0);
    if bscale == 0.0 {
        return Err(invalid_data("BSCALE = 0 is invalid"));
    }
    Ok((bzero, bscale))
}

/// How stored `BITPIX = 8`/`16` pixels map to `u16` ADU counts, see `IntMapping::from_header`.
enum IntMapping {
    /// Unit BSCALE and an integral BZERO: exact integer math
    Offset { bzero: i64 },
    /// Any other mapping: `f64` math
    Scaled { bscale: f64, bzero: f64 },
}

impl IntMapping {
    /// Builds the mapping from BZERO/BSCALE and rejects one that cannot reach the ADU range
    /// at all. The verdict is taken from the header, never from the pixels, so per-plane reads
    /// of an RGB triplet file always get the same mapping.
    fn from_header(header: &Header) -> Result<Self> {
        let (bzero, bscale) = mapping_cards(header)?;
        let mapped = bzero != 0.0 || bscale != 1.0;
        let (imin, imax) = intrinsic_range(header.bitpix, mapped);
        // The mapping is linear, so its extremes sit at the intrinsic bounds
        // (comparing both also swaps them for a negative BSCALE)
        let v1 = imin * bscale + bzero;
        let v2 = imax * bscale + bzero;
        let pmin = f64::min(v1, v2);
        let pmax = f64::max(v1, v2);

        if mapped {
            log::info!("FITS: applying BSCALE = {bscale:?}, BZERO = {bzero:?}");
            if pmin > ADU_MAX + ADU_ROUND_TOL || pmax < -ADU_ROUND_TOL {
                // No pixel is representable here, so the file would decode to uniform black/white
                return Err(Error::new(
                    ErrorKind::Unsupported,
                    format!(
                        "FITS mapping [BSCALE = {bscale:?}, BZERO = {bzero:?}] puts {}-bit data in \
                         [{pmin:?}, {pmax:?}]: nothing rounds into 0..{}",
                        header.bitpix, u16::MAX
                    )
                ));
            }
            if pmin < 0.0 || pmax > ADU_MAX {
                log::warn!(
                    "FITS: values [{pmin:?}, {pmax:?}] do not fit 0..{} and are saturated",
                    u16::MAX
                );
            }
        }

        if bscale == 1.0 && f64::trunc(bzero) == bzero {
            // The check above bounds BZERO well inside i64, so the cast is loss-free
            Ok(IntMapping::Offset { bzero: bzero as i64 })
        } else {
            Ok(IntMapping::Scaled { bscale, bzero })
        }
    }

    /// Whether the stored bits hold signed two's complement values, see `intrinsic_range`
    fn signed_stored(&self, bitpix: i8) -> bool {
        let mapped = match self {
            IntMapping::Offset { bzero } => *bzero != 0,
            IntMapping::Scaled { .. } => true
        };
        bitpix == 16 && mapped
    }

    fn to_adu(&self, intrinsic: i32) -> u16 {
        match self {
            // Out-of-range results saturate; `from_header` warns when the mapping can produce them
            IntMapping::Offset { bzero } => {
                i64::clamp(i64::from(intrinsic) + bzero, 0, i64::from(u16::MAX)) as u16
            }
            IntMapping::Scaled { bscale, bzero } => {
                let physical = f64::from(intrinsic) * bscale + bzero;
                f64::clamp(f64::round(physical), 0.0, ADU_MAX) as u16
            }
        }
    }
}

/// Intrinsic value range stored in the file.
///
/// `BITPIX = 8` is always unsigned. `BITPIX = 16` bits hold a signed range only for a
/// non-identity mapping (the canonical `BZERO = 32768` shifts signed int16 into the storage
/// slot); with no mapping they stay unsigned, because many cameras dump `uint16` payloads
/// without a BZERO card.
fn intrinsic_range(bitpix: i8, mapped: bool) -> (f64, f64) {
    if bitpix == 8 {
        (0.0, 255.0)
    } else if mapped {
        (f64::from(i16::MIN), f64::from(i16::MAX))
    } else {
        (0.0, ADU_MAX)
    }
}

///////////////////////////////////////////////////////////////////////////////

pub struct FitsReader {
    pub headers: Vec<Header>,
}

impl FitsReader {
    pub fn new(stream: &mut dyn SeekNRead) -> Result<FitsReader> {
        stream.seek(SeekFrom::Start(0))?;

        let hdus = Self::read_all_headers(stream)?;
        Ok(Self { headers: hdus })
    }

    fn read_all_headers(stream: &mut dyn SeekNRead) -> Result<Vec<Header>> {
        let mut result = Vec::new();

        loop {
            let hdu_res = Self::read_header(stream);
            let hdu = match hdu_res {
                Ok(hdu) => hdu,
                Err(err) => {
                    if err.kind() == ErrorKind::UnexpectedEof { break; }
                    else { return Err(err); }
                }
            };

            // FITS data units are padded to whole 2880-byte blocks.
            let skip_bytes = usize::checked_next_multiple_of(hdu.bytes_len, FITS_BLOCK_SIZE)
                .ok_or_else(|| invalid_data("FITS padded data size overflow"))?;
            let next_pos = u64::checked_add(hdu.data_pos as u64, skip_bytes as u64)
                .ok_or_else(|| invalid_data("FITS next HDU offset overflow"))?;
            result.push(hdu);
            stream.seek(SeekFrom::Start(next_pos))?;
        }

        Ok(result)
    }

    fn read_header(stream: &mut dyn SeekNRead) -> Result<Header> {
        let mut buf = [0_u8; FITS_BLOCK_SIZE];
        let mut values = Vec::new();
        let mut last_block = false;
        loop {
            stream.read_exact(&mut buf)?;
            for line in buf.chunks(FITS_CARD_SIZE) {
                let line = std::str::from_utf8(line)
                    .map_err(|e| Error::new(ErrorKind::InvalidData, e))?
                    .trim();
                if let Some((key, value_and_comment)) = line.split_once("=") {
                    let value_and_comment = value_and_comment.trim();
                    let (value, comment) = value_and_comment
                        .split_once("/")
                        .unwrap_or((value_and_comment, ""));
                    let value = value.trim();
                    values.push(Value {
                        name:    key.trim().to_string(),
                        value:   value.trim().to_string(),
                        comment: Some(comment.trim().to_string())
                    });
                }
                if line.eq_ignore_ascii_case("end") {
                    last_block = true;
                }
            }
            if last_block {
                break;
            }
        }

        let ndim_i64: i64 = Header::get_value_impl(&values, "NAXIS").unwrap_or(0);
        if ndim_i64 < 0 || ndim_i64 > MAX_FITS_NAXIS as i64 {
            return Err(invalid_data("NAXIS is invalid"));
        }
        let ndim = ndim_i64 as usize;

        let gcount_i64: i64 = Header::get_value_impl(&values, "GCOUNT").unwrap_or(1);
        let pcount_i64: i64 = Header::get_value_impl(&values, "PCOUNT").unwrap_or(0);
        if gcount_i64 < 0 || pcount_i64 < 0 {
            return Err(invalid_data("GCOUNT or PCOUNT is negative"));
        }
        let gcount = usize::try_from(gcount_i64)
            .map_err(|_| invalid_data("GCOUNT is too large"))?;
        let pcount = usize::try_from(pcount_i64)
            .map_err(|_| invalid_data("PCOUNT is too large"))?;

        let bitpix_i64: i64 = Header::get_value_impl(&values, "BITPIX").unwrap_or(DEFAULT_BITPIX);
        // A primary HDU with NAXIS=0 and no PCOUNT parameters has no data.
        let no_data = ndim == 0 && pcount == 0;
        let bitpix = if no_data {
            i8::try_from(bitpix_i64).unwrap_or(0)
        } else {
            if !matches!(bitpix_i64, 8 | 16 | 32 | 64 | -32 | -64) {
                return Err(invalid_data(&format!("BITPIX = {bitpix_i64} is not supported")));
            }
            bitpix_i64 as i8
        };
        let byte_per_value = if no_data {
            0
        } else {
            bit_bytes_per_value(bitpix)
        };

        let mut dims = Vec::with_capacity(ndim);
        let mut data_len = if no_data { 0 } else { 1 };
        for idx in 1 ..= ndim {
            let key = format!("NAXIS{}", idx);
            let dim_i64: i64 = Header::get_value_impl(&values, &key)
                .ok_or_else(|| invalid_data(&format!("Missing {key}")))?;
            if dim_i64 < 0 {
                return Err(invalid_data(&format!("{key} is negative")));
            }
            let dim = usize::try_from(dim_i64)
                .map_err(|_| invalid_data(&format!("{key} is too large")))?;
            data_len = usize::checked_mul(data_len, dim)
                .ok_or_else(|| invalid_data("FITS image size overflow"))?;
            dims.push(dim);
        }
        data_len = usize::checked_add(data_len, pcount)
            .ok_or_else(|| invalid_data("FITS PCOUNT overflow"))?;
        data_len = usize::checked_mul(data_len, gcount)
            .ok_or_else(|| invalid_data("FITS GCOUNT overflow"))?;

        let bytes_len = usize::checked_mul(data_len, byte_per_value)
            .ok_or_else(|| invalid_data("FITS data size overflow"))?;
        let data_pos = usize::try_from(stream.stream_position()?)
            .map_err(|_| invalid_data("FITS position is too large"))?;

        Ok(Header{values, bitpix, dims, data_pos, data_len, bytes_len})
    }

    /// Decodes an image HDU region into `u16` ADU counts.
    ///
    /// For `BITPIX = 8`/`16` the `BZERO`/`BSCALE` mapping is applied (both cards are real
    /// values in FITS, so `32768`, `32768.0` and `3.2768E+04` are equivalent); results outside
    /// the ADU range saturate. A non-finite card or `BSCALE = 0` is `InvalidData`, a mapping
    /// that cannot reach the ADU range at all is `Unsupported`. For `BITPIX = -32` the mapping
    /// is chosen per HDU, see `read_data_f32`.
    pub fn read_data(
        header: &Header,
        stream: &mut dyn SeekNRead,
        offset: usize,
        result: &mut [u16]
    ) -> Result<()> {
        if !matches!(header.bitpix, 8 | 16 | -32) {
            return Err(Error::new(
                ErrorKind::Unsupported,
                format!("BITPIX = {} is not supported", header.bitpix)
            ));
        }
        if header.bitpix == -32 {
            return Self::read_data_f32(header, stream, offset, result);
        }
        let mapping = IntMapping::from_header(header)?;
        let signed_stored = mapping.signed_stored(header.bitpix);
        let elem_len = header.bytes_per_value();
        let mut stream_buf = vec![0_u8; BUF_DATA_LEN * elem_len];
        stream.seek(SeekFrom::Start((header.data_pos + offset) as u64))?;
        for chunk in result.chunks_mut(BUF_DATA_LEN) {
            let len_to_read = chunk.len();
            let buf = &mut stream_buf[.. elem_len * len_to_read];
            stream.read_exact(buf)?;
            match header.bitpix {
                8 => {
                    for (b, dst) in izip!(buf, chunk) {
                        *dst = mapping.to_adu(i32::from(*b));
                    }
                }
                16 => {
                    for ((b1, b2), dst) in izip!(buf.iter().tuples(), chunk) {
                        let raw = ((*b1 as u16) << 8) + *b2 as u16;
                        let intrinsic = if signed_stored { i32::from(raw as i16) } else { i32::from(raw) };
                        *dst = mapping.to_adu(intrinsic);
                    }
                },
                _ => unreachable!(),
            }
        }
        Ok(())
    }

    /// Converts float32 (`BITPIX = -32`) pixels to `u16` ADU counts.
    ///
    /// The FITS mapping `physical = intrinsic * BSCALE + BZERO` is applied when
    /// BZERO/BSCALE cards are present. Third-party float files use two conventions
    /// that no header key reliably distinguishes: normalized 0..1 values
    /// (PixInsight/Siril exports) and physical counts (astropy/NOIRLab calibrated
    /// or stacked frames), so the value range scanned over the whole HDU decides
    /// the mapping (consistent across per-plane reads of RGB triplet files).
    /// If neither candidate maps into the `u16` ADU range, `Unsupported` is
    /// returned instead of silently clipping the whole image to 0/65535.
    fn read_data_f32(
        header: &Header,
        stream: &mut dyn SeekNRead,
        offset: usize,
        result: &mut [u16]
    ) -> Result<()> {
        if result.is_empty() {
            return Ok(());
        }
        // BZERO/BSCALE are real values in FITS; `mapping_cards` accepts both spellings
        let (bzero, bscale) = mapping_cards(header)?;
        let mapped = bzero != 0.0 || bscale != 1.0;
        let physical = |v: f64| v * bscale + bzero;

        let elem_len = header.bytes_per_value();
        let data_start = (header.data_pos + offset) as u64;
        let mut stream_buf = vec![0_u8; BUF_DATA_LEN * elem_len];

        // Pass 1: physical value range over the whole HDU (from `data_pos`, not `offset`)
        // so that sequential per-plane reads of an RGB triplet file share one mapping
        let mut scan_len = 1_usize;
        for dim in &header.dims {
            scan_len = usize::checked_mul(scan_len, *dim).unwrap_or(usize::MAX);
        }
        let scan_len = usize::max(scan_len, result.len());
        stream.seek(SeekFrom::Start(header.data_pos as u64))?;
        let mut min = f64::INFINITY;
        let mut max = f64::NEG_INFINITY;
        let mut pos = 0_usize;
        while pos < scan_len {
            let count = usize::min(BUF_DATA_LEN, scan_len - pos);
            let buf = &mut stream_buf[.. elem_len * count];
            stream.read_exact(buf)?;
            for b in buf.chunks_exact(elem_len) {
                let bytes: [u8; 4] = b.try_into().unwrap();
                // Float overflow is defined (inf) and caught by the finiteness check below
                let phys = physical(f64::from(f32::from_be_bytes(bytes)));
                if !phys.is_finite() {
                    return Err(Error::new(
                        ErrorKind::Unsupported,
                        format!("Non-finite float pixel value ({phys})")
                    ));
                }
                min = f64::min(min, phys);
                max = f64::max(max, phys);
            }
            pos += count;
        }

        // Choose the ADU mapping from the value range
        let normalized = if max <= 1.0 + FLOAT_NORM_HIGH_TOL {
            if min < -FLOAT_NORM_NEG_TOL {
                return Self::float_unsupported(min, max, mapped);
            }
            true
        } else if max <= FLOAT_ADU_MAX && min >= -FLOAT_NEG_ADU_TOL {
            if min < 0.0 {
                log::warn!("FITS float: negative values (min {min}) clipped to 0");
            }
            false
        } else {
            return Self::float_unsupported(min, max, mapped);
        };
        // Normalized 0..1 convention (also covers all-zero files) is stretched to the ADU range
        let stretch = if normalized { u16::MAX as f64 } else { 1.0 };
        log::info!("FITS float: reading as {}", if normalized {
            "0..1 normalized values (stretched to 0..65535)"
        } else {
            "ADU counts"
        });

        // Pass 2: convert
        stream.seek(SeekFrom::Start(data_start))?;
        for chunk in result.chunks_mut(BUF_DATA_LEN) {
            let buf = &mut stream_buf[.. elem_len * chunk.len()];
            stream.read_exact(buf)?;
            let buf = &*buf;
            for ((b1, b2, b3, b4), dst) in izip!(buf.iter().tuples(), chunk) {
                let value = f64::from(f32::from_be_bytes([*b1, *b2, *b3, *b4]));
                // Range-checked in pass 1, so `adu` is finite here
                let adu = physical(value) * stretch;
                // Slight negatives and the rounding overshoot clip to the 0..65535 bounds
                let rounded = f64::min(f64::max(f64::round(adu), 0.0), u16::MAX as f64);
                *dst = rounded as u16;
            }
        }
        Ok(())
    }

    fn float_unsupported(min: f64, max: f64, mapped: bool) -> Result<()> {
        Err(Error::new(
            ErrorKind::Unsupported,
            format!(
                "FITS float values [{min}, {max}] cannot be mapped to the 0..65535 ADU range{}",
                if mapped { " (after the BZERO/BSCALE mapping)" } else { "" }
            )
        ))
    }
}

///////////////////////////////////////////////////////////////////////////////

pub struct FitsTableCol {
    pub name: &'static str,
    pub type_: &'static str,
    pub unit: &'static str,
}

pub struct FitsWriter {}

impl FitsWriter {
    pub fn new() -> Self {
        Self {}
    }

    fn write_block_padding<W: Write + ?Sized>(stream: &mut W, written: usize) -> Result<()> {
        let rem = written % FITS_BLOCK_SIZE;
        if rem != 0 {
            stream.write_all(&FITS_BLOCK_ZEROS[..FITS_BLOCK_SIZE - rem])?;
        }
        Ok(())
    }

    pub fn write_header(&self, stream: &mut dyn Write, hdu: &Header) -> Result<()> {
        for item in &hdu.values {
            let mut line = format!("{:8}= ", item.name);
            if item.value.starts_with("'") {
                line.push_str(&format!("{:<20}", item.value))
            } else {
                line.push_str(&format!("{:>20}", item.value))
            }
            line.push_str(" / ");
            line.push_str(item.comment.as_ref().unwrap_or(&item.name));
            while line.len() < FITS_CARD_SIZE { line.push(' '); }
            while line.len() > FITS_CARD_SIZE { line.pop(); }
            stream.write_all(line.as_bytes())?;
        }
        write!(stream, "{:<width$}", "END", width = FITS_CARD_SIZE)?;
        let lines_written = hdu.values.len() + 1;
        let lines_to_complete =
            (FITS_CARDS_PER_BLOCK - lines_written % FITS_CARDS_PER_BLOCK) % FITS_CARDS_PER_BLOCK;
        for _ in 0..lines_to_complete {
            write!(stream, "{:width$}", "", width = FITS_CARD_SIZE)?;
        }
        Ok(())
    }

    pub fn write_data(
        &self,
        bitpix: i8,
        bzero:  u16,
        stream: &mut dyn Write,
        data:   &[u16],
    ) -> Result<()> {
        if !matches!(bitpix, 8|16) {
            return Err(Error::new(
                ErrorKind::Unsupported,
                format!("BITPIX = {} is not supported", bitpix)
            ));
        }
        let item_len = bit_bytes_per_value(bitpix);
        let mut stream_buf = vec![0_u8; BUF_DATA_LEN * item_len];
        let mut written = 0_usize;
        for chunk in data.chunks(BUF_DATA_LEN) {
            let len_to_write = chunk.len();
            let buf = &mut stream_buf[.. item_len * len_to_write];
            match bitpix {
                8 => {
                    for (b, v) in izip!(buf.iter_mut(), chunk) {
                        *b = (*v as u8).wrapping_sub(bzero as u8);
                    }
                }
                16 => {
                    for ((b1, b2), v) in izip!(buf.iter_mut().tuples(), chunk) {
                        let be_bytes = v.wrapping_sub(bzero).to_be_bytes();
                        [*b1, *b2] = be_bytes;
                    }
                },
                _ => unreachable!(),
            }
            stream.write_all(buf)?;
            written += buf.len();
        }
        Self::write_block_padding(stream, written)?;
        Ok(())
    }


    pub fn write_header_and_bintable_f64(
        &self,
        stream: &mut dyn SeekNWrite,
        cols: &[FitsTableCol],
        data: &[f64],
    ) -> Result<()> {
        assert!(!data.is_empty());
        let len = data.len() / cols.len();
        let mut full_hdr = Header::new();
        full_hdr.set_str("XTENSION", "BINTABLE");
        full_hdr.set_i64("BITPIX", 8);
        full_hdr.set_i64("NAXIS", 2);
        full_hdr.set_i64("NAXIS1", (8 * cols.len()) as i64);
        full_hdr.set_i64("NAXIS2", len as i64);
        full_hdr.set_i64("PCOUNT", 0);
        full_hdr.set_i64("GCOUNT", 1);
        full_hdr.set_i64("TFIELDS", cols.len() as i64);

        for (idx, col) in cols.iter().enumerate() {
            let name_fld = format!("TTYPE{}", idx + 1);
            full_hdr.set_str(&name_fld, col.name);
            let type_fld = format!("TFORM{}", idx + 1);
            full_hdr.set_str(&type_fld, col.type_);
        }
        for (idx, col) in cols.iter().enumerate() {
            let unit_fld = format!("TUNIT{}", idx + 1);
            full_hdr.set_str(&unit_fld, col.unit);
        }

        self.write_header(stream, &full_hdr)?;
        self.write_data_f64(stream, data)?;
        Ok(())
    }


    fn write_data_f64(
        &self,
        stream: &mut dyn SeekNWrite,
        data:   &[f64],
    ) -> Result<()> {
        let item_len = std::mem::size_of::<f64>();
        let mut stream_buf = vec![0_u8; BUF_DATA_LEN * item_len];
        let mut written = 0_usize;
        for chunk in data.chunks(BUF_DATA_LEN) {
            let buf = &mut stream_buf[.. std::mem::size_of_val(chunk)];
            for ((b1, b2, b3, b4, b5, b6, b7, b8), v) in izip!(buf.iter_mut().tuples(), chunk) {
                [*b1, *b2, *b3, *b4, *b5, *b6, *b7, *b8] = v.to_be_bytes();
            }
            stream.write_all(buf)?;
            written += buf.len();
        }

        Self::write_block_padding(stream, written)?;

        Ok(())
    }

}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn empty_primary_header() -> Header {
        let mut hdu = Header::new();
        hdu.set_bool("SIMPLE", true);
        hdu.set_i64("BITPIX", 8);
        hdu.set_i64("NAXIS", 0);
        hdu.set_bool("EXTEND", true);
        hdu
    }

    fn image_header(width: usize, height: usize) -> Header {
        let mut hdu = Header::new();
        hdu.set_str("XTENSION", "IMAGE");
        hdu.set_i64("BITPIX", 16);
        hdu.set_i64("NAXIS", 2);
        hdu.set_i64("NAXIS1", width as i64);
        hdu.set_i64("NAXIS2", height as i64);
        hdu.set_i64("PCOUNT", 0);
        hdu.set_i64("GCOUNT", 1);
        hdu
    }

    fn empty_primary_bytes() -> Vec<u8> {
        let mut out = Vec::new();
        FitsWriter::new().write_header(&mut out, &empty_primary_header()).unwrap();
        out
    }

    #[test]
    fn empty_primary_has_no_data() {
        let mut cursor = Cursor::new(empty_primary_bytes());
        let reader = FitsReader::new(&mut cursor).unwrap();
        assert_eq!(reader.headers.len(), 1);
        assert_eq!(reader.headers[0].data_len(), 0);
        assert_eq!(reader.headers[0].bytes_len(), 0);
    }

    #[test]
    fn multi_hdu_file_after_empty_primary() {
        let mut out = empty_primary_bytes();
        let writer = FitsWriter::new();
        let hdu = image_header(10, 10);
        writer.write_header(&mut out, &hdu).unwrap();
        writer.write_data(16, 0, &mut out, &[1_u16; 100]).unwrap();

        let mut cursor = Cursor::new(out);
        let reader = FitsReader::new(&mut cursor).unwrap();
        assert_eq!(reader.headers.len(), 2);
        assert_eq!(reader.headers[1].dims().as_slice(), &[10_usize, 10]);
        assert_eq!(reader.headers[1].bytes_len(), 200);

        let mut data = [0_u16; 100];
        FitsReader::read_data(&reader.headers[1], &mut cursor, 0, &mut data).unwrap();
        assert!(data.iter().all(|v| *v == 1));
    }

    #[test]
    fn write_data_pads_to_fits_block() {
        let mut out = Vec::new();
        FitsWriter::new().write_data(16, 0, &mut out, &[0_u16; 100]).unwrap();
        assert_eq!(out.len(), FITS_BLOCK_SIZE);
    }

    #[test]
    fn write_header_does_not_add_extra_block_when_full() {
        let mut hdu = Header::new();
        for idx in 0 .. 35 {
            hdu.set_i64(&format!("K{idx}"), idx);
        }

        let mut out = Vec::new();
        FitsWriter::new().write_header(&mut out, &hdu).unwrap();
        assert_eq!(out.len(), FITS_BLOCK_SIZE);
    }

    #[test]
    fn rejects_invalid_bitpix() {
        let mut hdu = Header::new();
        hdu.set_i64("BITPIX", 1);
        hdu.set_i64("NAXIS", 2);
        hdu.set_i64("NAXIS1", 2);
        hdu.set_i64("NAXIS2", 2);

        let mut out = Vec::new();
        FitsWriter::new().write_header(&mut out, &hdu).unwrap();

        let mut cursor = Cursor::new(out);
        let err = match FitsReader::new(&mut cursor) {
            Ok(_) => panic!("expected error"),
            Err(err) => err,
        };
        assert_eq!(err.kind(), ErrorKind::InvalidData);
    }

    #[test]
    fn rejects_missing_axis() {
        let mut hdu = Header::new();
        hdu.set_i64("BITPIX", 8);
        hdu.set_i64("NAXIS", 1);

        let mut out = Vec::new();
        FitsWriter::new().write_header(&mut out, &hdu).unwrap();

        let mut cursor = Cursor::new(out);
        let err = match FitsReader::new(&mut cursor) {
            Ok(_) => panic!("expected error"),
            Err(err) => err,
        };
        assert_eq!(err.kind(), ErrorKind::InvalidData);
    }

    #[test]
    fn multi_hdu_reader_skips_padded_data() {
        let mut out = empty_primary_bytes();
        let writer = FitsWriter::new();

        let first = image_header(100, 100);
        writer.write_header(&mut out, &first).unwrap();
        writer.write_data(16, 0, &mut out, &[1_u16; 10000]).unwrap();

        let second = image_header(10, 10);
        writer.write_header(&mut out, &second).unwrap();
        writer.write_data(16, 0, &mut out, &[2_u16; 100]).unwrap();

        let mut cursor = Cursor::new(out);
        let reader = FitsReader::new(&mut cursor).unwrap();
        assert_eq!(reader.headers.len(), 3);
        assert_eq!(reader.headers[1].bytes_len(), 20000);
        assert_eq!(reader.headers[2].bytes_len(), 200);

        let mut data = [0_u16; 100];
        FitsReader::read_data(&reader.headers[2], &mut cursor, 0, &mut data).unwrap();
        assert!(data.iter().all(|v| *v == 2));
    }

    #[test]
    fn write_data_pads_8bit_to_fits_block() {
        let mut out = Vec::new();
        FitsWriter::new().write_data(8, 0, &mut out, &[0_u16; 100]).unwrap();
        assert_eq!(out.len(), FITS_BLOCK_SIZE);
    }

    #[test]
    fn empty_primary_allows_bitpix_zero() {
        let mut hdu = Header::new();
        hdu.set_bool("SIMPLE", true);
        hdu.set_i64("BITPIX", 0);
        hdu.set_i64("NAXIS", 0);
        hdu.set_bool("EXTEND", true);

        let mut out = Vec::new();
        FitsWriter::new().write_header(&mut out, &hdu).unwrap();

        let mut cursor = Cursor::new(out);
        let reader = FitsReader::new(&mut cursor).unwrap();
        assert_eq!(reader.headers.len(), 1);
        assert_eq!(reader.headers[0].bytes_len(), 0);
    }

    #[test]
    fn corrupt_lone_apostrophe_card_does_not_panic() {
        let mut block = vec![b' '; FITS_BLOCK_SIZE];
        let cards = [
            "SIMPLE =                        T",
            "BITPIX =                        0",
            "NAXIS =                         0",
            "BAYERPAT=                       '",
            "OBSERVAT=                       ''",
            "END",
        ];
        for (idx, card) in cards.iter().enumerate() {
            let start = idx * FITS_CARD_SIZE;
            block[start .. start + card.len()].copy_from_slice(card.as_bytes());
        }

        let mut cursor = Cursor::new(block);
        let reader = FitsReader::new(&mut cursor).unwrap();
        assert_eq!(reader.headers[0].get_str("BAYERPAT"), Some("'"));
        assert_eq!(reader.headers[0].get_str("OBSERVAT"), Some(""));
    }

    #[test]
    fn rejects_bitpix_out_of_i8_range() {
        let mut hdu = Header::new();
        hdu.set_i64("BITPIX", 128);
        hdu.set_i64("NAXIS", 2);
        hdu.set_i64("NAXIS1", 2);
        hdu.set_i64("NAXIS2", 2);

        let mut out = Vec::new();
        FitsWriter::new().write_header(&mut out, &hdu).unwrap();

        let mut cursor = Cursor::new(out);
        let err = match FitsReader::new(&mut cursor) {
            Ok(_) => panic!("expected error"),
            Err(err) => err,
        };
        assert_eq!(err.kind(), ErrorKind::InvalidData);
    }

    fn float32_file(data: &[f32], extra: &dyn Fn(&mut Header)) -> Vec<u8> {
        let mut hdu = test_image_header(-32, data.len());
        extra(&mut hdu);

        let mut out = Vec::new();
        FitsWriter::new().write_header(&mut out, &hdu).unwrap();
        for v in data {
            out.extend_from_slice(&v.to_be_bytes());
        }
        out
    }

    fn read_float32(data: &[f32], extra: &dyn Fn(&mut Header)) -> std::io::Result<Vec<u16>> {
        let mut cursor = Cursor::new(float32_file(data, extra));
        let reader = FitsReader::new(&mut cursor)?;
        let mut result = vec![0_u16; data.len()];
        FitsReader::read_data(&reader.headers[0], &mut cursor, 0, &mut result)?;
        Ok(result)
    }

    #[test]
    fn float32_counts_are_not_rescaled() {
        let data = read_float32(
            &[1000.0, 20000.5, 50000.0, -50.0],
            &|_hdu| {}
        ).unwrap();
        assert_eq!(data, vec![1000, 20001, 50000, 0]);
    }

    #[test]
    fn float32_normalized_is_stretched() {
        let data = read_float32(
            &[0.0, 0.5, 1.0, 0.25],
            &|_hdu| {}
        ).unwrap();
        assert_eq!(data, vec![0, 32768, 65535, 16384]);
    }

    #[test]
    fn float32_out_of_counts_range_is_unsupported() {
        // The reproduction payload from VERIFICATION_ISSUES.md: physical values
        // beyond the u16 ADU range must be rejected, not clipped to white
        let err = match read_float32(
            &[1000.0, 20000.5, 50000.0, -50.0, 370000.0],
            &|_hdu| {}
        ) {
            Ok(_) => panic!("expected error"),
            Err(err) => err,
        };
        assert_eq!(err.kind(), ErrorKind::Unsupported);
    }

    #[test]
    fn float32_applies_bzero_bscale() {
        let data = read_float32(&[0.0, 1.0, 2.0], &|hdu| {
            hdu.set_f64("BSCALE", 100.0);
            hdu.set_f64("BZERO", 10.0);
        }).unwrap();
        assert_eq!(data, vec![10, 110, 210]);
    }

    #[test]
    fn float32_mapped_out_of_range_is_unsupported() {
        let err = match read_float32(&[0.0, 1.0, 2.0], &|hdu| {
            hdu.set_f64("BSCALE", 1.0e6);
        }) {
            Ok(_) => panic!("expected error"),
            Err(err) => err,
        };
        assert_eq!(err.kind(), ErrorKind::Unsupported);
    }

    #[test]
    fn float32_explicit_zero_bzero_is_identity() {
        let data = read_float32(&[0.0, 500.0], &|hdu| {
            hdu.set_f64("BZERO", 0.0);
        }).unwrap();
        assert_eq!(data, vec![0, 500]);
    }

    #[test]
    fn float32_nan_is_unsupported() {
        let err = match read_float32(&[0.0, f32::NAN], &|_hdu| {}) {
            Ok(_) => panic!("expected error"),
            Err(err) => err,
        };
        assert_eq!(err.kind(), ErrorKind::Unsupported);
    }

    #[test]
    fn float32_bad_bscale_is_invalid_data() {
        let err = match read_float32(&[0.0, 1.0], &|hdu| {
            hdu.set_f64("BSCALE", 0.0);
        }) {
            Ok(_) => panic!("expected error"),
            Err(err) => err,
        };
        assert_eq!(err.kind(), ErrorKind::InvalidData);
    }

    /// One-row image header of `len` pixels with the given BITPIX
    fn test_image_header(bitpix: i8, len: usize) -> Header {
        let mut hdu = Header::new();
        hdu.set_bool("SIMPLE", true);
        hdu.set_i64("BITPIX", i64::from(bitpix));
        hdu.set_i64("NAXIS", 2);
        hdu.set_i64("NAXIS1", len as i64);
        hdu.set_i64("NAXIS2", 1);
        hdu
    }

    /// Writes a card value verbatim, to exercise spellings `set_i64`/`set_f64` cannot produce
    fn set_raw_card(hdu: &mut Header, key: &str, raw: &str) {
        hdu.set_value_impl(key, raw.to_string());
    }

    /// Builds a file whose stored payload corresponds to `data` written with the pedestal
    /// `write_bzero`, so the caller can declare any mapping spelling over it (see `FitsWriter::write_data`)
    fn int_file(bitpix: i8, write_bzero: u16, data: &[u16], extra: &dyn Fn(&mut Header)) -> Vec<u8> {
        let mut hdu = test_image_header(bitpix, data.len());
        extra(&mut hdu);

        let mut out = Vec::new();
        let writer = FitsWriter::new();
        writer.write_header(&mut out, &hdu).unwrap();
        writer.write_data(bitpix, write_bzero, &mut out, data).unwrap();
        out
    }

    fn read_int(bitpix: i8, write_bzero: u16, data: &[u16], extra: &dyn Fn(&mut Header)) -> std::io::Result<Vec<u16>> {
        let mut cursor = Cursor::new(int_file(bitpix, write_bzero, data, extra));
        let reader = FitsReader::new(&mut cursor)?;
        let mut result = vec![0_u16; data.len()];
        FitsReader::read_data(&reader.headers[0], &mut cursor, 0, &mut result)?;
        Ok(result)
    }

    #[test]
    fn int16_bzero_forms_are_equivalent() {
        // The reproduction payload from VERIFICATION_ISSUES.md. The real-value spellings of
        // BZERO come from third-party writers, the plain integer one is what Astra writes today
        let payload = [32767_u16, 32768, 32769, 0, 65535];
        for form in ["32768.0", "32768.", "3.27680000000000E+04", "32768", " 32768.0 "] {
            let data = read_int(16, 32768, &payload, &|hdu| {
                set_raw_card(hdu, "BZERO", form);
            }).unwrap();
            assert_eq!(data, payload, "BZERO card `{form}` must round-trip");
        }
    }

    #[test]
    fn int16_bscale_is_applied() {
        // Scaled int16 data is signed two's complement; payload holds -100, 0, 1000
        let stored = [(-100_i16) as u16, 0, 1000];
        let data = read_int(16, 0, &stored, &|hdu| {
            set_raw_card(hdu, "BSCALE", "5.00000000000000E-01");
            hdu.set_f64("BZERO", 100.0);
        }).unwrap();
        assert_eq!(data, vec![50, 100, 600]);
    }

    #[test]
    fn int16_out_of_range_mapping_clips_instead_of_wrapping() {
        // The old modular `wrapping_add` turned 72767 into 7231; now it saturates
        let stored = [(-32768_i16) as u16, 0, 32767];
        let data = read_int(16, 0, &stored, &|hdu| {
            set_raw_card(hdu, "BZERO", "40000.0");
        }).unwrap();
        assert_eq!(data, vec![7232, 40000, 65535]);
    }

    #[test]
    fn int16_negative_mapping_clips_instead_of_wrapping() {
        let stored = [(-100_i16) as u16, 0, 16000];
        let data = read_int(16, 0, &stored, &|hdu| {
            hdu.set_f64("BSCALE", 2.0);
        }).unwrap();
        assert_eq!(data, vec![0, 0, 32000]);
    }

    #[test]
    fn int16_mapping_fully_above_adu_range_is_unsupported() {
        let err = match read_int(16, 0, &[0, 1], &|hdu| {
            set_raw_card(hdu, "BZERO", "100000.0");
        }) {
            Ok(_) => panic!("expected error"),
            Err(err) => err,
        };
        assert_eq!(err.kind(), ErrorKind::Unsupported);
    }

    #[test]
    fn int16_without_cards_keeps_unsigned_payload() {
        // Third-party `uint16` payloads written without a BZERO card must not be read as signed
        let payload = [0_u16, 40000, 65535];
        let data = read_int(16, 0, &payload, &|_hdu| {}).unwrap();
        assert_eq!(data, payload);
    }

    #[test]
    fn int16_identity_cards_keep_unsigned_payload() {
        // `BSCALE = 1.` declares nothing; only a mapping that shifts values may imply signed
        // storage (see `intrinsic_range`)
        let payload = [0_u16, 40000, 65535];
        let data = read_int(16, 0, &payload, &|hdu| {
            set_raw_card(hdu, "BSCALE", "1.");
        }).unwrap();
        assert_eq!(data, payload);
    }

    #[test]
    fn int16_unparsable_bzero_is_treated_as_absent() {
        let payload = [0_u16, 1000, 65535];
        let data = read_int(16, 0, &payload, &|hdu| {
            set_raw_card(hdu, "BZERO", "'32768'");
        }).unwrap();
        assert_eq!(data, payload);
    }

    #[test]
    fn int16_negative_bscale_is_applied() {
        // A negative scale swaps the mapping extremes; payload holds -100, 0, 1000
        let stored = [(-100_i16) as u16, 0, 1000];
        let data = read_int(16, 0, &stored, &|hdu| {
            hdu.set_f64("BSCALE", -0.5);
            hdu.set_f64("BZERO", 500.0);
        }).unwrap();
        assert_eq!(data, vec![550, 500, 0]);
    }

    #[test]
    fn int16_fractional_bzero_is_applied() {
        let data = read_int(16, 0, &[0, 10, 20], &|hdu| {
            set_raw_card(hdu, "BZERO", "0.5");
        }).unwrap();
        assert_eq!(data, vec![1, 11, 21]);
    }

    #[test]
    fn int16_per_plane_reads_share_the_mapping() {
        // `io.rs` reads the R/G/B planes of a triplet file from one HDU by byte offset
        let payload = [1_u16, 2, 30000, 65535];
        let file = int_file(16, 32768, &payload, &|hdu| {
            set_raw_card(hdu, "BZERO", "32768.0");
        });
        let mut cursor = Cursor::new(file);
        let reader = FitsReader::new(&mut cursor).unwrap();
        let hdu = &reader.headers[0];

        let mut all = vec![0_u16; 2];
        FitsReader::read_data(hdu, &mut cursor, 0, &mut all).unwrap();
        let mut plane = vec![0_u16; 2];
        let plane_bytes = hdu.bytes_per_value() * 2;
        FitsReader::read_data(hdu, &mut cursor, plane_bytes, &mut plane).unwrap();

        all.extend_from_slice(&plane);
        assert_eq!(all, payload);
    }

    #[test]
    fn int8_bscale_is_applied() {
        let data = read_int(8, 0, &[0, 10, 255], &|hdu| {
            hdu.set_f64("BSCALE", 2.0);
        }).unwrap();
        assert_eq!(data, vec![0, 20, 510]);
    }

    #[test]
    fn int8_negative_bzero_is_clipped_not_wrapped() {
        // Signed byte data (BZERO = -128): physical values below 0 must land on 0, not wrap to white
        let data = read_int(8, 0, &[10, 128, 255], &|hdu| {
            set_raw_card(hdu, "BZERO", "-128.0");
        }).unwrap();
        assert_eq!(data, vec![0, 0, 127]);
    }

    #[test]
    fn int16_float_form_bzero_covers_the_whole_adu_range() {
        let data = read_int(16, 32768, &[0, 1000, 65535], &|hdu| {
            set_raw_card(hdu, "BZERO", "32768.0");
        }).unwrap();
        assert_eq!(data, vec![0, 1000, 65535]);
    }

    #[test]
    fn int16_nan_bzero_is_invalid_data() {
        let err = match read_int(16, 0, &[0, 1], &|hdu| {
            set_raw_card(hdu, "BZERO", "NaN");
        }) {
            Ok(_) => panic!("expected error"),
            Err(err) => err,
        };
        assert_eq!(err.kind(), ErrorKind::InvalidData);
    }

    #[test]
    fn int16_bscale_zero_is_invalid_data() {
        let err = match read_int(16, 0, &[0, 1], &|hdu| {
            set_raw_card(hdu, "BSCALE", "0");
        }) {
            Ok(_) => panic!("expected error"),
            Err(err) => err,
        };
        assert_eq!(err.kind(), ErrorKind::InvalidData);
    }
}
