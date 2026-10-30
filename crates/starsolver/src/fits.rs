//! Minimal FITS reader: only header cards, BINTABLE columns and the endianness
//! of raw (`'A'`-typed) blobs. No `CONTINUE` support: our files do not use it.

use anyhow::{Context, Result, bail, ensure};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

/// FITS block size, bytes.
pub const BLOCK_SIZE: usize = 2880;
/// Header card size, bytes.
pub const CARD_SIZE: usize = 80;

/// Byte order of binary data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Endian {
    Big,
    Little,
}

impl Endian {
    /// Decodes the `ENDIAN` key (the bytes of `0x01020304` in write order):
    /// `04:03:02:01` is little-endian, `01:02:03:04` big-endian.
    pub fn from_key(s: &str) -> Result<Self> {
        match s.trim() {
            "04:03:02:01" => Ok(Endian::Little),
            "01:02:03:04" => Ok(Endian::Big),
            other => bail!("unsupported ENDIAN value {other:?}"),
        }
    }

    /// From the primary header; a missing key means classic FITS (e.g. `.xyls`),
    /// which is big-endian.
    pub fn from_header(header: &Header) -> Result<Self> {
        match header.get_str("ENDIAN") {
            Some(s) => Self::from_key(s),
            None => {
                // A present-but-unquoted `ENDIAN` must not fall to the big-endian default.
                ensure!(
                    header.get("ENDIAN").is_none(),
                    "ENDIAN key is present but not a quoted string"
                );
                Ok(Endian::Big)
            }
        }
    }

    pub fn read_u16(self, b: &[u8]) -> u16 {
        let a: [u8; 2] = b[..2].try_into().expect("short slice");
        match self {
            Endian::Big => u16::from_be_bytes(a),
            Endian::Little => u16::from_le_bytes(a),
        }
    }

    pub fn read_u32(self, b: &[u8]) -> u32 {
        let a: [u8; 4] = b[..4].try_into().expect("short slice");
        match self {
            Endian::Big => u32::from_be_bytes(a),
            Endian::Little => u32::from_le_bytes(a),
        }
    }

    pub fn read_f64(self, b: &[u8]) -> f64 {
        let a: [u8; 8] = b[..8].try_into().expect("short slice");
        match self {
            Endian::Big => f64::from_be_bytes(a),
            Endian::Little => f64::from_le_bytes(a),
        }
    }
}

/// A parsed header-card value.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
}

impl Value {
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Bool(b) => Some(*b),
            _ => None,
        }
    }

    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Value::Int(i) => Some(*i),
            _ => None,
        }
    }

    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Value::Float(f) => Some(*f),
            Value::Int(i) => Some(*i as f64),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(s) => Some(s.as_str()),
            _ => None,
        }
    }
}

/// FITS header: ordered cards, `COMMENT`/`HISTORY` cards are stored with no value.
#[derive(Debug, Clone, Default)]
pub struct Header {
    cards: Vec<(String, Option<Value>)>,
}

impl Header {
    fn push(&mut self, key: &str, value: Option<Value>) {
        self.cards.push((key.to_string(), value));
    }

    /// Last card with this key that actually has a value.
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.cards
            .iter()
            .rev()
            .find(|(k, v)| k == key && v.is_some())
            .and_then(|(_, v)| v.as_ref())
    }

    pub fn get_bool(&self, key: &str) -> Option<bool> {
        self.get(key).and_then(Value::as_bool)
    }

    pub fn get_i64(&self, key: &str) -> Option<i64> {
        self.get(key).and_then(Value::as_i64)
    }

    pub fn get_f64(&self, key: &str) -> Option<f64> {
        self.get(key).and_then(Value::as_f64)
    }

    pub fn get_str(&self, key: &str) -> Option<&str> {
        self.get(key).and_then(Value::as_str)
    }

    pub fn require_i64(&self, key: &str) -> Result<i64> {
        self.get_i64(key)
            .ok_or_else(|| anyhow::anyhow!("header key {key} is missing or not an integer"))
    }

    pub fn require_f64(&self, key: &str) -> Result<f64> {
        self.get_f64(key)
            .ok_or_else(|| anyhow::anyhow!("header key {key} is missing or not a number"))
    }

    pub fn require_str(&self, key: &str) -> Result<&str> {
        self.get_str(key)
            .ok_or_else(|| anyhow::anyhow!("header key {key} is missing or not a string"))
    }
}

fn parse_value(key: &str, field: &[u8]) -> Result<Option<Value>> {
    // Lenient: a non-UTF-8 field is dropped, not an error, so odd cards never abort parsing.
    let Ok(s) = std::str::from_utf8(field) else {
        return Ok(None);
    };
    let s = s.trim();
    if s.is_empty() {
        return Ok(None);
    }
    if let Some(rest) = s.strip_prefix('\'') {
        // Scan to the closing quote; a doubled '' is an escaped literal quote
        // (`'it''s'` -> `it's`). Unterminated must bail: a silent key drop would
        // surface later as a misleading "missing key".
        let mut out = String::new();
        let mut chars = rest.chars().peekable();
        let terminated = loop {
            match chars.next() {
                None => break false,
                Some('\'') => {
                    if chars.peek() == Some(&'\'') {
                        out.push('\'');
                        chars.next();
                    } else {
                        break true;
                    }
                }
                Some(c) => out.push(c),
            }
        };
        ensure!(terminated, "unterminated string value for key {key}");
        return Ok(Some(Value::Str(out.trim_end().to_string())));
    }
    // A '/' outside quotes starts a comment: keep only the head before it.
    let tok = s.split_once('/').map_or(s, |(head, _)| head).trim();
    match tok {
        "T" => return Ok(Some(Value::Bool(true))),
        "F" => return Ok(Some(Value::Bool(false))),
        _ => {}
    }
    if let Ok(i) = tok.parse::<i64>() {
        return Ok(Some(Value::Int(i)));
    }
    // FITS real values may use D/E exponents.
    let normalized = tok.replace('D', "E").replace('d', "e");
    if let Ok(f) = normalized.parse::<f64>() {
        return Ok(Some(Value::Float(f)));
    }
    Ok(None)
}

/// Parses one 80-byte card.
fn parse_card(card: &[u8]) -> Result<(String, Option<Value>)> {
    let raw_key = std::str::from_utf8(&card[..8]).unwrap_or("        ").trim();
    let key = raw_key.to_string();
    if card[8..10] != *b"= " {
        // COMMENT / HISTORY / blank / continuation cards.
        return Ok((key, None));
    }
    let value = parse_value(&key, &card[10..CARD_SIZE])?;
    Ok((key, value))
}

/// Reads a header starting at `start`; returns it and the offset of the
/// (padded) block following the header.
fn parse_header(buf: &[u8], start: usize) -> Result<(Header, usize)> {
    let mut header = Header::default();
    let mut off = start;
    loop {
        ensure!(
            off + CARD_SIZE <= buf.len(),
            "truncated FITS header at byte {off}"
        );
        let card = &buf[off..off + CARD_SIZE];
        off += CARD_SIZE;
        let (key, value) = parse_card(card)?;
        if key == "END" && value.is_none() {
            return Ok((header, off.div_ceil(BLOCK_SIZE) * BLOCK_SIZE));
        }
        header.push(&key, value);
    }
}

/// One HDU: its header plus the exact slice of its data area.
#[derive(Debug)]
pub struct Hdu {
    pub header: Header,
    data_off: usize,
    data_len: usize,
}

