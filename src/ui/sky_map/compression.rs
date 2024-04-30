use bitstream_io::*;
use itertools::*;

const COMPR_BUF_SIZE:  usize = 64;
const MAX_LARGE_CNT:   usize = COMPR_BUF_SIZE / 4;

const HEADER_ZEROS:     u32 = 0b000;
const HEADER_NO_COMPR:  u32 = 0b001;
const HEADER_LZ:        u32 = 0b010;
const HEADER_LZ_2:      u32 = 0b011;
const HEADER_LZ_TZ:     u32 = 0b100;
const HEADER_LZ_TZ_2:   u32 = 0b101;

pub struct ValuesCompressor {
    data:       [u32; COMPR_BUF_SIZE],
    data_ptr:   usize,
    prev_value: u32,
}

// Compression for u32, i32 and f32 values
impl ValuesCompressor {
    pub fn new() -> Self {
        Self {
            data:       [0_u32; COMPR_BUF_SIZE],
            data_ptr:   0,
            prev_value: 0,
        }
    }

    pub fn write_u32<T: BitWrite>(&mut self, value: u32, writer: &mut T) -> std::io::Result<()> {
        self.data[self.data_ptr] = value ^ self.prev_value;
        self.prev_value = value;
        self.data_ptr += 1;
        if self.data_ptr == COMPR_BUF_SIZE {
            self.flush(writer)?;
        }
        Ok(())
    }

    pub fn write_f32<T: BitWrite>(&mut self, value: f32, writer: &mut T) -> std::io::Result<()> {
        self.write_u32(value.to_bits(), writer)
    }

    pub fn write_i32<T: BitWrite>(&mut self, value: i32, writer: &mut T) -> std::io::Result<()> {
        let is_neg = value < 0;
        let u32_value = if is_neg {
            ((value as u32 ^ 0xFFFF_FFFF) << 1) | 1
        } else {
            (value as u32) << 1
        };
        self.write_u32(u32_value, writer)
    }

    pub fn flush<T: BitWrite>(&mut self, writer: &mut T) -> std::io::Result<()> {
        if self.data_ptr == 0 {
            return Ok(())
        }
        self.data[self.data_ptr..].fill(0);
        self.data_ptr = 0;
        self.compress(writer)?;
        Ok(())
    }

    fn compress<T: BitWrite>(&mut self, writer: &mut T) -> std::io::Result<()> {
        let mut lz_freq = [0_u8; 32+1];
        let mut lz_values = [0_u32; COMPR_BUF_SIZE];
        let mut lz_large = u32::MAX;
        let mut min_tz = u32::MAX;
        for (d, lz) in izip!(self.data, &mut lz_values) {
            let tz = d.trailing_zeros();
            if tz < min_tz { min_tz = tz; }
            *lz = d.leading_zeros();
            if *lz < lz_large { lz_large = *lz; }
            lz_freq[*lz as usize] += 1;
        }
        if min_tz == 32 {
            writer.write(3, HEADER_ZEROS)?;
            return Ok(());
        }
        let mut cnt_sum = 0_usize;
        let mut lz_norm = 0_u32;
        for (lz, cnt) in lz_freq.iter().enumerate() {
            cnt_sum += *cnt as usize;
            if cnt_sum >= MAX_LARGE_CNT {
                lz_norm = lz as u32;
                break;
            }
        }
        lz_norm = lz_norm
            .max(lz_large)
            .min(31 - min_tz);

        if lz_norm + min_tz < 2 {
            writer.write(3, HEADER_NO_COMPR)?;
            for v in self.data {
                writer.write(32, v)?;
            }
            return Ok(());
        }
        let use_large_lz_bits = (lz_norm as i32 - lz_large as i32) >= 2;
        if min_tz == 0 {
            if !use_large_lz_bits {
                writer.write(3, HEADER_LZ)?;
                writer.write(5, lz_large)?;
            } else {
                writer.write(3, HEADER_LZ_2)?;
                writer.write(5, lz_large)?;
                writer.write(5, lz_norm)?;
            }
        } else {
            if !use_large_lz_bits {
                writer.write(3, HEADER_LZ_TZ)?;
                writer.write(5, lz_large)?;
                writer.write(5, min_tz)?;
            } else {
                writer.write(3, HEADER_LZ_TZ_2)?;
                writer.write(5, lz_large)?;
                writer.write(5, lz_norm)?;
                writer.write(5, min_tz)?;
            }
        }
        let large_len = (32 - lz_large - min_tz).max(1);
        if use_large_lz_bits {
            let mut large_bits = 0_u64;
            for lz in &lz_values {
                large_bits <<= 1;
                if *lz < lz_norm {
                    large_bits |= 1;
                }
            }
            writer.write(COMPR_BUF_SIZE as u32, large_bits)?;
            let norm_len = (32 - lz_norm - min_tz).max(1);
            for (v, lz) in izip!(&self.data, &lz_values) {
                let len_to_write = if *lz >= lz_norm { norm_len } else { large_len };
                writer.write(len_to_write, *v >> min_tz)?;
            }
        } else {
            for v in &self.data {
                writer.write(large_len, *v >> min_tz)?;
            }
        }
        Ok(())
    }
}

