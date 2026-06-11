//! Value generators: semantic type + RNG -> a plausible-looking string.
//! All output is the text representation a SQL client expects to see.

use rand::Rng;
use rand::seq::IndexedRandom;

use crate::infer::SemanticType;

static FIRST_NAMES: &[&str] = &[
    "Alice", "Marcus", "Yuki", "Priya", "Omar", "Ingrid", "Chen", "Fatima", "Diego", "Astrid",
    "Kofi", "Elena", "Hiro", "Zara", "Lars", "Amara", "Felix", "Noor", "Mateo", "Sigrid", "Ravi",
    "Lucia", "Emeka", "Hannah", "Tomas", "Mei", "Andrei", "Leila", "Bjorn", "Carmen", "Dmitri",
    "Aisha", "Pablo", "Greta", "Kenji", "Rosa", "Viktor", "Nadia", "Sven", "Imani",
];

static LAST_NAMES: &[&str] = &[
    "Okafor",
    "Lindqvist",
    "Tanaka",
    "Petrov",
    "Garcia",
    "Nguyen",
    "Schmidt",
    "Rossi",
    "Kowalski",
    "Andersson",
    "Yamamoto",
    "Silva",
    "Novak",
    "Haddad",
    "Eriksen",
    "Moreau",
    "Castillo",
    "Weber",
    "Ivanova",
    "Fernandez",
    "Larsen",
    "Dubois",
    "Hoffmann",
    "Marino",
    "Sato",
    "Virtanen",
    "Costa",
    "Bergström",
    "Olawale",
    "Kimura",
    "Vasquez",
    "Lehmann",
    "Romano",
    "Park",
    "Almeida",
    "Fischer",
];

static COMPANY_STEMS: &[&str] = &[
    "Lumen", "Vexel", "Quark", "Nimbus", "Apex", "Zephyr", "Onyx", "Fathom", "Strato", "Ember",
    "Cobalt", "Drift", "Pulse", "Vertex", "Halcyon", "Mosaic", "Tundra", "Cinder", "Meridian",
    "Flux",
];

static COMPANY_SUFFIXES: &[&str] = &[
    "Labs",
    "Systems",
    "Dynamics",
    "Industries",
    "Works",
    "Forge",
    "Analytics",
    "Logic",
    "Networks",
    "Holdings",
    "Collective",
    "Technologies",
    "Group",
    "Co.",
];

static CITIES: &[&str] = &[
    "Portland",
    "Osaka",
    "Helsinki",
    "Marseille",
    "Cartagena",
    "Tbilisi",
    "Brisbane",
    "Kraków",
    "Valparaíso",
    "Tampere",
    "Da Nang",
    "Leipzig",
    "Galway",
    "Kumasi",
    "Bologna",
    "Sapporo",
    "Rotterdam",
    "Antwerp",
    "Curitiba",
    "Ljubljana",
    "Wellington",
    "Porto",
    "Gdańsk",
    "Bandung",
];

static COUNTRIES: &[&str] = &[
    "Japan",
    "Brazil",
    "Finland",
    "Ghana",
    "Portugal",
    "New Zealand",
    "Poland",
    "Chile",
    "Vietnam",
    "Germany",
    "Ireland",
    "Georgia",
    "Australia",
    "Netherlands",
    "Slovenia",
    "Indonesia",
    "Italy",
    "Belgium",
    "Canada",
    "Morocco",
    "Estonia",
    "Uruguay",
];

static COUNTRY_CODES: &[&str] = &[
    "JP", "BR", "FI", "GH", "PT", "NZ", "PL", "CL", "VN", "DE", "IE", "GE", "AU", "NL", "SI", "ID",
    "IT", "BE", "CA", "MA", "EE", "UY",
];

static STREET_NAMES: &[&str] = &[
    "Maple",
    "Harbor",
    "Juniper",
    "Birchwood",
    "Cedar",
    "Foxglove",
    "Larkspur",
    "Willow",
    "Granite",
    "Meadow",
    "Alder",
    "Bayview",
    "Clover",
    "Driftwood",
    "Elm",
    "Hazel",
];

