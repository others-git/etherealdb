//! Robustness fuzzing: throw a flood of random and adversarial input at the
//! query/inference parsers and assert they never panic. EtherealDB accepts any
//! query, so its parsers must survive any bytes a client sends.

use etherealdb::infer::{self, Rules};
use etherealdb::shape;
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};

/// A grab-bag of characters that tend to break hand-written scanners.
const ALPHABET: &[char] = &[
    'a', 'Z', '0', '9', '_', ' ', '\t', '\n', '\r', '\'', '"', '`', '(', ')', '[', ']', '{', '}',
    ',', ';', '.', ':', '*', '/', '\\', '-', '+', '=', '<', '>', '$', '#', '%', '∑', '👻', '\u{0}',
    'é', '人',
];

const KEYWORDS: &[&str] = &[
    "select",
    "SELECT",
    "from",
    "where",
    "insert",
    "into",
    "update",
    "delete",
    "create",
    "table",
    "*",
    "count(*)",
    "as",
    "limit",
    "::int",
    "$1",
    "(",
    ")",
    "'",
    "--",
    "/*",
    "pg_catalog.",
    "1.5",
    "u.email",
    "version()",
    "current_user",
    "and",
    "or",
    "join",
    "on",
];

fn random_query(rng: &mut impl Rng) -> String {
    let mut s = String::new();
    let len = rng.random_range(0..40);
    for _ in 0..len {
        if rng.random_bool(0.5) {
            s.push_str(KEYWORDS[rng.random_range(0..KEYWORDS.len())]);
            s.push(' ');
        } else {
            let n = rng.random_range(0..8);
            for _ in 0..n {
                s.push(ALPHABET[rng.random_range(0..ALPHABET.len())]);
            }
        }
    }
    s
}

#[test]
fn parsers_never_panic_on_random_input() {
    let mut rng = StdRng::seed_from_u64(0xE7E54);
    let rules = Rules::default();
    for _ in 0..50_000 {
        let q = random_query(&mut rng);

        // Every parser entry point must tolerate arbitrary bytes.
        for stmt in shape::split_statements(&q) {
            let shape = shape::extract(stmt);
            for col in &shape.columns {
                let _ = infer::infer(&col.name);
            }
            let _ = shape.crush_class(etherealdb::shape::CrushThreshold::All3);
        }
        let _ = shape::param_types(&q, &rules);
        let _ = infer::infer(&q);
    }
}

#[test]
fn rules_parser_never_panics() {
    let mut rng = StdRng::seed_from_u64(99);
    for _ in 0..10_000 {
        let mut line = String::new();
        let n = rng.random_range(0..12);
        for _ in 0..n {
            line.push(ALPHABET[rng.random_range(0..ALPHABET.len())]);
            if rng.random_bool(0.3) {
                line.push(' ');
            }
        }
        // Parsing may Err, but must never panic.
        let _ = Rules::parse(&line);
    }
}

#[test]
fn value_inference_handles_pathological_names() {
    // Names that are empty, huge, or all punctuation must still resolve.
    for name in [
        "",
        " ",
        "\0",
        &"x".repeat(100_000),
        "...:::",
        "👻_id",
        "SELECT",
    ] {
        let _ = infer::infer(name);
    }
}