// Decompression for u32, i32 and f32 values
pub struct ValuesDecompressor {
    values:     [u32; COMPR_BUF_SIZE],
    values_ptr: usize,
    prev_value: u32,
}

impl ValuesDecompressor {
    pub fn new() -> Self {
        Self {
            values:     [0_u32; COMPR_BUF_SIZE],
            values_ptr: COMPR_BUF_SIZE,
            prev_value: 0,
        }
    }

    pub fn read_f32<T: BitRead>(&mut self, reader: &mut T) -> std::io::Result<f32> {
        Ok(f32::from_bits(self.read_u32(reader)?))
    }

    pub fn read_i32<T: BitRead>(&mut self, reader: &mut T) -> std::io::Result<i32> {
        let mut u32_value = self.read_u32(reader)?;
        let neg_flag = u32_value & 1 != 0;
        u32_value >>= 1;
        if neg_flag {
            u32_value ^= 0xFFFF_FFFF;
        }
        Ok(u32_value as i32)
    }

    pub fn read_u32<T: BitRead>(&mut self, reader: &mut T) -> std::io::Result<u32> {
        if self.values_ptr == COMPR_BUF_SIZE {
            self.decompress_values(reader)?;
        }
        let result = self.values[self.values_ptr];
        self.values_ptr += 1;
        Ok(result)
    }

    fn decompress_values<T: BitRead>(&mut self, reader: &mut T) -> std::io::Result<()> {
        self.values_ptr = 0;
        let header = reader.read::<u32>(3)?;
        if header == HEADER_ZEROS {
            for v in &mut self.values { *v = self.prev_value; }
            return Ok(());
        }
        if header == HEADER_NO_COMPR {
            for v in &mut self.values {
                self.prev_value ^= reader.read::<u32>(32)?;
                *v = self.prev_value;
            }
            return Ok(());
        }
        let (lz_large, lz_norm, tz, use_large_lz_bits) = match header {
            HEADER_LZ =>
                (reader.read::<u32>(5)?, 0, 0, false),
            HEADER_LZ_2 =>
                (reader.read::<u32>(5)?, reader.read::<u32>(5)?, 0, true),
            HEADER_LZ_TZ =>
                (reader.read::<u32>(5)?, 0, reader.read::<u32>(5)?, false),
            HEADER_LZ_TZ_2 => (
                reader.read::<u32>(5)?, reader.read::<u32>(5)?, reader.read::<u32>(5)?, true),
            _ =>
            unreachable!(),
        };
        let large_len = (32 - lz_large - tz).max(1);
        if use_large_lz_bits {
            let norm_len = (32 - lz_norm - tz).max(1);
            let mut large_bits = reader.read::<u64>(COMPR_BUF_SIZE as u32)?;
            for v in &mut self.values {
                if (large_bits & (1 << (COMPR_BUF_SIZE-1))) == 0 {
                    self.prev_value ^= reader.read::<u32>(norm_len)? << tz;
                } else {
                    self.prev_value ^= reader.read::<u32>(large_len)? << tz;
                }
                *v = self.prev_value;
                large_bits <<= 1;
            }
        } else {
            for v in &mut self.values {
                self.prev_value ^= reader.read::<u32>(large_len)? << tz;
                *v = self.prev_value;
            }
        }
        Ok(())
    }
}

#[test]
fn test_i32_compression() {
    let mut mem_data = Vec::<u8>::new();
    let mem_writer = std::io::Cursor::new(&mut mem_data);
    let mut bit_writer = BitWriter::endian(mem_writer, BigEndian);
    let mut compressor = ValuesCompressor::new();

    compressor.write_i32(-10, &mut bit_writer).unwrap();
    compressor.write_i32(10, &mut bit_writer).unwrap();
    compressor.write_i32(-42, &mut bit_writer).unwrap();
    compressor.write_i32(42, &mut bit_writer).unwrap();

    compressor.write_i32(0, &mut bit_writer).unwrap();
    compressor.write_i32(i32::MIN, &mut bit_writer).unwrap();
    compressor.write_i32(i32::MAX, &mut bit_writer).unwrap();

    compressor.flush(&mut bit_writer).unwrap();
    bit_writer.write(8, 0).unwrap();
    drop(compressor);
    drop(bit_writer);

    let mem_reader = std::io::Cursor::new(&mem_data);
    let mut bit_reader = BitReader::endian(mem_reader, BigEndian);
    let mut decompressor = ValuesDecompressor::new();

    assert_eq!(decompressor.read_i32(&mut bit_reader).unwrap(), -10);
    assert_eq!(decompressor.read_i32(&mut bit_reader).unwrap(), 10);
    assert_eq!(decompressor.read_i32(&mut bit_reader).unwrap(), -42);
    assert_eq!(decompressor.read_i32(&mut bit_reader).unwrap(), 42);

    assert_eq!(decompressor.read_i32(&mut bit_reader).unwrap(), 0);
    assert_eq!(decompressor.read_i32(&mut bit_reader).unwrap(), i32::MIN);
    assert_eq!(decompressor.read_i32(&mut bit_reader).unwrap(), i32::MAX);
}

