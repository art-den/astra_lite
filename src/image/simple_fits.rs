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

const DEFAULT_BITPIX: u16 = 8;

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

    fn get_value_impl<T: FromStr>(values: &Vec<Value>, key: &str) -> Option<T> {
        values.iter()
            .find(|item| item.name.eq_ignore_ascii_case(key))
            .as_deref()?
            .value.parse()
            .ok()
    }

    fn set_value_impl(&mut self, key: &str, value: String) {
        if let Some(item) = self.values.iter_mut().find(|item| item.name.eq_ignore_ascii_case(key)) {
            item.value = value;
        } else {
            let value = Value {
                name: key.to_string(),
                value: value,
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
        let values_str = if value { "T" } else { "F" };
        self.set_value_impl(key, values_str.to_string());
    }

    pub fn set_str(&mut self, key: &str, value: &str) {
        self.set_value_impl(key, format!("'{:<8}'", value));
    }

    pub fn bitpix(&self) -> i8 {
        self.bitpix
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

    pub fn data_len(&self) -> usize {
        self.data_len
    }

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

            let size = hdu.bytes_len;
            result.push(hdu);
            stream.seek(SeekFrom::Current(size as i64))?;
        }

        Ok(result)
    }

    fn read_header(stream: &mut dyn SeekNRead) -> Result<Header> {
        let mut buf = [0_u8; 2880];
        let mut values = Vec::new();
        let mut last_block = false;
        loop {
            stream.read_exact(&mut buf)?;
            for line in buf.chunks(80) {
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

        let ndim: usize = Header::get_value_impl(&values, "NAXIS").unwrap_or(0);
        let gcount: usize = Header::get_value_impl(&values, "GCOUNT").unwrap_or(1);
        let pcount: usize = Header::get_value_impl(&values, "PCOUNT").unwrap_or(0);
        let bitpix: i8 = Header::get_value_impl(&values, "BITPIX").unwrap_or(DEFAULT_BITPIX as i8);

        let mut dims = Vec::new();
        let mut data_len = 1_usize;
        for idx in 1 ..= ndim {
            let key = format!("NAXIS{}", idx);
            let dim: usize = Header::get_value_impl(&values, &key).unwrap_or(1);
            data_len *= dim;
            dims.push(dim);
        }
        data_len += pcount;
        data_len *= gcount;

        let byte_per_value = (bitpix.abs() / 8) as usize;
        let bytes_len = data_len * byte_per_value;
        let data_pos = stream.stream_position().unwrap_or(0) as usize;

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
        let elem_len = header.bitpix.abs() as usize / 8;
        const BUF_DATA_LEN: usize = 512;
        let mut stream_buf = Vec::<u8>::new();
        stream_buf.resize(BUF_DATA_LEN * elem_len, 0);
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
                        let value = u16::from_be_bytes([*b1, *b2]);
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
        return Ok(())
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

    pub fn write_header_and_data_u16(
        &self,
        stream: &mut dyn SeekNWrite,
        hdu: &Header,
        data: &[u16],
    ) -> Result<()> {
        assert!(!data.is_empty());
        const U16_BZERO: u16 = 32768;
        let mut full_hdr = Header::new();
        let max_value = data.iter().max().copied().unwrap_or(0);
        let bzero = if max_value > 255 {
            full_hdr.bitpix = 16;
            U16_BZERO
        } else {
            full_hdr.bitpix = 8;
            0_u16
        };

        full_hdr.set_bool("SIMPLE", true);
        full_hdr.set_i64("BITPIX", full_hdr.bitpix as i64);
        full_hdr.set_i64("NAXIS",  hdu.dims.len() as i64);
        for (idx, dim) in hdu.dims.iter().enumerate() {
            let name = format!("NAXIS{}", idx+1);
            full_hdr.set_i64(&name, *dim as i64);
        }
        full_hdr.set_bool("EXTEND", true);

        if bzero != 0 {
            full_hdr.set_i64("BZERO", bzero as i64);
        }

        for value in &hdu.values {
            full_hdr.values.push(value.clone());
        }

        self.write_header(stream, &full_hdr)?;
        self.write_data(full_hdr.bitpix, bzero, stream, data)?;
        Ok(())
    }

    pub fn write_header(&self, stream: &mut dyn SeekNWrite, hdu: &Header) -> Result<()> {
        for item in &hdu.values {
            let mut line = format!("{:8}= ", item.name);
            if item.value.starts_with("'") {
                line.push_str(&format!("{:<20}", item.value))
            } else {
                line.push_str(&format!("{:>20}", item.value))
            }
            line.push_str(" / ");
            line.push_str(item.comment.as_ref().unwrap_or(&item.name));
            while line.len() < 80 { line.push(' '); }
            while line.len() > 80 { line.pop(); }
            stream.write_all(line.as_bytes())?;
        }
        write!(stream, "{:80}", "END")?;
        let lines_written = hdu.values.len() + 1;
        let lines_to_complete = 36 - lines_written % 36;
        for _ in 0..lines_to_complete {
            write!(stream, "{:80}", "")?;
        }
        Ok(())
    }

    fn write_data(
        &self,
        bitpix: i8,
        bzero:  u16,
        stream: &mut dyn SeekNWrite,
        data:   &[u16],
    ) -> Result<()> {
        if !matches!(bitpix, 8|16) {
            return Err(Error::new(
                ErrorKind::Unsupported,
                format!("BITPIX = {} is not supported", bitpix)
            ));
        }
        let item_len = bitpix as usize / 8;
        const BUF_DATA_LEN: usize = 512;
        let mut stream_buf = Vec::<u8>::new();
        stream_buf.resize(BUF_DATA_LEN * item_len, 0);
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
        }
        Ok(())
    }


    pub fn write_header_and_bintable_f64(
        &self,
        stream: &mut dyn SeekNWrite,
        hdu: &Header,
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
            full_hdr.set_str(&name_fld, &col.name);
            let type_fld = format!("TFORM{}", idx + 1);
            full_hdr.set_str(&type_fld, &col.type_);
        }
        for (idx, col) in cols.iter().enumerate() {
            let unit_fld = format!("TUNIT{}", idx + 1);
            full_hdr.set_str(&unit_fld, &col.unit);
        }

        for value in &hdu.values {
            full_hdr.values.push(value.clone());
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
        let mut stream_buf = Vec::<u8>::new();
        stream_buf.resize(BUF_DATA_LEN * item_len, 0);
        let mut written = 0_usize;
        for chunk in data.chunks(BUF_DATA_LEN) {
            let len_to_write = chunk.len();
            let buf = &mut stream_buf[.. item_len * len_to_write];
            for ((b1, b2, b3, b4, b5, b6, b7, b8), v) in izip!(buf.iter_mut().tuples(), chunk) {
                [*b1, *b2, *b3, *b4, *b5, *b6, *b7, *b8] = v.to_be_bytes();
            }
            stream.write_all(buf)?;
            written += buf.len();
        }

        written %= 2880;

        if written != 0 {
            let mut zeros = Vec::new();
            zeros.resize(2880 - written, 0u8);
            stream.write_all(&zeros)?;
        }

        Ok(())
    }

}