static STREET_TYPES: &[&str] = &[
    "St", "Ave", "Blvd", "Lane", "Way", "Court", "Drive", "Terrace",
];

static COLORS: &[&str] = &[
    "crimson",
    "teal",
    "ochre",
    "violet",
    "chartreuse",
    "indigo",
    "coral",
    "slate",
    "amber",
    "fuchsia",
    "olive",
    "cerulean",
    "maroon",
    "mint",
    "lavender",
    "rust",
];

static STATUSES: &[&str] = &[
    "active",
    "pending",
    "inactive",
    "archived",
    "suspended",
    "expired",
];

static KINDS: &[&str] = &[
    "standard",
    "premium",
    "basic",
    "trial",
    "enterprise",
    "legacy",
];

static DOMAINS: &[&str] = &[
    "lumenforge.io",
    "vexel.dev",
    "quarkmail.com",
    "nimbus.net",
    "fathom.app",
    "stratolabs.io",
    "embermail.org",
    "cobalt.systems",
    "driftbox.com",
    "pulsewire.net",
];

static WORDS: &[&str] = &[
    "ambient", "brisk", "cobalt", "drift", "ember", "fathom", "glint", "hollow", "iris",
    "junction", "keel", "lattice", "moss", "nimble", "orbit", "plume", "quill", "ripple", "sable",
    "tide", "umber", "vesper", "wisp", "yonder", "zenith", "arc", "bloom", "cairn", "delta",
    "echo", "fern", "grove", "haze", "inlet", "knoll", "loom", "mirth", "north", "opal", "pith",
];

static CURRENCY_CODES: &[&str] = &[
    "USD", "EUR", "JPY", "GBP", "BRL", "PLN", "AUD", "CAD", "CHF", "SEK",
];

static LANGUAGE_CODES: &[&str] = &[
    "en", "ja", "pt", "fi", "pl", "de", "es", "vi", "it", "nl", "fr", "ko",
];

static MIME_TYPES: &[&str] = &[
    "application/json",
    "text/html",
    "image/png",
    "image/jpeg",
    "application/pdf",
    "text/csv",
    "audio/mpeg",
    "video/mp4",
    "application/zip",
    "text/plain",
];

static USER_AGENTS: &[&str] = &[
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36",
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 14_5) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.5 Safari/605.1.15",
    "Mozilla/5.0 (X11; Linux x86_64; rv:127.0) Gecko/20100101 Firefox/127.0",
    "Mozilla/5.0 (iPhone; CPU iPhone OS 17_5 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Mobile/15E148",
];

fn pick<'a>(rng: &mut impl Rng, items: &'a [&'a str]) -> &'a str {
    items.choose(rng).unwrap()
}

fn word(rng: &mut impl Rng) -> &'static str {
    WORDS.choose(rng).unwrap()
}

/// Civil-from-days (Howard Hinnant's algorithm): days since 1970-01-01 -> (y, m, d).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (y + (m <= 2) as i64, m, d)
}

/// Inverse of `civil_from_days` (Howard Hinnant): (y, m, d) -> days since the
/// Unix epoch. Used to re-encode our text dates into Postgres binary format.
pub fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let yoe = y - era * 400; // [0, 399]
    let m = m as i64;
    let d = d as i64;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146097 + doe - 719468
}

/// Roughly "now" (mid-2026); fake timestamps land within ~2 years before this.
const NOW_EPOCH: i64 = 1_781_049_600;
const TWO_YEARS: i64 = 63_113_904;

fn random_epoch(rng: &mut impl Rng) -> i64 {
    NOW_EPOCH - rng.random_range(0..TWO_YEARS)
}

fn fmt_date(epoch: i64) -> String {
    let (y, m, d) = civil_from_days(epoch.div_euclid(86400));
    format!("{y:04}-{m:02}-{d:02}")
}

