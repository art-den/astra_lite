
pub struct Base64Decoder {
    table:  [u8; 256],
    result: Vec<u8>,
    buffer: u32,
}

impl Base64Decoder {
    pub fn new(expected_size: usize) -> Self {
        let mut table = [0u8; 256];
        for (i, v) in table.iter_mut().enumerate() {
            let i = i as u8;
            *v = match i {
                b'A'..=b'Z' => i - b'A',
                b'a'..=b'z' => i - b'a' + 26,
                b'0'..=b'9' => i - b'0' + 52,
                b'+'        => 62,
                b'/'        => 63,
                _           => 255,
            }
        }
        Self {
            table,
            result: Vec::with_capacity(expected_size),
            buffer: 1,
        }
    }

    pub fn clear(&mut self, expected_size: usize) {
        self.result.clear();
        self.result.reserve(expected_size);
        self.buffer = 1;
    }

    pub fn take_result(&mut self) -> Vec<u8> {
        if self.buffer != 1 {
            let mut extra_len = 0;
            while self.buffer & 0x01000000 == 0 {
                self.buffer <<= 6;
                extra_len += 1;
            }
            let bytes = self.buffer.to_be_bytes();
            match extra_len {
                1 => self.result.extend_from_slice(&bytes[1..=2]),
                2 => self.result.extend_from_slice(&bytes[1..=1]),
                _ => unreachable!(),
            }
        }
        std::mem::take(&mut self.result)
    }

    pub fn add_bytes(&mut self, base64_data: &[u8]) {
        for byte in base64_data {
            self.add_byte(*byte);
        }
    }

    #[inline(always)]
    pub fn add_byte(&mut self, v: u8) {
        let index = self.table[v as usize] as u32;
        if index == 255 { return; }
        self.buffer = (self.buffer << 6) | index;
        if self.buffer & 0x01000000 != 0 {
            let bytes = self.buffer.to_be_bytes();
            self.result.extend_from_slice(&bytes[1..]);
            self.buffer = 1;
        }
    }
}

#[test]
fn test_base64_decoder() {
    fn test(base64: &[u8], result: &[u8]) {
        let mut decoder = Base64Decoder::new(0);
        decoder.add_bytes(base64);
        assert_eq!(&decoder.take_result(), result);
    }
    test(b"",                 b"");
    test(b"TWFu",             b"Man");
    test(b"TWF=",             b"Ma");
    test(b"TW==",             b"M");
    test(br#"////////"#,      &[255, 255, 255, 255, 255, 255]);
    test(br#"///////="#,      &[255, 255, 255, 255, 255]);
    test(b"dGVzdCBhIHRlc3Q=", b"test a test");
    test(b"c2hvcnRT",         b"shortS");
    test(b"c2hvcnRTMg==",     b"shortS2");
    test(b"c2hvcnRTMjM=",     b"shortS23");
    test(b"c2hvcnRTMjM0",     b"shortS234");
}
