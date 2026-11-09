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

    pub fn get_str<'b>(&'b self, key: &str) -> Option<&'b str> {
        let mut result = self.values.iter()
            .find(|item| item.name.eq_ignore_ascii_case(key))?
            .value.as_str();
        if result.starts_with("'") && result.ends_with("'") {
            result = &result[1..result.len()-1];
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
        let bzero = header.get_i64("BZERO").unwrap_or(0) as u16;
        let elem_len = header.bytes_per_value();
        const BUF_DATA_LEN: usize = 512;
        let mut stream_buf = vec![0_u8; BUF_DATA_LEN * elem_len];
        stream.seek(SeekFrom::Start((header.data_pos + offset) as u64))?;
        for chunk in result.chunks_mut(BUF_DATA_LEN) {
            let len_to_read = chunk.len();
            let buf = &mut stream_buf[.. elem_len * len_to_read];
            stream.read_exact(buf)?;
            match header.bitpix {
                8 => {
                    let add = bzero as u8;
                    for (b, dst) in izip!(buf, chunk) {
                        *dst = b.wrapping_add(add) as u16;
                    }
                }
                16 => {
                    for ((b1, b2), dst) in izip!(buf.iter().tuples(), chunk) {
                        let value = ((*b1 as u16) << 8) + *b2 as u16;
                        *dst = value.wrapping_add(bzero);
                    }
                },
                -32 => {
                    for ((b1, b2, b3, b4), dst) in izip!(buf.iter().tuples(), chunk) {
                        let value = f32::from_be_bytes([*b1, *b2, *b3, *b4]);
                        let i32_value = (value * u16::MAX as f32 + 0.5) as i32;
                        *dst = i32_value.max(0).min(u16::MAX as i32) as u16;
                    }
                }
                _ => unreachable!(),
            }
        }
        Ok(())
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
        const BUF_DATA_LEN: usize = 512;
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
        const BUF_DATA_LEN: usize = 512;
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
}