/// Data size in bytes described by a header (padding not included).
fn data_size(header: &Header) -> Result<usize> {
    let naxis = header.get_i64("NAXIS").unwrap_or(0);
    if naxis <= 0 {
        return Ok(0);
    }
    // The FITS standard caps NAXIS at 999; a hostile value must not spin here.
    ensure!(
        naxis <= 999,
        "NAXIS {naxis} exceeds the FITS maximum of 999"
    );
    let mut elems: u64 = 1;
    for i in 1..=naxis {
        let n = header.get_i64(&format!("NAXIS{i}")).unwrap_or(0);
        ensure!(n >= 0, "negative NAXIS{i}");
        let n = u64::try_from(n).map_err(|_| anyhow::anyhow!("NAXIS{i} too large"))?;
        elems = elems
            .checked_mul(n)
            .ok_or_else(|| anyhow::anyhow!("NAXIS products overflow"))?;
    }
    let bitpix = header.get_i64("BITPIX").unwrap_or(8);
    ensure!(
        matches!(bitpix, 8 | 16 | 32 | 64 | -32 | -64),
        "unsupported BITPIX {bitpix}"
    );
    // BINTABLE-only: GCOUNT/PCOUNT clamped, PCOUNT added as a raw byte count
    // (valid only for extended tables; typed groups unsupported). Math stays in
    // u64 so a 64-bit GCOUNT/PCOUNT cannot truncate on a 32-bit target.
    let gcount = u64::try_from(header.get_i64("GCOUNT").unwrap_or(1).max(1))
        .map_err(|_| anyhow::anyhow!("GCOUNT too large"))?;
    let pcount = u64::try_from(header.get_i64("PCOUNT").unwrap_or(0).max(0))
        .map_err(|_| anyhow::anyhow!("PCOUNT too large"))?;
    let bytes = elems
        .checked_mul(bitpix.unsigned_abs() / 8)
        .and_then(|b| b.checked_mul(gcount))
        .and_then(|b| b.checked_add(pcount))
        .ok_or_else(|| anyhow::anyhow!("data area size overflow"))?;
    usize::try_from(bytes).map_err(|_| anyhow::anyhow!("data area size exceeds the address space"))
}

/// Byte storage behind [`Fits`], `Arc`-shared with every lazy [`BlobView`] kept
/// after the `Fits` is dropped.
///
/// Real files are mmap'd: a multi-gigabyte index must never be copied into
/// anonymous memory, and mmap pages (demand-paged, clean, evictable under
/// pressure) keep private unreclaimable memory independent of file size
/// (the < 200 MB peak RSS target).
#[derive(Clone)]
pub enum Bytes {
    Mmap(std::sync::Arc<memmap2::Mmap>),
    /// Bytes the process owns instead of a file: crafted test files
    /// ([`Fits::from_bytes`]) and the compact hint-filtered codes tree
    /// ([`KdTree::filtered`](crate::kdtree::KdTree::filtered)), a few thousand
    /// records, well inside the RSS budget that keeps real index tables mapped.
    Owned(std::sync::Arc<[u8]>),
}

impl Bytes {
    #[must_use]
    pub fn as_slice(&self) -> &[u8] {
        match self {
            Bytes::Mmap(m) => m,
            Bytes::Owned(v) => v,
        }
    }
}

impl std::fmt::Debug for Bytes {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (kind, len) = match self {
            Bytes::Mmap(m) => ("mmap", m.len()),
            Bytes::Owned(v) => ("owned", v.len()),
        };
        write!(f, "Bytes({kind}, {len} bytes)")
    }
}

/// A lazy view over the data area of one BINTABLE, sharing the [`Bytes`]
/// storage with the [`Fits`] it came from. Elements are decoded on access (the
/// kd-tree data, `lr`, `split` and `quads` arrays), so no index-sized buffer is
/// materialized and untouched file regions stay on disk.
///
/// Two layouts, one per accessor that builds a view:
/// * first-blob-column packing (`Fits::blob_view`: `col_off`/`col_bytes` select
///   the column; padding skipped, so element `i` sits at `i * element size` of
///   the packed column bytes);
/// * whole-row packing (`Fits::raw_view`: `col_off = 0`, `col_bytes = row_len`),
///   equivalent to chunking the raw table bytes.
#[derive(Debug, Clone)]
pub struct BlobView {
    bytes: Bytes,
    /// Absolute offset of row 0 in `bytes`.
    data_off: usize,
    row_len: usize,
    col_off: usize,
    col_bytes: usize,
    nrows: usize,
    /// The column fills the whole row at offset 0, so element reads are a
    /// single multiply-add (the hot kd-tree data path).
    flat: bool,
}

impl BlobView {
    #[must_use]
    pub fn nrows(&self) -> usize {
        self.nrows
    }

    /// Total packed payload: the bytes the element accessor walks over (column
    /// bytes for a blob view, whole rows for a raw view).
    ///
    /// Cannot overflow: `Table::new` checks `nrows * row_len` against the data
    /// length and each column fits its row, bounding the product by file size.
    #[must_use]
    pub fn payload_bytes(&self) -> usize {
        self.nrows * self.col_bytes
    }

    /// Number of `u32` entries: the bytes read back-to-back as a packed array,
    /// so the per-row width must be a positive multiple of 4.
    pub fn u32_entries(&self) -> Result<usize> {
        ensure!(
            self.col_bytes > 0 && usize::is_multiple_of(self.col_bytes, 4),
            "blob column is {} bytes, not a positive multiple of 4",
            self.col_bytes
        );
        Ok(self.payload_bytes() / 4)
    }

    fn at(&self, byte: usize, len: usize) -> &[u8] {
        debug_assert!(self.col_bytes > 0, "element read on a zero-width column");
        let off = if self.flat {
            self.data_off + byte
        } else {
            self.data_off
                + byte / self.col_bytes * self.row_len
                + self.col_off
                + byte % self.col_bytes
        };
        &self.bytes.as_slice()[off..off + len]
    }

    /// The `idx`-th packed unsigned integer (`size` 2 or 4). Blob payloads are
    /// memory images: decoded by the file's `ENDIAN` key, not typed columns' big-endian.
    #[must_use]
    pub fn uint_at(&self, endian: Endian, size: usize, idx: usize) -> u32 {
        let b = self.at(idx * size, size);
        match size {
            2 => u32::from(endian.read_u16(b)),
            4 => endian.read_u32(b),
            // `IntKind::size()` yields 2/4 and current callers pass a literal 4.
            other => unreachable!("uint_at element size {other} (only 2 or 4)"),
        }
    }

    /// Whether element reads are the single multiply-add layout (no row walk).
    #[must_use]
    pub fn is_flat(&self) -> bool {
        self.flat
    }

    /// The whole data area as one slice (flat views only; the tiny fixed-size
    /// kd-tree `range` tables and the flat little-endian coordinate fast paths).
    #[must_use]
    pub fn raw_bytes(&self) -> &[u8] {
        debug_assert!(self.flat);
        &self.bytes.as_slice()[self.data_off..self.data_off + self.nrows * self.row_len]
    }

    /// A flat little-endian view over bytes a run-time kd-tree build writes
    /// instead of a file ([`KdTree::filtered`]). `row_len` (>= 1) gives
    /// `bytes.len() / row_len` rows read through the same accessors as a mapped
    /// view; flat + little-endian keeps searches on their fast decode paths.
    ///
    /// [`KdTree::filtered`]: crate::kdtree::KdTree::filtered
    #[must_use]
    pub(crate) fn owned(bytes: Vec<u8>, row_len: usize) -> Self {
        debug_assert!(row_len >= 1, "owned view with a zero-width row");
        let nrows = bytes.len() / row_len;
        Self {
            bytes: Bytes::Owned(bytes.into()),
            data_off: 0,
            row_len,
            col_off: 0,
            col_bytes: row_len,
            nrows,
            flat: true,
        }
    }
}