fn fmt_time(epoch: i64) -> String {
    let s = epoch.rem_euclid(86400);
    format!("{:02}:{:02}:{:02}", s / 3600, (s % 3600) / 60, s % 60)
}

fn fmt_timestamp(epoch: i64) -> String {
    format!("{} {}", fmt_date(epoch), fmt_time(epoch))
}

pub fn generate(st: SemanticType, rng: &mut impl Rng) -> String {
    use SemanticType::*;
    match st {
        IdInt => rng.random_range(1..1_000_000i64).to_string(),
        FkInt => rng.random_range(1..5_000i64).to_string(),
        IdUuid => {
            let mut b = [0u8; 16];
            rng.fill(&mut b);
            uuid::Builder::from_random_bytes(b).into_uuid().to_string()
        }
        Bool => if rng.random_bool(0.5) { "t" } else { "f" }.to_string(),
        IntSmall => rng.random_range(0..100i64).to_string(),
        IntBig => rng.random_range(0..1_000_000i64).to_string(),
        Float => format!("{:.2}", rng.random_range(0.0..1000.0)),
        Money => format!("{:.2}", rng.random_range(0.99..9999.99)),
        Percent => format!("{:.2}", rng.random_range(0.0..100.0)),
        Timestamp => fmt_timestamp(random_epoch(rng)),
        Date => fmt_date(random_epoch(rng)),
        Time => fmt_time(random_epoch(rng)),
        Email => {
            let f = pick(rng, FIRST_NAMES).to_lowercase();
            let l = pick(rng, LAST_NAMES).to_lowercase();
            format!("{f}.{l}@{}", pick(rng, DOMAINS))
        }
        Phone => format!(
            "+1 ({}) {}-{:04}",
            rng.random_range(200..990),
            rng.random_range(200..990),
            rng.random_range(0..10_000)
        ),
        Url => format!("https://{}/{}-{}", pick(rng, DOMAINS), word(rng), word(rng)),
        Ip => format!(
            "{}.{}.{}.{}",
            rng.random_range(1..240),
            rng.random_range(0..256),
            rng.random_range(0..256),
            rng.random_range(1..255)
        ),
        FirstName => pick(rng, FIRST_NAMES).to_string(),
        LastName => pick(rng, LAST_NAMES).to_string(),
        FullName => format!("{} {}", pick(rng, FIRST_NAMES), pick(rng, LAST_NAMES)),
        Username => format!(
            "{}{}",
            pick(rng, FIRST_NAMES).to_lowercase(),
            rng.random_range(1..100)
        ),
        Company => format!(
            "{} {}",
            pick(rng, COMPANY_STEMS),
            pick(rng, COMPANY_SUFFIXES)
        ),
        City => pick(rng, CITIES).to_string(),
        Country => pick(rng, COUNTRIES).to_string(),
        CountryCode => pick(rng, COUNTRY_CODES).to_string(),
        StreetAddress => format!(
            "{} {} {}",
            rng.random_range(1..9999),
            pick(rng, STREET_NAMES),
            pick(rng, STREET_TYPES)
        ),
        Zipcode => format!("{:05}", rng.random_range(501..99950)),
        Color => pick(rng, COLORS).to_string(),
        HexColor => format!("#{:06x}", rng.random_range(0..0x1000000u32)),
        Lat => format!("{:.6}", rng.random_range(-90.0..90.0)),
        Lng => format!("{:.6}", rng.random_range(-180.0..180.0)),
        StatusEnum => pick(rng, STATUSES).to_string(),
        KindEnum => pick(rng, KINDS).to_string(),
        Slug => format!("{}-{}-{}", word(rng), word(rng), word(rng)),
        PasswordHash => {
            const CS: &[u8] = b"./ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
            let tail: String = (0..53)
                .map(|_| CS[rng.random_range(0..CS.len())] as char)
                .collect();
            format!("$2b$12${tail}")
        }
        Json => format!(
            r#"{{"{}": "{}", "count": {}, "active": {}}}"#,
            word(rng),
            word(rng),
            rng.random_range(0..100),
            rng.random_bool(0.5)
        ),
        LoremShort => {
            let n = rng.random_range(2..=4);
            let mut s = (0..n).map(|_| word(rng)).collect::<Vec<_>>().join(" ");
            if let Some(c) = s.get_mut(0..1) {
                c.make_ascii_uppercase();
            }
            s
        }
        LoremLong => {
            let n = rng.random_range(8..=16);
            let mut s = (0..n).map(|_| word(rng)).collect::<Vec<_>>().join(" ");
            if let Some(c) = s.get_mut(0..1) {
                c.make_ascii_uppercase();
            }
            s.push('.');
            s
        }
        CurrencyCode => pick(rng, CURRENCY_CODES).to_string(),
        LanguageCode => pick(rng, LANGUAGE_CODES).to_string(),
        MimeType => pick(rng, MIME_TYPES).to_string(),
        UserAgent => pick(rng, USER_AGENTS).to_string(),
        CreditCard => format!(
            "4{:03} {:04} {:04} {:04}",
            rng.random_range(0..1000),
            rng.random_range(0..10_000),
            rng.random_range(0..10_000),
            rng.random_range(0..10_000)
        ),
        FileSize => rng.random_range(128..10_000_000_000i64).to_string(),
        ShortCode => {
            const CS: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
            (0..8)
                .map(|_| CS[rng.random_range(0..CS.len())] as char)
                .collect()
        }
        Version => format!(
            "{}.{}.{}",
            rng.random_range(0..10),
            rng.random_range(0..20),
            rng.random_range(0..30)
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::SeedableRng;
    use rand_chacha::ChaCha8Rng;

    #[test]
    fn email_looks_like_email() {
        let mut rng = ChaCha8Rng::seed_from_u64(1);
        let v = generate(SemanticType::Email, &mut rng);
        assert!(v.contains('@') && v.contains('.'));
    }

    #[test]
    fn deterministic_with_same_seed() {
        let mut a = ChaCha8Rng::seed_from_u64(42);
        let mut b = ChaCha8Rng::seed_from_u64(42);
        for st in [
            SemanticType::Email,
            SemanticType::Timestamp,
            SemanticType::Money,
            SemanticType::IdUuid,
        ] {
            assert_eq!(generate(st, &mut a), generate(st, &mut b));
        }
    }

    #[test]
    fn date_formats() {
        let mut rng = ChaCha8Rng::seed_from_u64(7);
        let ts = generate(SemanticType::Timestamp, &mut rng);
        // "YYYY-MM-DD HH:MM:SS"
        assert_eq!(ts.len(), 19, "bad timestamp: {ts}");
        assert_eq!(&ts[4..5], "-");
        assert_eq!(&ts[10..11], " ");
        let d = generate(SemanticType::Date, &mut rng);
        assert_eq!(d.len(), 10, "bad date: {d}");
    }

    #[test]
    fn bool_is_pg_text_format() {
        let mut rng = ChaCha8Rng::seed_from_u64(3);
        let v = generate(SemanticType::Bool, &mut rng);
        assert!(v == "t" || v == "f");
    }

    #[test]
    fn known_epoch_renders_correctly() {
        // 2024-02-29 12:00:00 UTC (leap day) = 1709208000
        assert_eq!(fmt_timestamp(1_709_208_000), "2024-02-29 12:00:00");
        assert_eq!(fmt_date(0), "1970-01-01");
    }

    #[test]
    fn days_from_civil_roundtrips() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(2000, 1, 1), 10957); // Postgres epoch
        for &days in &[0i64, 10957, 20000, -3650, 19783] {
            let (y, m, d) = civil_from_days(days);
            assert_eq!(days_from_civil(y, m, d), days, "roundtrip failed at {days}");
        }
    }
}