#[test]
fn test_compression_decompression() {
    fn test_values(values: &[f32]) {
        let mut mem_data = Vec::<u8>::new();
        let mem_writer = std::io::Cursor::new(&mut mem_data);
        let mut bit_writer = BitWriter::endian(mem_writer, BigEndian);
        let mut compressor = ValuesCompressor::new();
        for value in values {
            compressor.write_f32(*value, &mut bit_writer).unwrap();
        }
        compressor.flush(&mut bit_writer).unwrap();
        bit_writer.write(8, 0).unwrap();
        drop(compressor);
        drop(bit_writer);

        if !mem_data.is_empty() {
            let compr_coeff = (values.len() * 4) as f64 / mem_data.len() as f64;
            println!("coeff = {:.2}", compr_coeff);
        }

        let mem_reader = std::io::Cursor::new(&mem_data);
        let mut bit_reader = BitReader::endian(mem_reader, BigEndian);
        let mut decompressor = ValuesDecompressor::new();

        for value in values {
            let decompr_value = decompressor.read_f32(&mut bit_reader).unwrap();
            assert_eq!(*value, decompr_value);
        }
    }

    test_values(&[
        0.00, 0.01, 0.02, 0.03, 0.04, 0.05, 0.06, 0.07, 0.08, 0.09,
        0.10, 0.11, 0.12, 0.13, 0.14, 0.15, 0.16, 0.17, 0.18, 0.19,
        0.20, 0.21, 0.22, 0.23, 0.24, 0.25, 0.26, 0.27, 0.28, 0.29,
        0.30, 0.31
    ]);

    test_values(&[
        0.0, 1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0,
        10.0, 11.0, 12.0, 13.0, 14.0, 15.0, 16.0, 17.0, 18.0, 19.0,
        20.0, 21.0, 22.0, 23.0, 24.0, 25.0, 26.0, 27.0, 28.0, 29.0,
        30.0, 31.0, 32.0, 33.0, 34.0, 35.0, 36.0, 37.0, 38.0, 39.0,
        40.0, 41.0, 42.0, 43.0, 44.0, 45.0, 46.0, 47.0, 48.0, 49.0,
        50.0, 51.0, 52.0, 53.0, 54.0, 55.0, 56.0, 57.0, 58.0, 59.0,
        60.0, 61.0, 62.0, 63.0,
    ]);

    fn test_random_u32() {
        use rand::prelude::*;
        let len = random::<u16>();
        let mut rng = thread_rng();
        let mut values = Vec::new();
        let offset = (random::<u16>() / 8) as u32;
        for _ in 0..len {
            values.push((rng.gen::<u16>() as u32 + offset) as f32);
            if rng.gen::<u16>() & 0xFFF == 0 {
                let eq_len = random::<u16>() / 4;
                let value = rng.gen::<u16>();
                for _ in 0..eq_len {
                    values.push(value as f32);
                }
            }
        }
        test_values(&values);
    }

    fn test_random_u32_f32() {
        use rand::prelude::*;
        let len = random::<u16>();
        let mut rng = thread_rng();
        let mut values = Vec::new();
        for _ in 0..len {
            values.push(rng.gen::<u16>() as f32 / u16::MAX as f32);
            if rng.gen::<u16>() & 0xFFF == 0 {
                let eq_len = random::<u16>() / 4;
                let value = rng.gen::<u16>() as f32 / u16::MAX as f32;
                for _ in 0..eq_len {
                    values.push(value as f32);
                }
            }
        }
        test_values(&values);
    }

    fn test_random_f32() {
        use rand::prelude::*;
        let len = random::<u16>();
        let mut rng = thread_rng();
        let mut values = Vec::new();
        for _ in 0..len {
            values.push(rng.gen::<f32>());
            if rng.gen::<u16>() & 0xFFF == 0 {
                let eq_len = random::<u16>() / 4;
                let value = rng.gen::<f32>();
                for _ in 0..eq_len {
                    values.push(value as f32);
                }
            }
        }
        test_values(&values);
    }

    fn test_random_f32_neg() {
        use rand::prelude::*;
        let len = random::<u16>();
        let mut rng = thread_rng();
        let mut values = Vec::new();
        for _ in 0..len {
            values.push(rng.gen_range(-2.0..2.0));
            if rng.gen::<u16>() & 0xFFF == 0 {
                let eq_len = random::<u16>() / 4;
                let value = rng.gen_range(-10.0..10.0);
                for _ in 0..eq_len {
                    values.push(value as f32);
                }
            }
        }
        test_values(&values);
    }

    for _ in 0..100 {
        test_random_u32();
        test_random_u32_f32();
        test_random_f32();
        test_random_f32_neg();
    }
}