/// A BINTABLE extension: `XTENSION = 'BINTABLE'` (already trimmed by
/// `parse_value`) with `BITPIX = 8`, the only blob layout understood here.
/// Rejecting another BITPIX early beats a later size mismatch.
fn is_bintable(header: &Header) -> bool {
    header.get_i64("BITPIX") == Some(8)
        && header
            .get_str("XTENSION")
            .is_some_and(|x| x.starts_with("BINTABLE"))
}

/// A FITS file mapped into memory, with parsed HDU headers. Table data is
/// read lazily through [`BlobView`]; see [`Bytes`] for why this is mmap.
#[derive(Debug)]
pub struct Fits {
    bytes: Bytes,
    hdus: Vec<Hdu>,
    endian: Endian,
}

impl Fits {
    pub fn open(path: &Path) -> Result<Self> {
        let file =
            File::open(path).map_err(|e| anyhow::anyhow!("cannot read {}: {e}", path.display()))?;
        let md = file
            .metadata()
            .map_err(|e| anyhow::anyhow!("cannot read {}: {e}", path.display()))?;
        // `File::open` succeeds on a directory, where mmap fails with a cryptic ENODEV.
        ensure!(md.is_file(), "{} is not a regular file", path.display());
        // `Mmap::map` rejects an empty file with a bare EINVAL, and anything
        // shorter than a BLOCK_SIZE cannot parse: reject both up front.
        ensure!(
            md.len() >= BLOCK_SIZE as u64,
            "file is too small to be FITS: {}",
            path.display()
        );
        // Assumes no truncation/rewrite while reading (index files are read-only
        // here); mmap pins the inode, so the map outlives `file`.
        let map = unsafe { memmap2::Mmap::map(&file) }
            .map_err(|e| anyhow::anyhow!("cannot mmap {}: {e}", path.display()))?;
        Self::from_storage(Bytes::Mmap(std::sync::Arc::new(map)))
            .with_context(|| format!("parsing FITS file {}", path.display()))
    }

    /// A file already in memory (crafted test files).
    #[cfg(test)]
    pub fn from_bytes(buf: Vec<u8>) -> Result<Self> {
        Self::from_storage(Bytes::Owned(buf.into()))
    }

    fn from_storage(bytes: Bytes) -> Result<Self> {
        let buf = bytes.as_slice();
        ensure!(buf.len() >= BLOCK_SIZE, "file is too small to be FITS");
        let mut hdus = Vec::new();
        let mut off = 0usize;
        while off + BLOCK_SIZE <= buf.len() {
            // Stop on trailing padding instead of misparsing it as a header.
            let first = std::str::from_utf8(&buf[off..off + 8]).unwrap_or(" ");
            if first.trim() != "SIMPLE" && first.trim() != "XTENSION" {
                break;
            }
            let (header, data_off) = parse_header(buf, off)
                .with_context(|| format!("HDU {} at byte {off}", hdus.len()))?;
            let len = data_size(&header)?;
            let end = data_off
                .checked_add(len)
                .ok_or_else(|| anyhow::anyhow!("data area size overflow in HDU {}", hdus.len()))?;
            ensure!(
                end <= buf.len(),
                "HDU {} data area ({len} bytes at {data_off}) runs past the end of file ({} bytes)",
                hdus.len(),
                buf.len()
            );
            let padded = end.div_ceil(BLOCK_SIZE) * BLOCK_SIZE;
            hdus.push(Hdu {
                header,
                data_off,
                data_len: len,
            });
            off = padded;
        }
        ensure!(!hdus.is_empty(), "no FITS HDUs found");
        let endian = Endian::from_header(&hdus[0].header)?;
        Ok(Self {
            bytes,
            hdus,
            endian,
        })
    }

    pub fn primary(&self) -> &Header {
        &self.hdus[0].header
    }

    pub fn endian(&self) -> Endian {
        self.endian
    }

    fn hdu_data(&self, hdu: &Hdu) -> &[u8] {
        &self.bytes.as_slice()[hdu.data_off..hdu.data_off + hdu.data_len]
    }

    /// Index of the BINTABLE whose first column is named `ttype`.
    pub fn find_table(&self, ttype: &str) -> Option<usize> {
        self.hdus.iter().position(|h| {
            let is_table = is_bintable(&h.header);
            is_table && h.header.get_str("TTYPE1") == Some(ttype)
        })
    }

    /// First BINTABLE holding a column named `name` (any position, unlike
    /// `find_table`) as `(HDU index, 1-based column index)`, giving callers a
    /// clearer error than "no such column".
    pub fn find_column(&self, name: &str) -> Option<(usize, usize)> {
        self.hdus.iter().enumerate().find_map(|(hdu, h)| {
            if !is_bintable(&h.header) {
                return None;
            }
            let nfields = h.header.get_i64("TFIELDS").unwrap_or(0);
            let nfields = usize::try_from(nfields).unwrap_or(0);
            // Reject-biased bound mirroring `Table::new` (this is the reject path):
            // every column takes >=1 row byte, so a hostile `TFIELDS` cannot spin this.
            let row_len = usize::try_from(h.header.get_i64("NAXIS1").unwrap_or(0)).unwrap_or(0);
            let nfields = usize::min(nfields, usize::max(row_len, 1));
            (1..=nfields)
                .find(|i| h.header.get_str(&format!("TTYPE{i}")) == Some(name))
                .map(|col| (hdu, col))
        })
    }

    /// Header of the HDU whose first column is named `ttype`.
    pub fn header_by_ttype(&self, ttype: &str) -> Result<&Header> {
        let i = self
            .find_table(ttype)
            .ok_or_else(|| anyhow::anyhow!("no BINTABLE column named {ttype:?}"))?;
        Ok(&self.hdus[i].header)
    }

    pub fn table_by_ttype(&self, ttype: &str) -> Result<Table<'_>> {
        let i = self
            .find_table(ttype)
            .ok_or_else(|| anyhow::anyhow!("no BINTABLE column named {ttype:?}"))?;
        self.table(i)
    }

    /// Opens HDU `i` as a BINTABLE.
    pub fn table(&self, i: usize) -> Result<Table<'_>> {
        let hdu = self
            .hdus
            .get(i)
            .ok_or_else(|| anyhow::anyhow!("no HDU {i} in file"))?;
        ensure!(
            is_bintable(&hdu.header),
            "HDU {i} is not a BINTABLE extension"
        );
        Table::new(&hdu.header, self.hdu_data(hdu))
            .with_context(|| format!("bad BINTABLE in HDU {i}"))
    }

    /// Names of all BINTABLE columns of the file (`TTYPE1` of every table HDU).
    pub fn table_names(&self) -> Vec<&str> {
        self.hdus
            .iter()
            .filter(|h| is_bintable(&h.header))
            .filter_map(|h| h.header.get_str("TTYPE1"))
            .collect()
    }

    /// A lazy [`BlobView`] over the first (blob) column of the `ttype` table,
    /// its per-row elements packed back-to-back with the row padding skipped.
    pub fn blob_view(&self, ttype: &str) -> Result<BlobView> {
        let i = self
            .find_table(ttype)
            .ok_or_else(|| anyhow::anyhow!("no BINTABLE column named {ttype:?}"))?;
        let t = self.table(i)?;
        let first = t
            .columns
            .first()
            .ok_or_else(|| anyhow::anyhow!("table {ttype:?} has no columns"))?;
        let Form::Blob(col_bytes) = first.form else {
            bail!(
                "column {:?} of table {ttype:?} is not a raw blob",
                first.name
            );
        };
        Ok(BlobView {
            bytes: self.bytes.clone(),
            data_off: self.hdus[i].data_off,
            row_len: t.row_len,
            col_off: first.offset,
            col_bytes,
            nrows: t.nrows,
            flat: first.offset == 0 && col_bytes == t.row_len,
        })
    }

    /// A lazy [`BlobView`] over the whole row area of the `ttype` table
    /// (element packing identical to chunking the raw table bytes).
    pub fn raw_view(&self, ttype: &str) -> Result<BlobView> {
        let i = self
            .find_table(ttype)
            .ok_or_else(|| anyhow::anyhow!("no BINTABLE column named {ttype:?}"))?;
        let t = self.table(i)?;
        Ok(BlobView {
            bytes: self.bytes.clone(),
            data_off: self.hdus[i].data_off,
            row_len: t.row_len,
            col_off: 0,
            col_bytes: t.row_len,
            nrows: t.nrows,
            flat: true,
        })
    }
}

