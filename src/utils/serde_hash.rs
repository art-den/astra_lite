// FNV-1a 64 state as an io::Write sink: zero allocations.
pub struct Fnv1aSerdeHasher(u64);

impl Fnv1aSerdeHasher {
    const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

    pub fn new() -> Self {
        Self(Self::FNV_OFFSET)
    }

    pub fn hash(self) -> u64 {
        self.0
    }
}

impl std::io::Write for Fnv1aSerdeHasher {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        for &b in buf {
            self.0 = (self.0 ^ u64::from(b)).wrapping_mul(Self::FNV_PRIME);
        }
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> { Ok(()) }
}

// let mut h = Fnv1aSerdeHasher::new();
// let _ = serde_json::to_writer(&mut h, &options.indi);
// let _ = serde_json::to_writer(&mut h, &options.cam);
// let options_hash = h.hash();
// ...
