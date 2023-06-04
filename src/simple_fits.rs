use std::{io::*, str::FromStr, mem::size_of};

use itertools::{Itertools, izip};

pub trait SeekNRead: Seek + Read {}
impl<T: Seek + Read> SeekNRead for T {}

pub trait SeekNWrite: Seek + Write {}
impl<T: Seek + Write> SeekNWrite for T {}

#[derive(Clone)]
struct HduValue {
    name: String,
    value: String,
    comment: Option<String>,
}

#[derive(Clone)]
pub struct Hdu {
    values:    Vec<HduValue>,
    bitpix:    i8,
    dims:      Vec<usize>,
    data_pos:  usize,
    data_len:  usize,
    bytes_len: usize,
}

impl Hdu {
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

    fn get_value_impl<T: FromStr>(values: &Vec<HduValue>, key: &str) -> Option<T> {
        values.iter()
            .find(|item| item.name.eq_ignore_ascii_case(key))
            .as_deref()?
            .value.parse()
            .ok()
    }

    pub fn set_value(&mut self, key: &str, value: &str) {
        Self::set_value_impl(&mut self.values, key, value, false);
    }

    fn set_value_impl(values: &mut Vec<HduValue>, key: &str, value: &str, at_front: bool) {
        if let Some(item) = values.iter_mut().find(|item| item.name.eq_ignore_ascii_case(key)) {
            item.value = value.to_string();
        } else {
            let value = HduValue {
                name:   key.to_string(),
                value:   value.to_string(),
                comment: None,
            };
            if !at_front {
                values.push(value);
            } else {
                values.insert(0, value);
            }
        }
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

    fn read_integer_from_stream<T: FormBytes + Copy + Default>(
        &self,
        stream: &mut dyn SeekNRead,
        bzero: T
    ) -> Result<Vec<T>> {
        const BUF_DATA_LEN: usize = 512;
        let mut stream_buf = Vec::<u8>::new();
        stream_buf.resize(BUF_DATA_LEN * size_of::<T>(), 0);
        let mut tmp_buf = Vec::<T>::new();
        tmp_buf.resize(BUF_DATA_LEN, T::default());
        let mut result = Vec::<T>::with_capacity(self.data_len);
        let mut len = self.data_len;
        stream.seek(SeekFrom::Start(self.data_pos as u64))?;
        while len != 0 {
            let len_to_read = usize::min(len, BUF_DATA_LEN / size_of::<T>());
            let buf = &mut stream_buf[.. size_of::<T>() * len_to_read];
            stream.read_exact(buf)?;
            T::from_bytes(buf, bzero, &mut tmp_buf[..len_to_read]);
            len -= len_to_read;
            result.extend_from_slice(&tmp_buf[..len_to_read]);
        }

        return Ok(result)
    }

    fn write_integer_to_stream<T: ToBytes + Copy>(
        &self,
        stream: &mut dyn SeekNWrite,
        data:   &[T],
        bzero:  T
    ) -> Result<()> {
        const BUF_DATA_LEN: usize = 512;
        let mut stream_buf = Vec::<u8>::new();
        stream_buf.resize(BUF_DATA_LEN * size_of::<T>(), 0);
        let mut len = data.len();
        let mut data = data;
        while len != 0 {
            let len_to_write = usize::min(len, BUF_DATA_LEN / size_of::<T>());
            let buf = &mut stream_buf[.. size_of::<T>() * len_to_write];
            T::to_bytes(buf, bzero, &data[..len_to_write]);
            stream.write_all(buf)?;
            len -= len_to_write;
            data = &data[len_to_write..];
        }
        Ok(())
    }

    pub fn data_u16(&self, stream: &mut dyn SeekNRead) -> Result<Vec<u16>> {
        let bzero = self.get_i64("BZERO").unwrap_or(0) as u16;
        let bitpix = self.get_i64("BITPIX").unwrap_or(0);
        if bitpix != 16 {
            return Err(Error::new(
                ErrorKind::Unsupported,
                format!("BITPIX = {} is not supported", bitpix)
            ))
        }
        self.read_integer_from_stream::<u16>(stream, bzero)
    }
}

///////////////////////////////////////////////////////////////////////////////

pub struct FitsReader {
    pub hdus: Vec<Hdu>,
}

impl FitsReader {
    pub fn new(stream: &mut dyn SeekNRead) -> Result<FitsReader> {
        stream.seek(SeekFrom::Start(0))?;

        let hdus = Self::read_hdus(stream)?;
        Ok(Self { hdus })
    }