/// Header access shared by [`Fits`] and [`FitsHeaders`], so checks that need no
/// table data (the index metadata) can run on either.
pub trait Headers {
    fn primary(&self) -> &Header;

    fn endian(&self) -> Endian;

    /// Names of the first column (`TTYPE1`) of every BINTABLE of the file.
    fn table_names(&self) -> Vec<&str>;

    /// Header of the HDU whose first column is named `ttype`.
    fn header_by_ttype(&self, ttype: &str) -> Result<&Header>;
}

impl Headers for Fits {
    fn primary(&self) -> &Header {
        Fits::primary(self)
    }

    fn endian(&self) -> Endian {
        Fits::endian(self)
    }

    fn table_names(&self) -> Vec<&str> {
        Fits::table_names(self)
    }

    fn header_by_ttype(&self, ttype: &str) -> Result<&Header> {
        Fits::header_by_ttype(self, ttype)
    }
}

/// The headers of a FITS file without any of its data.
///
/// Reading the metadata of every index of a directory must not read the
/// gigabytes of kd-tree data after the headers, so this reader walks header
/// blocks and seeks over each data area.
#[derive(Debug)]
pub struct FitsHeaders {
    headers: Vec<Header>,
    endian: Endian,
}

impl FitsHeaders {
    /// The error is not contextualized with the path: the caller prints it with the file name.
    pub fn open(path: &Path) -> Result<Self> {
        let mut file =
            File::open(path).map_err(|e| anyhow::anyhow!("cannot read {}: {e}", path.display()))?;
        Self::from_reader(&mut file)
    }

    fn from_reader<R: Read + Seek>(reader: &mut R) -> Result<Self> {
        let headers = scan_headers(reader)?;
        ensure!(!headers.is_empty(), "no FITS HDUs found");
        let endian = Endian::from_header(&headers[0])?;
        Ok(Self { headers, endian })
    }

    /// Header-only view of bytes already in memory.
    #[cfg(test)]
    pub(crate) fn from_bytes(buf: Vec<u8>) -> Result<Self> {
        Self::from_reader(&mut std::io::Cursor::new(buf))
    }

    /// Header of HDU `index` (0 is the primary).
    pub fn header_at(&self, index: usize) -> Option<&Header> {
        self.headers.get(index)
    }
}

impl Headers for FitsHeaders {
    fn primary(&self) -> &Header {
        // `from_reader` guarantees a non-empty list, like `Fits::primary`.
        &self.headers[0]
    }

    fn endian(&self) -> Endian {
        self.endian
    }

    fn table_names(&self) -> Vec<&str> {
        self.headers
            .iter()
            .filter(|h| is_bintable(h))
            .filter_map(|h| h.get_str("TTYPE1"))
            .collect()
    }

    fn header_by_ttype(&self, ttype: &str) -> Result<&Header> {
        self.headers
            .iter()
            .find(|h| is_bintable(h) && h.get_str("TTYPE1") == Some(ttype))
            .ok_or_else(|| anyhow::anyhow!("no BINTABLE column named {ttype:?}"))
    }
}

/// A `BLOCK_SIZE` read: a full block, a truncated file's short tail, or EOF.
enum Chunk {
    Full,
    Short,
    Eof,
}

fn read_chunk<R: Read>(reader: &mut R, block: &mut [u8; BLOCK_SIZE]) -> Result<Chunk> {
    let mut filled = 0usize;
    while filled < block.len() {
        match reader.read(&mut block[filled..])? {
            0 => break,
            n => filled += n,
        }
    }
    Ok(match filled {
        0 => Chunk::Eof,
        BLOCK_SIZE => Chunk::Full,
        _ => Chunk::Short,
    })
}

/// Walks the HDU headers of a FITS stream, seeking over every data area.
/// [`Fits::from_storage`] (also behind the test-only `Fits::from_bytes`) is its
/// twin: the stop rule and header/data size math are shared, any change belongs
/// to both. Generic over `Read + Seek` so tests can run it on a `Cursor`.
fn scan_headers<R: Read + Seek>(reader: &mut R) -> Result<Vec<Header>> {
    let mut headers: Vec<Header> = Vec::new();
    let mut block = [0u8; BLOCK_SIZE];
    // Byte offset of the block most recently read.
    let mut off: u64 = 0;
    loop {
        match read_chunk(reader, &mut block)? {
            Chunk::Eof => break,
            Chunk::Short => bail!("file is too small to be FITS"),
            Chunk::Full => {}
        }
        // Only a block starting with SIMPLE or XTENSION opens another HDU: the
        // padding after the last HDU does not (and neither does a non-FITS file).
        let starts_hdu = {
            let first = std::str::from_utf8(&block[..8]).unwrap_or(" ").trim();
            first == "SIMPLE" || first == "XTENSION"
        };
        if !starts_hdu {
            break;
        }
        let mut header = Header::default();
        'cards: loop {
            for (i, card) in block.as_chunks::<CARD_SIZE>().0.iter().enumerate() {
                let at = off + (i * CARD_SIZE) as u64;
                let (key, value) =
                    parse_card(card).with_context(|| format!("card at byte {at}"))?;
                if key == "END" && value.is_none() {
                    break 'cards;
                }
                header.push(&key, value);
            }
            off += BLOCK_SIZE as u64;
            match read_chunk(reader, &mut block)? {
                Chunk::Full => {}
                _ => bail!("truncated FITS header at byte {off}"),
            }
        }
        // The END block is consumed whole, so data starts on the next block boundary.
        off += BLOCK_SIZE as u64;
        let len = data_size(&header).with_context(|| format!("HDU {} header", headers.len()))?;
        let skip = (len as u64).div_ceil(BLOCK_SIZE as u64) * BLOCK_SIZE as u64;
        reader
            .seek(SeekFrom::Current(skip as i64))
            .with_context(|| format!("skipping the data of HDU {}", headers.len()))?;
        off += skip;
        headers.push(header);
    }
    Ok(headers)
}

/// Numeric FITS column kinds we support.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NumKind {
    U8,
    I16,
    I32,
    I64,
    F32,
    F64,
}

impl NumKind {
    pub fn size(self) -> usize {
        match self {
            NumKind::U8 => 1,
            NumKind::I16 => 2,
            NumKind::I32 => 4,
            NumKind::I64 => 8,
            NumKind::F32 => 4,
            NumKind::F64 => 8,
        }
    }
}

/// Parsed `TFORMn`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Form {
    /// `'A'`: a raw blob of `n` bytes holding a memory image (its elements
    /// follow the file's `ENDIAN` key, not the big-endian typed layout).
    Blob(usize),
    Num {
        count: usize,
        kind: NumKind,
    },
}

