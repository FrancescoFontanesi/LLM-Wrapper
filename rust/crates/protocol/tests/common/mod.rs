#![allow(dead_code)] // each test crate uses a different subset of these helpers

//! Deterministic pseudo-random numbers for the property tests (no external crate, reproducible).

/// xorshift64*: tiny, fast, good enough to explore input space deterministically.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed.max(1))
    }

    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    /// Uniform in `0..n` (`n > 0`).
    pub fn below(&mut self, n: usize) -> usize {
        usize::try_from(self.next_u64() % u64::try_from(n).unwrap()).unwrap()
    }

    pub fn chance(&mut self, percent: usize) -> bool {
        self.below(100) < percent
    }

    pub fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len())]
    }

    pub fn string_from(&mut self, alphabet: &str, min: usize, max: usize) -> String {
        let chars: Vec<char> = alphabet.chars().collect();
        let len = min + self.below(max - min + 1);
        (0..len).map(|_| *self.pick(&chars)).collect()
    }

    pub fn bytes(&mut self, max_len: usize) -> Vec<u8> {
        let len = self.below(max_len + 1);
        (0..len)
            .map(|_| u8::try_from(self.next_u64() & 0xff).unwrap())
            .collect()
    }
}