    fn read_hdus(stream: &mut dyn SeekNRead) -> Result<Vec<Hdu>> {
        let mut result = Vec::new();

        loop {
            let hdu_res = Self::read_hdu(stream);
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

    fn read_hdu(stream: &mut dyn SeekNRead) -> Result<Hdu> {
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
                    values.push(HduValue {
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

        let ndim: usize = Hdu::get_value_impl(&values, "NAXIS").unwrap_or(0);
        let gcount: usize = Hdu::get_value_impl(&values, "GCOUNT").unwrap_or(1);
        let pcount: usize = Hdu::get_value_impl(&values, "PCOUNT").unwrap_or(0);
        let bitpix: i8 = Hdu::get_value_impl(&values, "BITPIX").unwrap_or(8);

        let mut dims = Vec::new();
        let mut data_len = 1_usize;
        for idx in 1 ..= ndim {
            let key = format!("NAXIS{}", idx);
            let dim: usize = Hdu::get_value_impl(&values, &key).unwrap_or(1);
            data_len *= dim;
            dims.push(dim);
        }
        data_len += pcount;
        data_len *= gcount;

        let byte_per_value = (bitpix.abs() / 8) as usize;
        let bytes_len = data_len * byte_per_value;
        let data_pos = stream.stream_position().unwrap_or(0) as usize;

        Ok(Hdu{values, bitpix, dims, data_pos, data_len, bytes_len})
    }
}

///////////////////////////////////////////////////////////////////////////////

trait FormBytes where Self: Sized {
    fn from_bytes(bytes: &[u8], add: Self, result: &mut [Self]);
}

impl FormBytes for u16 {
    fn from_bytes(bytes: &[u8], add: Self, result: &mut [Self]) {
        for ((b1, b2), dst) in izip!(bytes.iter().tuples(), result) {
            let value = u16::from_be_bytes([*b1, *b2]);
            *dst = value.wrapping_add(add);
        }
    }
}

///////////////////////////////////////////////////////////////////////////////

pub struct FitsWriter {}

impl FitsWriter {
    pub fn new() -> Self {
        Self {}
    }

    pub fn write(&self, stream: &mut dyn SeekNWrite, hdu: &Hdu, data: &[u16]) -> Result<()> {
        let mut hdu = hdu.clone();
        Hdu::set_value_impl(&mut hdu.values, "BZERO", "32768", true);
        Hdu::set_value_impl(&mut hdu.values, "BITPIX", "16", true);
        Hdu::set_value_impl(&mut hdu.values, "SIMPLE", "T", true);
        Self::write_hdu(stream, &hdu)?;
        hdu.write_integer_to_stream(stream, data, 32768)?;
        Ok(())
    }

    fn write_hdu(stream: &mut dyn SeekNWrite, hdu: &Hdu) -> Result<()> {
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

///////////////////////////////////////////////////////////////////////////////

trait ToBytes where Self: Sized {
    fn to_bytes(result: &mut [u8], sub: Self, data: &[Self]);
}

impl ToBytes for u16 {
    fn to_bytes(result: &mut [u8], sub: Self, data: &[Self]) {
        for ((b1, b2), v) in izip!(result.iter_mut().tuples(), data) {
            let be_bytes = v.wrapping_sub(sub).to_be_bytes();
            [*b1, *b2] = be_bytes;
        }
    }
}
