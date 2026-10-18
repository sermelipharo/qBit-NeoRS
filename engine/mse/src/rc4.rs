/// RC4 keystream generator (as required by MSE; not for general use).
#[derive(Clone)]
pub struct Rc4 {
    s: [u8; 256],
    i: u8,
    j: u8,
}

impl Rc4 {
    pub fn new(key: &[u8]) -> Self {
        let mut s = [0u8; 256];
        for (i, v) in s.iter_mut().enumerate() {
            *v = i as u8;
        }
        let mut j: u8 = 0;
        for i in 0..256 {
            j = j.wrapping_add(s[i]).wrapping_add(key[i % key.len()]);
            s.swap(i, j as usize);
        }
        Self { s, i: 0, j: 0 }
    }

    /// MSE discards the first 1024 bytes of keystream.
    pub fn new_mse(key: &[u8]) -> Self {
        let mut rc4 = Self::new(key);
        let mut discard = [0u8; 1024];
        rc4.apply(&mut discard);
        rc4
    }

    pub fn apply(&mut self, data: &mut [u8]) {
        for b in data {
            self.i = self.i.wrapping_add(1);
            self.j = self.j.wrapping_add(self.s[self.i as usize]);
            self.s.swap(self.i as usize, self.j as usize);
            let k = self.s[(self.s[self.i as usize].wrapping_add(self.s[self.j as usize])) as usize];
            *b ^= k;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_vectors() {
        // Classic test vectors (Wikipedia).
        let mut d = *b"Plaintext";
        Rc4::new(b"Key").apply(&mut d);
        assert_eq!(d, [0xBB, 0xF3, 0x16, 0xE8, 0xD9, 0x40, 0xAF, 0x0A, 0xD3]);
        let mut d = *b"pedia";
        Rc4::new(b"Wiki").apply(&mut d);
        assert_eq!(d, [0x10, 0x21, 0xBF, 0x04, 0x20]);
    }
}
