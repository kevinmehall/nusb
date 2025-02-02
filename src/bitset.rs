/// Bitset capable of storing 0x00..=0x0f and 0x80..=0x8f
#[derive(Default, Clone, Copy)]
pub struct EndpointBitSet(u32);

impl EndpointBitSet {
    pub fn mask(ep: u8) -> u32 {
        let bit = ((ep & 0x0f) << 1) | (ep >> 7);
        1 << bit
    }

    pub fn is_set(&self, bit: u8) -> bool {
        self.0 & Self::mask(bit) != 0
    }

    pub fn is_empty(&self) -> bool {
        self.0 == 0
    }

    pub fn set(&mut self, bit: u8) {
        self.0 |= Self::mask(bit)
    }

    pub fn clear(&mut self, bit: u8) {
        self.0 &= !Self::mask(bit)
    }
}
