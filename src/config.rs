use crate::infer::Rules;
use crate::shape::CrushThreshold;
use crate::theme::{self, ThemeData};

#[derive(Debug, Clone)]
pub struct Config {
    /// When set, the same query always returns the same garbage.
    pub seed: Option<u64>,
    /// Row-count band used when a query has no LIMIT.
    pub rows_min: usize,
    pub rows_max: usize,
    pub crush: CrushConfig,
    /// Vocabulary pools the value generators draw from.
    pub theme: &'static ThemeData,
    /// User inference rules, layered over the built-ins.
    pub rules: Rules,
}

#[derive(Debug, Clone)]
pub struct CrushConfig {
    /// Master switch. When false, unsafe queries get an ordinary response.
    pub enabled: bool,
    /// Upper bound on rows streamed for a single crushed query.
    pub max_rows: u64,
    /// Detect and log unsafe queries, but answer them normally.
    pub warn_only: bool,
    /// Which combination of missing-safety signals trips crush.
    pub threshold: CrushThreshold,
    /// Maximum simultaneous crush streams; further unsafe queries answer normally.
    pub concurrency: usize,
}

impl Default for CrushConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            max_rows: 5_000_000,
            warn_only: false,
            threshold: CrushThreshold::All3,
            concurrency: 4,
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            seed: None,
            rows_min: 5,
            rows_max: 20,
            crush: CrushConfig::default(),
            theme: &theme::GENERIC,
            rules: Rules::default(),
        }
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
