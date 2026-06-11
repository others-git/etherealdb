use std::time::Duration;

use crate::generate::Gen;
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
    /// Fault injection + value fuzzing for client resilience testing.
    pub ghosts: GhostConfig,
}

impl Config {
    /// The value-generation context (theme + fuzz probability).
    pub fn gen_ctx(&self) -> Gen<'static> {
        Gen {
            theme: self.theme,
            fuzz: self.ghosts.fuzz,
        }
    }
}

/// Ghosts: server-side hauntings for testing how clients cope with a flaky,
/// half-real database — random latency, the odd protocol error, dropped
/// connections, and pathological values. Probabilities are 0.0..=1.0; all
/// default to off. Decisions use a thread RNG, so they stay random even in
/// `--seed` (deterministic-data) mode.
#[derive(Debug, Clone, Default)]
pub struct GhostConfig {
    /// Chance of delaying a response, and the delay band (ms).
    pub latency_prob: f64,
    pub latency_ms: (u64, u64),
    /// Chance of answering a command with a protocol error instead of a result.
    pub error_prob: f64,
    /// Chance of dropping the connection mid-conversation.
    pub drop_prob: f64,
    /// Chance, per value, of emitting a pathological value to stress the client.
    pub fuzz: f64,
}

fn hits(p: f64) -> bool {
    p > 0.0 && rand::random::<f64>() < p
}

impl GhostConfig {
    /// True if any fault-injection knob (not fuzz) is active.
    pub fn haunting(&self) -> bool {
        self.latency_prob > 0.0 || self.error_prob > 0.0 || self.drop_prob > 0.0
    }

    /// Possibly sleep before responding.
    pub async fn maybe_latency(&self) {
        if hits(self.latency_prob) {
            let (lo, hi) = self.latency_ms;
            let ms = if hi > lo {
                lo + rand::random::<u64>() % (hi - lo + 1)
            } else {
                lo
            };
            tokio::time::sleep(Duration::from_millis(ms)).await;
        }
    }

    /// True if this command should be answered with a protocol error.
    pub fn maybe_error(&self) -> bool {
        hits(self.error_prob)
    }

    /// True if the connection should be dropped now.
    pub fn maybe_drop(&self) -> bool {
        hits(self.drop_prob)
    }
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
            ghosts: GhostConfig::default(),
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