impl Form {
    fn parse(tform: &str) -> Result<Form> {
        let digits: String = tform.chars().take_while(|c| c.is_ascii_digit()).collect();
        let count: usize = if digits.is_empty() {
            1
        } else {
            digits
                .parse()
                .map_err(|e| anyhow::anyhow!("bad TFORM {tform:?}: {e}"))?
        };
        let kind_char = tform[digits.len()..].trim().chars().next();
        let kind = match kind_char {
            Some('A') => return Ok(Form::Blob(count)),
            Some('B') => NumKind::U8,
            Some('I') => NumKind::I16,
            Some('J') => NumKind::I32,
            Some('K') => NumKind::I64,
            Some('E') => NumKind::F32,
            Some('D') => NumKind::F64,
            other => bail!("unsupported TFORM {tform:?} (type {other:?})"),
        };
        Ok(Form::Num { count, kind })
    }

    /// Bytes this column occupies in a row.
    ///
    /// Checked so a hostile `TFORM` count (e.g. `2^62D`) cannot overflow the
    /// row-size arithmetic; `Table::new` validates offsets against `row_len`.
    pub fn nbytes(&self) -> Result<usize> {
        match self {
            Form::Blob(n) => Ok(*n),
            Form::Num { count, kind } => count
                .checked_mul(kind.size())
                .ok_or_else(|| anyhow::anyhow!("TFORM count {count} overflows the column size")),
        }
    }
}

fn align_up(x: usize, align: usize) -> usize {
    if align <= 1 {
        x
    } else {
        x.div_ceil(align) * align
    }
}

/// One column of a BINTABLE: its name, `TFORM` and offset inside a row.
#[derive(Debug)]
pub struct Column {
    pub name: String,
    pub form: Form,
    pub offset: usize,
}

impl Column {
    /// Raw bytes of this column in a row.
    fn bytes<'a>(&self, row: &'a [u8]) -> Result<&'a [u8]> {
        let end = self
            .offset
            .checked_add(self.form.nbytes()?)
            .ok_or_else(|| anyhow::anyhow!("column {:?} size overflow", self.name))?;
        row.get(self.offset..end)
            .ok_or_else(|| anyhow::anyhow!("column {:?} runs past the row", self.name))
    }

    /// `idx`-th value of a numeric column.
    ///
    /// Typed FITS columns are always big-endian (the standard's fixed byte
    /// order), unlike `'A'` blobs, whose order follows the `ENDIAN` key.
    ///
    /// Returns the raw storage value: quantization (`TZERO + TSCAL*n`) and
    /// `TNULL` are not applied, so such columns are rejected in `Table::new`.
    pub fn f64_at(&self, row: &[u8], idx: usize) -> Result<f64> {
        let Form::Num { count, kind } = self.form else {
            bail!("column {:?} is a raw blob, not a numeric column", self.name);
        };
        ensure!(
            idx < count,
            "column {:?}: index {idx} out of {count}",
            self.name
        );
        let kind_size = kind.size();
        // `bytes.len() == count * kind_size` and `idx < count`: in bounds, no overflow.
        let bytes = self.bytes(row)?;
        let b = &bytes[idx * kind_size..(idx + 1) * kind_size];
        Ok(match kind {
            NumKind::U8 => b[0] as f64,
            NumKind::I16 => i16::from_be_bytes(b[..2].try_into()?) as f64,
            NumKind::I32 => i32::from_be_bytes(b[..4].try_into()?) as f64,
            NumKind::I64 => i64::from_be_bytes(b[..8].try_into()?) as f64,
            NumKind::F32 => f32::from_be_bytes(b[..4].try_into()?) as f64,
            NumKind::F64 => f64::from_be_bytes(b[..8].try_into()?),
        })
    }
}

/// A BINTABLE view over the data area of one HDU.
#[derive(Debug)]
pub struct Table<'a> {
    pub row_len: usize,
    pub nrows: usize,
    pub columns: Vec<Column>,
    data: &'a [u8],
}

impl<'a> Table<'a> {
    fn new(header: &'a Header, data: &'a [u8]) -> Result<Self> {
        // A BINTABLE is 2-D (NAXIS1 = row length, NAXIS2 = row count), so
        // NAXIS < 2 cannot let a negative NAXIS2 slip past `data_size`'s per-axis checks.
        let naxis = header.require_i64("NAXIS")?;
        ensure!(naxis == 2, "BINTABLE requires NAXIS == 2, found {naxis}");

        let row_len = usize::try_from(header.require_i64("NAXIS1")?)
            .map_err(|_| anyhow::anyhow!("negative NAXIS1"))?;
        let nrows = usize::try_from(header.require_i64("NAXIS2")?)
            .map_err(|_| anyhow::anyhow!("negative NAXIS2"))?;
        let nfields = usize::try_from(header.require_i64("TFIELDS")?)
            .map_err(|_| anyhow::anyhow!("negative TFIELDS"))?;

        let expected_len = row_len
            .checked_mul(nrows)
            .ok_or_else(|| anyhow::anyhow!("BINTABLE size overflow"))?;
        ensure!(
            data.len() == expected_len,
            "BINTABLE data is {} bytes, expected {} rows x {} bytes",
            data.len(),
            nrows,
            row_len
        );
        // Zero-width rows with a positive count pass the size check on empty
        // data, leaving `nrows` free to explode a later allocation.
        ensure!(
            row_len != 0 || nrows == 0,
            "BINTABLE has zero-width rows but {nrows} of them"
        );
        // Cheap, reject-biased pre-`with_capacity` bound: real columns are >=1
        // byte, so TFIELDS <= row_len; the offset loop below enforces the exact
        // sum(nbytes) <= row_len (zero-width TFORMs only make this stricter).
        ensure!(
            nfields <= usize::max(row_len, 1),
            "TFIELDS {nfields} exceeds the row length {row_len}"
        );

        let mut columns = Vec::with_capacity(nfields);
        let mut cursor = 0usize;
        for i in 1..=nfields {
            let name = header
                .get_str(&format!("TTYPE{i}"))
                .unwrap_or("<unnamed>")
                .to_string();
            let tform = header
                .get_str(&format!("TFORM{i}"))
                .ok_or_else(|| anyhow::anyhow!("TFORM{i} is missing"))?
                .to_string();
            let form = Form::parse(&tform).with_context(|| {
                format!(
                    "column {i} ({name:?}) of table {:?}",
                    header.get_str("TTYPE1")
                )
            })?;
            // `f64_at` gives raw storage values: quantized/nullable would be silently wrong.
            if matches!(form, Form::Num { .. }) {
                for key in ["TSCAL", "TZERO", "TNULL"] {
                    ensure!(
                        header.get(&format!("{key}{i}")).is_none(),
                        "column {i} ({name:?}) sets {key}{i}: quantized and nullable \
                         columns are not supported"
                    );
                }
            }
            // Columns start at a multiple of their data-unit size capped at 4
            // bytes (the standard's BINTABLE cap), so `D` and `K` align on 4.
            let align = match form {
                Form::Blob(_) => 1,
                Form::Num { kind, .. } => usize::min(kind.size(), 4),
            };
            cursor = align_up(cursor, align);
            let nbytes = form.nbytes()?;
            let end = cursor
                .checked_add(nbytes)
                .ok_or_else(|| anyhow::anyhow!("column {i} ({name:?}) offset overflow"))?;
            ensure!(
                end <= row_len,
                "column {i} ({name:?}) runs past the row length {row_len}"
            );
            columns.push(Column {
                name,
                form,
                offset: cursor,
            });
            cursor = end;
        }
        Ok(Self {
            row_len,
            nrows,
            columns,
            data,
        })
    }

