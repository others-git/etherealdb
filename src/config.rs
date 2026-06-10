#[derive(Debug, Clone)]
pub struct Config {
    /// When set, the same query always returns the same garbage.
    pub seed: Option<u64>,
    /// Row-count band used when a query has no LIMIT.
    pub rows_min: usize,
    pub rows_max: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self { seed: None, rows_min: 5, rows_max: 20 }
    }
}

/// FNV-1a, used to derive per-query RNG seeds. Stable across releases,
/// unlike std's DefaultHasher.
pub fn fnv64(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}
