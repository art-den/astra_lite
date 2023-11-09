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

    pub fn set_value(&mut self, key: &str, value: &str) {
        if let Some(item) = self.values.iter_mut().find(|item| item.name.eq_ignore_ascii_case(key)) {
            item.value = value.to_string();
        } else {
            let value = Value {
                name:   key.to_string(),
                value:   value.to_string(),
                comment: None,
            };
            self.values.push(value);
        }
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

    fn write_data(
        &self,
        stream: &mut dyn SeekNWrite,
        data:   &[u16],
        bzero:  u16
    ) -> Result<()> {
        if !matches!(self.bitpix, 8|16) {
            return Err(Error::new(
                ErrorKind::Unsupported,
                format!("BITPIX = {} is not supported", self.bitpix)
            ));
        }
        let item_len = self.bitpix as usize / 8;
        const BUF_DATA_LEN: usize = 512;
        let mut stream_buf = Vec::<u8>::new();
        stream_buf.resize(BUF_DATA_LEN * item_len, 0);
        for chunk in data.chunks(BUF_DATA_LEN) {
            let len_to_write = chunk.len();
            let buf = &mut stream_buf[.. item_len * len_to_write];
            match self.bitpix {
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

    pub fn read_data(&self, stream: &mut dyn SeekNRead) -> Result<Vec<u16>> {
        if !matches!(self.bitpix, 8|16) {
            return Err(Error::new(
                ErrorKind::Unsupported,
                format!("BITPIX = {} is not supported", self.bitpix)
            ));
        }
        let bzero = self.get_i64("BZERO").unwrap_or(0) as u16;
        let elem_len = self.bitpix as usize / 8;
        const BUF_DATA_LEN: usize = 512;
        let mut stream_buf = Vec::<u8>::new();
        stream_buf.resize(BUF_DATA_LEN * elem_len, 0);
        let mut result = Vec::new();
        result.resize(self.data_len, 0);
        stream.seek(SeekFrom::Start(self.data_pos as u64))?;
        for chunk in result.chunks_mut(BUF_DATA_LEN) {
            let len_to_read = chunk.len();
            let buf = &mut stream_buf[.. elem_len * len_to_read];
            stream.read_exact(buf)?;
            match self.bitpix {
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
                _ => unreachable!(),
            }
        }
        return Ok(result)
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
}

///////////////////////////////////////////////////////////////////////////////

pub struct FitsWriter {}

impl FitsWriter {
    pub fn new() -> Self {
        Self {}
    }

    pub fn write_header_and_data(
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

        full_hdr.set_value("SIMPLE", "T");
        full_hdr.set_value("BITPIX", &full_hdr.bitpix.to_string());
        full_hdr.set_value("NAXIS",  &hdu.dims.len().to_string());
        for (idx, dim) in hdu.dims.iter().enumerate() {
            let name = format!("NAXIS{}", idx+1);
            full_hdr.set_value(&name, &dim.to_string());
        }
        full_hdr.set_value("EXTEND", "T");

        if bzero != 0 {
            full_hdr.set_value("BZERO", &bzero.to_string());
        }

        full_hdr.dims = hdu.dims.clone();
        full_hdr.data_pos = hdu.data_pos;
        full_hdr.data_len = hdu.data_len;
        full_hdr.bytes_len = hdu.bytes_len;
        for value in &hdu.values {
            full_hdr.values.push(value.clone());
        }

        Self::write_header(stream, &full_hdr)?;
        full_hdr.write_data(stream, data, bzero)?;
        Ok(())
    }

    fn write_header(stream: &mut dyn SeekNWrite, hdu: &Header) -> Result<()> {
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
}