    /// The whole data area (the raw array for a single-blob-column table).
    /// Production readers use the lazy `Fits::raw_view`/`blob_view`; this is for tests.
    #[cfg(test)]
    pub fn raw(&self) -> &'a [u8] {
        self.data
    }

    /// Reads the numeric single-value columns `names` into one vector per column.
    ///
    /// Any single-element numeric TFORM is accepted (`1E` floats or integers
    /// read like `1D` doubles); a multi-element column is rejected, not silently
    /// truncated to its first element.
    pub fn f64_columns(&self, names: &[&str]) -> Result<Vec<Vec<f64>>> {
        let mut out = Vec::with_capacity(names.len());
        for name in names {
            let col = self.column(name)?;
            ensure!(
                matches!(col.form, Form::Num { count: 1, .. }),
                "column {name:?} is not a single numeric column"
            );
            let mut v = Vec::with_capacity(self.nrows);
            for row in 0..self.nrows {
                v.push(col.f64_at(self.row(row), 0)?);
            }
            out.push(v);
        }
        Ok(out)
    }

    pub fn row(&self, i: usize) -> &'a [u8] {
        debug_assert!(i < self.nrows, "row index {i} out of {}", self.nrows);
        &self.data[i * self.row_len..(i + 1) * self.row_len]
    }

    pub fn column(&self, name: &str) -> Result<&Column> {
        self.columns
            .iter()
            .find(|c| c.name == name)
            .ok_or_else(|| anyhow::anyhow!("table has no column {name:?}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Header {
        let mut buf = Vec::new();
        for line in text.lines() {
            let mut card = line.as_bytes().to_vec();
            card.resize(CARD_SIZE, b' ');
            buf.extend_from_slice(&card);
        }
        let mut end = b"END".to_vec();
        end.resize(CARD_SIZE, b' ');
        buf.extend_from_slice(&end);
        let (h, _) = parse_header(&buf, 0).unwrap();
        h
    }

    #[test]
    fn value_types() {
        let h = parse(
            "SIMPLE  =                    T / Standard\nBITPIX  =                    8\n\
             NAXIS   =                    0\nENDIAN  = '04:03:02:01' / Endianness\n\
             AN_FILE = 'QUAD    ' / quads\nSCALE_U =      0.0494509954732 / upper\n\
             HEALPIX =                   -1 / healpix\nCOMMENT just a comment\n\
             TUNIT1  =                      / empty",
        );
        assert_eq!(h.get_bool("SIMPLE"), Some(true));
        assert_eq!(h.get_i64("BITPIX"), Some(8));
        assert_eq!(h.get_str("ENDIAN"), Some("04:03:02:01"));
        assert_eq!(h.get_str("AN_FILE"), Some("QUAD"));
        assert!((h.get_f64("SCALE_U").unwrap() - 0.0494509954732).abs() < 1e-15);
        assert_eq!(h.get_i64("HEALPIX"), Some(-1));
        assert_eq!(h.get("COMMENT"), None);
        assert_eq!(h.get("TUNIT1"), None);
    }

    #[test]
    fn endian_key() {
        assert_eq!(Endian::from_key("04:03:02:01").unwrap(), Endian::Little);
        assert_eq!(Endian::from_key("01:02:03:04").unwrap(), Endian::Big);
        assert!(Endian::from_key("nonsense").is_err());
    }

    #[test]
    fn tform_parsing() {
        assert_eq!(Form::parse("12A").unwrap(), Form::Blob(12));
        assert_eq!(Form::parse("0A").unwrap(), Form::Blob(0));
        assert_eq!(
            Form::parse("1D").unwrap(),
            Form::Num {
                count: 1,
                kind: NumKind::F64
            }
        );
        assert_eq!(
            Form::parse("1E").unwrap(),
            Form::Num {
                count: 1,
                kind: NumKind::F32
            }
        );
        assert_eq!(
            Form::parse("3J").unwrap(),
            Form::Num {
                count: 3,
                kind: NumKind::I32
            }
        );
        assert!(Form::parse("2X").is_err());
    }

    #[test]
    fn readers() {
        assert_eq!(Endian::Little.read_u32(&[1, 0, 0, 0]), 1);
        assert_eq!(Endian::Big.read_u32(&[0, 0, 0, 1]), 1);
        assert_eq!(Endian::Little.read_u16(&[1, 0]), 1);
        assert_eq!(Endian::Big.read_f64(&1.5f64.to_be_bytes()), 1.5);
    }

    /// Builds a two-HDU FITS image in memory: a minimal primary header plus one
    /// BINTABLE whose data area is `data`.
    fn make_fits(table_cards: &[&str], data: &[u8]) -> Vec<u8> {
        let mut buf = Vec::new();
        let mut push_header = |cards: Vec<&str>| {
            let mut blocks: Vec<u8> = Vec::new();
            for c in cards {
                let mut card = c.as_bytes().to_vec();
                card.resize(CARD_SIZE, b' ');
                blocks.extend_from_slice(&card);
            }
            let mut end = b"END".to_vec();
            end.resize(CARD_SIZE, b' ');
            blocks.extend_from_slice(&end);
            blocks.resize(blocks.len().div_ceil(BLOCK_SIZE) * BLOCK_SIZE, b' ');
            buf.extend_from_slice(&blocks);
        };
        push_header(vec![
            "SIMPLE  =                    T",
            "BITPIX  =                    8",
            "NAXIS   =                    0",
            // ENDIAN is taken from the primary HDU (`from_storage`), so it goes here.
            "ENDIAN  = '04:03:02:01'",
        ]);
        let mut cards = vec![
            "XTENSION= 'BINTABLE'           ",
            "BITPIX  =                    8",
            "NAXIS   =                    2",
        ];
        cards.extend_from_slice(table_cards);
        push_header(cards);
        let mut data = data.to_vec();
        data.resize(data.len().div_ceil(BLOCK_SIZE) * BLOCK_SIZE, 0);
        buf.extend_from_slice(&data);
        buf
    }

    /// Builds a single primary-HDU FITS from `cards` (no data area).
    fn primary_only(cards: &[&str]) -> Vec<u8> {
        let mut blocks: Vec<u8> = Vec::new();
        for c in cards {
            let mut card = c.as_bytes().to_vec();
            card.resize(CARD_SIZE, b' ');
            blocks.extend_from_slice(&card);
        }
        let mut end = b"END".to_vec();
        end.resize(CARD_SIZE, b' ');
        blocks.extend_from_slice(&end);
        blocks.resize(blocks.len().div_ceil(BLOCK_SIZE) * BLOCK_SIZE, b' ');
        blocks
    }

    /// Parsing the file or opening its "BLOB" BINTABLE must give an `Err`, never panic or hang.
    fn assert_rejected(buf: Vec<u8>) {
        match Fits::from_bytes(buf) {
            Err(_) => {}
            Ok(fits) => {
                assert!(
                    fits.table_by_ttype("BLOB").is_err(),
                    "crafted BINTABLE was accepted"
                );
            }
        }
    }

    /// Offsets follow the standard's 4-byte BINTABLE alignment cap, and typed
    /// values are big-endian regardless of `ENDIAN`.
    #[test]
    fn table_columns_and_offsets() {
        let mut row = Vec::new();
        row.extend_from_slice(&(-3i16).to_be_bytes());
        row.extend_from_slice(&7i16.to_be_bytes());
        row.extend_from_slice(&2.5f64.to_be_bytes());
        row.extend_from_slice(&(1.5f32).to_be_bytes());
        let buf = make_fits(
            &[
                "NAXIS1  =                   16",
                "NAXIS2  =                    1",
                "PCOUNT  =                    0",
                "GCOUNT  =                    1",
                "TFIELDS =                    3",
                "TFORM1  = '2I     '",
                "TTYPE1  = 'SHORTS  '",
                "TFORM2  = '1D     '",
                "TTYPE2  = 'DBL     '",
                "TFORM3  = '1E     '",
                "TTYPE3  = 'FLT     '",
            ],
            &row,
        );
        let fits = Fits::from_bytes(buf).unwrap();
        // The blob endianness comes from ENDIAN; typed columns do not care.
        assert_eq!(fits.endian(), Endian::Little);
        let table = fits.table_by_ttype("SHORTS").unwrap();
        let offsets: Vec<usize> = table.columns.iter().map(|c| c.offset).collect();
        assert_eq!(offsets, vec![0, 4, 12]);
        let r = table.row(0);
        assert_eq!(table.column("DBL").unwrap().f64_at(r, 0).unwrap(), 2.5);
        assert_eq!(table.column("FLT").unwrap().f64_at(r, 0).unwrap(), 1.5);
        assert_eq!(table.column("SHORTS").unwrap().f64_at(r, 0).unwrap(), -3.0);
        assert_eq!(table.column("SHORTS").unwrap().f64_at(r, 1).unwrap(), 7.0);
        assert_eq!(table.raw(), &row[..]);
    }

    /// A single-element numeric column reads whatever its storage type is: `1E`
    /// floats and the integer forms, not only `1D` doubles.
    #[test]
    fn f64_columns_accepts_float_and_int_forms() {
        let mut data = Vec::new();
        for (x, y) in [(10.5f32, 20.25f32), (1.0, 2.0), (4656.0, 3522.0)] {
            data.extend_from_slice(&x.to_be_bytes());
            data.extend_from_slice(&y.to_be_bytes());
        }
        let buf = make_fits(
            &[
                "NAXIS1  =                    8",
                "NAXIS2  =                    3",
                "PCOUNT  =                    0",
                "GCOUNT  =                    1",
                "TFIELDS =                    2",
                "TFORM1  = '1E     '",
                "TTYPE1  = 'X       '",
                "TFORM2  = '1E     '",
                "TTYPE2  = 'Y       '",
            ],
            &data,
        );
        let fits = Fits::from_bytes(buf).unwrap();
        let table = fits.table_by_ttype("X").unwrap();
        assert_eq!(
            table.f64_columns(&["X", "Y"]).unwrap(),
            vec![vec![10.5, 1.0, 4656.0], vec![20.25, 2.0, 3522.0],]
        );

        // Integer forms too: the usual quantization vehicle, hence the TSCAL/TZERO/TNULL guard.
        let mut data = Vec::new();
        for (x, y) in [(3i32, 4i32), (4656, 3522)] {
            data.extend_from_slice(&x.to_be_bytes());
            data.extend_from_slice(&y.to_be_bytes());
        }
        let buf = make_fits(
            &[
                "NAXIS1  =                    8",
                "NAXIS2  =                    2",
                "PCOUNT  =                    0",
                "GCOUNT  =                    1",
                "TFIELDS =                    2",
                "TFORM1  = '1J     '",
                "TTYPE1  = 'X       '",
                "TFORM2  = '1J     '",
                "TTYPE2  = 'Y       '",
            ],
            &data,
        );
        let fits = Fits::from_bytes(buf).unwrap();
        let table = fits.table_by_ttype("X").unwrap();
        assert_eq!(
            table.f64_columns(&["X", "Y"]).unwrap(),
            vec![vec![3.0, 4656.0], vec![4.0, 3522.0]]
        );
    }

    /// A multi-element numeric column is rejected, not truncated to its first element.
    #[test]
    fn f64_columns_rejects_multi_element() {
        let data = [1.5f64.to_be_bytes(), 2.5f64.to_be_bytes()].concat();
        let buf = make_fits(
            &[
                "NAXIS1  =                   16",
                "NAXIS2  =                    1",
                "PCOUNT  =                    0",
                "GCOUNT  =                    1",
                "TFIELDS =                    1",
                "TFORM1  = '2D     '",
                "TTYPE1  = 'XY      '",
            ],
            &data,
        );
        let fits = Fits::from_bytes(buf).unwrap();
        let table = fits.table_by_ttype("XY").unwrap();
        assert!(table.f64_columns(&["XY"]).is_err());
    }

    /// Quantized (`TZERO + TSCAL*n`) and nullable (`TNULL`) columns are not
    /// implemented: rejected at load time, not read as storage-unit values.
    #[test]
    fn rejects_quantized_column() {
        for card in [
            "TSCAL1  =                  2.0",
            "TZERO1  =                  1.0",
            "TNULL1  =                   -1",
        ] {
            let cards = vec![
                "NAXIS1  =                    4",
                "NAXIS2  =                    1",
                "PCOUNT  =                    0",
                "GCOUNT  =                    1",
                "TFIELDS =                    1",
                "TFORM1  = '1J     '",
                "TTYPE1  = 'X       '",
                card,
            ];
            let buf = make_fits(&cards, &3i32.to_be_bytes());
            let fits = Fits::from_bytes(buf).unwrap();
            assert!(fits.table_by_ttype("X").is_err(), "card {card:?} accepted");
        }
    }

    #[test]
    fn rejects_truncated_data() {
        let buf = make_fits(
            &[
                "NAXIS1  =                    8",
                "NAXIS2  =                 1000",
                "PCOUNT  =                    0",
                "GCOUNT  =                    1",
                "TFIELDS =                    1",
                "TFORM1  = '8A     '",
                "TTYPE1  = 'BLOB    '",
            ],
            &[0u8; 8],
        );
        assert!(Fits::from_bytes(buf).is_err());
    }

    /// A negative `TFIELDS` must not reach `Vec::with_capacity` (capacity overflow panic).
    #[test]
    fn rejects_negative_tfields() {
        let buf = make_fits(
            &[
                "NAXIS1  =                    4",
                "NAXIS2  =                    1",
                "PCOUNT  =                    0",
                "GCOUNT  =                    1",
                "TFIELDS =                   -1",
                "TFORM1  = '4A     '",
                "TTYPE1  = 'BLOB    '",
            ],
            &[0u8; 4],
        );
        assert_rejected(buf);
    }

    /// A BINTABLE must declare `NAXIS == 2`: 0 or 1 lets a negative `NAXIS2`
    /// slip past `data_size`'s per-axis checks into an unchecked cast.
    #[test]
    fn rejects_bad_naxis() {
        for naxis in ["0", "1"] {
            let buf = make_fits(
                &[
                    &format!("NAXIS   = {:>20}", naxis),
                    "NAXIS1  =                    8",
                    "NAXIS2  =                   -1",
                    "PCOUNT  =                    0",
                    "GCOUNT  =                    1",
                    "TFIELDS =                    1",
                    "TFORM1  = '8A     '",
                    "TTYPE1  = 'BLOB    '",
                ],
                &[0u8; 8],
            );
            assert_rejected(buf);
        }
    }

    /// A negative axis is rejected; last card wins, so the override beats the earlier pair.
    #[test]
    fn rejects_negative_axis() {
        for card in [
            "NAXIS1  =                   -1",
            "NAXIS2  =                   -1",
        ] {
            let cards = vec![
                "NAXIS1  =                    4",
                "NAXIS2  =                    1",
                "PCOUNT  =                    0",
                "GCOUNT  =                    1",
                "TFIELDS =                    1",
                "TFORM1  = '4A     '",
                "TTYPE1  = 'BLOB    '",
                card,
            ];
            let buf = make_fits(&cards, &[0u8; 4]);
            assert!(Fits::from_bytes(buf).is_err(), "{card:?} accepted");
        }
    }

    /// A `TFORM` count that overflows `count * size()` must give an `Err`, not
    /// a wrapping `nbytes()` that later slices out of bounds.
    #[test]
    fn rejects_overflowing_tform() {
        let buf = make_fits(
            &[
                "NAXIS1  =                    8",
                "NAXIS2  =                    1",
                "PCOUNT  =                    0",
                "GCOUNT  =                    1",
                "TFIELDS =                    1",
                "TFORM1  = '4611686018427387904D'",
                "TTYPE1  = 'BLOB    '",
            ],
            &[0u8; 8],
        );
        assert_rejected(buf);
    }

    /// Only standard BITPIX values are accepted: 0 and 24 would pass a naive
    /// "multiple of 8" check and mis-size the data area.
    #[test]
    fn rejects_bad_bitpix() {
        for bitpix in ["0", "24"] {
            let buf = primary_only(&[
                "SIMPLE  =                    T",
                &format!("BITPIX  = {:>20}", bitpix),
                "NAXIS   =                    1",
                "NAXIS1  =                    8",
            ]);
            assert!(Fits::from_bytes(buf).is_err(), "BITPIX {bitpix} accepted");
        }
    }

    /// A hostile `NAXIS` fails fast instead of spinning `data_size`'s axis loop (capped at 999).
    #[test]
    fn rejects_huge_naxis() {
        for naxis in ["200000000", "9223372036854775807"] {
            let buf = primary_only(&[
                "SIMPLE  =                    T",
                "BITPIX  =                    8",
                &format!("NAXIS   = {:>20}", naxis),
            ]);
            assert!(Fits::from_bytes(buf).is_err(), "NAXIS {naxis} accepted");
        }
    }

    /// A doubled `''` in a quoted value is an escaped quote, not a terminator.
    #[test]
    fn doubled_quote_escape() {
        let h = parse("KEY     = 'it''s here' / comment");
        assert_eq!(h.get_str("KEY"), Some("it's here"));
    }

    /// An unterminated string is a clean `Err`, not a silent key drop
    /// (which would surface later as a misleading "missing key").
    #[test]
    fn unterminated_string_is_error() {
        let mut card = b"KEY     = 'oops".to_vec();
        card.resize(CARD_SIZE, b' ');
        let mut end = b"END".to_vec();
        end.resize(CARD_SIZE, b' ');
        let mut buf = card;
        buf.extend_from_slice(&end);
        assert!(parse_header(&buf, 0).is_err());
    }

    /// `blob_view` exposes only the first blob column's bytes; the second
    /// column's must not leak into the element sequence. The row here is col1
    /// (4A) = u32 1, col2 (4A) = u32 2, ENDIAN little.
    #[test]
    fn blob_view_reads_only_first_column() {
        let row = [1u8, 0, 0, 0, 2, 0, 0, 0];
        let buf = make_fits(
            &[
                "NAXIS1  =                    8",
                "NAXIS2  =                    1",
                "PCOUNT  =                    0",
                "GCOUNT  =                    1",
                "TFIELDS =                    2",
                "TFORM1  = '4A     '",
                "TTYPE1  = 'BLOB    '",
                "TFORM2  = '4A     '",
                "TTYPE2  = 'EXTRA   '",
            ],
            &row,
        );
        let fits = Fits::from_bytes(buf).unwrap();
        let view = fits.blob_view("BLOB").unwrap();
        assert_eq!(view.payload_bytes(), 4);
        assert_eq!(view.uint_at(fits.endian(), 4, 0), 1);
    }

    /// One padded header block built from `cards`, with `END` appended.
    fn hdu_block(cards: &[&str]) -> Vec<u8> {
        let mut buf = Vec::new();
        for line in cards {
            let mut card = line.as_bytes().to_vec();
            card.resize(CARD_SIZE, b' ');
            buf.extend_from_slice(&card);
        }
        let mut end = b"END".to_vec();
        end.resize(CARD_SIZE, b' ');
        buf.extend_from_slice(&end);
        let pad = buf.len().div_ceil(BLOCK_SIZE) * BLOCK_SIZE;
        buf.resize(pad, b' ');
        buf
    }

    /// A primary plus a one-row BINTABLE: the header-only reader must see the
    /// same headers as the whole-file reader, with the data area seeked over.
    #[test]
    fn header_only_reader_matches_the_whole_file_reader() {
        let mut bytes = hdu_block(&[
            "SIMPLE  =                    T",
            "BITPIX  =                    8",
            "NAXIS   =                    0",
            "ENDIAN  = '04:03:02:01'",
            "AN_FILE = 'QUAD'",
        ]);
        bytes.extend_from_slice(&hdu_block(&[
            "XTENSION= 'BINTABLE'",
            "BITPIX  =                    8",
            "NAXIS   =                    2",
            "NAXIS1  =                    4",
            "NAXIS2  =                    1",
            "TFIELDS =                    1",
            "TFORM1  = '4A'",
            "TTYPE1  = 'quads'",
        ]));
        // The table's data area, padded to a block, is the rest of the file.
        bytes.resize(3 * BLOCK_SIZE, 0);

        let full = Fits::from_bytes(bytes.clone()).expect("crafted file should parse");
        let headers =
            FitsHeaders::from_bytes(bytes).expect("the headers must parse without the data");
        assert_eq!(headers.endian(), full.endian());
        assert_eq!(
            headers.primary().get_str("AN_FILE"),
            full.primary().get_str("AN_FILE")
        );
        assert_eq!(headers.table_names(), vec!["quads"]);
        assert_eq!(headers.table_names(), full.table_names());
        assert!(headers.header_by_ttype("quads").is_ok());
        assert!(headers.header_by_ttype("stars").is_err());
    }

    /// Scanning a directory must not read data: a header declaring a gigabyte is
    /// seeked over, while the whole-file reader insists those bytes exist.
    #[test]
    fn header_only_reader_skips_a_data_area_it_cannot_see() {
        let bytes = hdu_block(&[
            "SIMPLE  =                    T",
            "BITPIX  =                    8",
            "NAXIS   =                    1",
            "NAXIS1  =           1073741824",
        ]);
        let headers = FitsHeaders::from_bytes(bytes.clone())
            .expect("a one-block file must be readable without its data");
        assert_eq!(headers.primary().get_i64("NAXIS1"), Some(1_073_741_824));
        assert_eq!(headers.table_names(), Vec::<&str>::new());

        let err = Fits::from_bytes(bytes).unwrap_err();
        assert!(
            format!("{err:#}").contains("runs past the end of file"),
            "{err:#}"
        );
    }

    /// A header block that never reaches `END` is a truncated file, not an empty header list.
    #[test]
    fn header_without_end_is_truncated() {
        let mut bytes = Vec::new();
        for i in 0..BLOCK_SIZE / CARD_SIZE {
            let card = if i == 0 {
                "SIMPLE  =                    T".to_string()
            } else {
                format!("TEST{i}  =                    1")
            };
            let mut card = card.into_bytes();
            card.resize(CARD_SIZE, b' ');
            bytes.extend_from_slice(&card);
        }
        let err = FitsHeaders::from_bytes(bytes).unwrap_err();
        assert!(
            format!("{err:#}").contains("truncated FITS header"),
            "{err:#}"
        );
    }
}
