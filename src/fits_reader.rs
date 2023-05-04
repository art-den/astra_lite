use std::{io::*, collections::HashMap, str::FromStr, mem::size_of};

use itertools::{Itertools, izip};

pub trait SeekNRead: Seek + Read {}
impl<T: Seek + Read> SeekNRead for T {}

struct HduValue {
    value: String,
    comment: String,
}

pub struct Hdu {
    values:    HashMap<String, HduValue>,
    bitpix:    i8,
    dims:      Vec<usize>,
    data_pos:  usize,
    data_len:  usize,
    bytes_len: usize,
}

impl Hdu {
    fn get_value_impl<T: FromStr>(values: &HashMap<String, HduValue>, key: &str) -> Option<T> {
        values
            .get(key)
            .as_deref()?
            .value.parse()
            .ok()
    }

    pub fn get_i64(&self, key: &str) -> Option<i64> {
        Self::get_value_impl(&self.values, key)
    }

    pub fn get_f64(&self, key: &str) -> Option<f64> {
        Self::get_value_impl(&self.values, key)
    }

    pub fn get_str<'b>(&'b self, key: &str) -> Option<&'b str> {
        let mut result = self.values.get(key)?.value.as_str();
        if result.starts_with("'") && result.ends_with("'") {
            result = &result[1..result.len()-1];
        }
        Some(result.trim())
    }

    pub fn dims(&self) -> &Vec<usize> {
        &self.dims
    }

    fn get_data_integer<T: VecFormBytes + Copy + Default>(
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
            T::vec_from_bytes(buf, bzero, &mut tmp_buf[..len_to_read]);
            len -= len_to_read;
            result.extend_from_slice(&tmp_buf[..len_to_read]);
        }

        return Ok(result)
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
        self.get_data_integer::<u16>(stream, bzero)
    }

}

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
        let mut values = HashMap::new();
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
                    values.insert(
                        key.trim().to_string(),
                        HduValue {
                            value: value.trim().to_string(),
                            comment: comment.trim().to_string()
                        }
                    );
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

trait VecFormBytes where Self: Sized {
    fn vec_from_bytes(bytes: &[u8], add: Self, result: &mut [Self]);
}

impl VecFormBytes for u16 {
    fn vec_from_bytes(bytes: &[u8], add: Self, result: &mut [Self]) {
        for ((b1, b2), dst) in izip!(bytes.iter().tuples(), result) {
            let value = u16::from_be_bytes([*b1, *b2]);
            *dst = value.wrapping_add(add);
        }
    }
}
