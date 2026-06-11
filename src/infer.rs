//! The inference engine: column name -> semantic type.
//!
//! Pipeline (first match wins, cheap -> expensive):
//!   1. exact name match
//!   2. suffix / prefix rules
//!   3. token rules (name split on `_` and camelCase boundaries)
//!   4. default: short lorem
//!
//! Rules live in data (the tables below), not code, so adding one is a
//! one-liner and the whole set is testable in isolation.

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum SemanticType {
    IdInt,
    IdUuid,
    FkInt,
    Bool,
    IntSmall,
    IntBig,
    Float,
    Money,
    Percent,
    Timestamp,
    Date,
    Time,
    Email,
    Phone,
    Url,
    Ip,
    FirstName,
    LastName,
    FullName,
    Username,
    Company,
    City,
    Country,
    CountryCode,
    StreetAddress,
    Zipcode,
    Color,
    HexColor,
    Lat,
    Lng,
    StatusEnum,
    KindEnum,
    Slug,
    PasswordHash,
    Json,
    LoremShort,
    LoremLong,
    CurrencyCode,
    LanguageCode,
    MimeType,
    UserAgent,
    CreditCard,
    FileSize,
    ShortCode,
    Version,
}

/// Dialect-agnostic wire types; each protocol frontend maps these to its own
/// type identifiers (Postgres OIDs, MySQL column types, ...).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum WireType {
    Bool,
    Int4,
    Int8,
    Float8,
    Numeric,
    Text,
    Date,
    Time,
    Timestamp,
    Uuid,
    Json,
}

pub fn wire_type(st: SemanticType) -> WireType {
    use SemanticType::*;
    match st {
        IdInt | FkInt | IntBig | FileSize => WireType::Int8,
        IntSmall => WireType::Int4,
        Bool => WireType::Bool,
        Float | Lat | Lng => WireType::Float8,
        Money | Percent => WireType::Numeric,
        Timestamp => WireType::Timestamp,
        Date => WireType::Date,
        Time => WireType::Time,
        IdUuid => WireType::Uuid,
        Json => WireType::Json,
        _ => WireType::Text,
    }
}

/// The generic generator for a wire type, used when a query casts a column
/// (`x::int`) and the name-derived flavor would contradict the cast.
pub fn generic_for(wt: WireType) -> SemanticType {
    match wt {
        WireType::Bool => SemanticType::Bool,
        WireType::Int4 => SemanticType::IntSmall,
        WireType::Int8 => SemanticType::IntBig,
        WireType::Float8 => SemanticType::Float,
        WireType::Numeric => SemanticType::Money,
        WireType::Date => SemanticType::Date,
        WireType::Time => SemanticType::Time,
        WireType::Timestamp => SemanticType::Timestamp,
        WireType::Uuid => SemanticType::IdUuid,
        WireType::Json => SemanticType::Json,
        WireType::Text => SemanticType::LoremShort,
    }
}

/// Parse a SQL type name (from a `::cast` or CAST(...)) into a wire type.
pub fn wire_type_from_sql(name: &str) -> Option<WireType> {
    Some(match name.to_ascii_lowercase().as_str() {
        "bool" | "boolean" => WireType::Bool,
        "int" | "integer" | "int4" | "smallint" | "int2" => WireType::Int4,
        "bigint" | "int8" | "serial8" | "bigserial" => WireType::Int8,
        "float" | "float4" | "float8" | "real" | "double" => WireType::Float8,
        "numeric" | "decimal" | "money" => WireType::Numeric,
        "text" | "varchar" | "char" | "bpchar" | "citext" | "string" => WireType::Text,
        "date" => WireType::Date,
        "time" | "timetz" => WireType::Time,
        "timestamp" | "timestamptz" | "datetime" => WireType::Timestamp,
        "uuid" => WireType::Uuid,
        "json" | "jsonb" => WireType::Json,
        _ => return None,
    })
}

use SemanticType::*;

static EXACT: &[(&str, SemanticType)] = &[
    ("id", IdInt),
    ("pk", IdInt),
    ("uuid", IdUuid),
    ("guid", IdUuid),
    ("email", Email),
    ("mail", Email),
    ("phone", Phone),
    ("tel", Phone),
    ("mobile", Phone),
    ("age", IntSmall),
    ("count", IntBig),
    ("sum", Money),
    ("avg", Float),
    ("min", Float),
    ("max", Float),
    ("total", Money),
    ("lat", Lat),
    ("latitude", Lat),
    ("lng", Lng),
    ("lon", Lng),
    ("longitude", Lng),
    ("status", StatusEnum),
    ("state", StatusEnum),
    ("type", KindEnum),
    ("kind", KindEnum),
    ("category", KindEnum),
    ("tier", KindEnum),
    ("plan", KindEnum),
    ("password", PasswordHash),
    ("secret", PasswordHash),
    ("token", PasswordHash),
    ("hash", PasswordHash),
    ("checksum", PasswordHash),
    ("slug", Slug),
    ("url", Url),
    ("uri", Url),
    ("link", Url),
    ("website", Url),
    ("homepage", Url),
    ("ip", Ip),
    ("name", FullName),
    ("firstname", FirstName),
    ("givenname", FirstName),
    ("lastname", LastName),
    ("surname", LastName),
    ("familyname", LastName),
    ("username", Username),
    ("login", Username),
    ("handle", Username),
    ("nickname", Username),
    ("company", Company),
    ("employer", Company),
    ("organization", Company),
    ("vendor", Company),
    ("city", City),
    ("town", City),
    ("country", Country),
    ("zip", Zipcode),
    ("zipcode", Zipcode),
    ("postcode", Zipcode),
    ("address", StreetAddress),
    ("street", StreetAddress),
    ("color", Color),
    ("colour", Color),
    ("currency", CurrencyCode),
    ("locale", LanguageCode),
    ("language", LanguageCode),
    ("lang", LanguageCode),
    ("description", LoremLong),
    ("summary", LoremLong),
    ("bio", LoremLong),
    ("notes", LoremLong),
    ("note", LoremLong),
    ("comment", LoremLong),
    ("body", LoremLong),
    ("content", LoremLong),
    ("message", LoremLong),
    ("title", LoremShort),
    ("label", LoremShort),
    ("subject", LoremShort),
    ("price", Money),
    ("cost", Money),
    ("amount", Money),
    ("balance", Money),
    ("fee", Money),
    ("salary", Money),
    ("revenue", Money),
    ("quantity", IntSmall),
    ("qty", IntSmall),
    ("stock", IntSmall),
    ("rating", Float),
    ("score", Float),
    ("weight", Float),
    ("height", Float),
    ("width", Float),
    ("percent", Percent),
    ("percentage", Percent),
    ("discount", Percent),
    ("timestamp", Timestamp),
    ("date", Date),
    ("birthday", Date),
    ("dob", Date),
    ("time", Time),
    ("active", Bool),
    ("enabled", Bool),
    ("disabled", Bool),
    ("deleted", Bool),
    ("verified", Bool),
    ("confirmed", Bool),
    ("public", Bool),
    ("visible", Bool),
    ("archived", Bool),
    ("json", Json),
    ("data", Json),
    ("payload", Json),
    ("metadata", Json),
    ("settings", Json),
    ("config", Json),
    ("preferences", Json),
    ("useragent", UserAgent),
    ("mime", MimeType),
    ("mimetype", MimeType),
    ("contenttype", MimeType),
    ("size", FileSize),
    ("filesize", FileSize),
    ("bytes", FileSize),
    ("sku", ShortCode),
    ("code", ShortCode),
    ("version", Version),
    ("creditcard", CreditCard),
    ("cardnumber", CreditCard),
];

static SUFFIX: &[(&str, SemanticType)] = &[
    ("_id", FkInt),
    ("_ids", FkInt),
    ("_uuid", IdUuid),
    ("_guid", IdUuid),
    ("_at", Timestamp),
    ("_timestamp", Timestamp),
    ("_date", Date),
    ("_day", Date),
    ("_time", Time),
    ("_count", IntSmall),
    ("_qty", IntSmall),
    ("_quantity", IntSmall),
    ("_num", IntSmall),
    ("_number", IntSmall),
    ("_url", Url),
    ("_uri", Url),
    ("_link", Url),
    ("_email", Email),
    ("_phone", Phone),
    ("_price", Money),
    ("_cost", Money),
    ("_amount", Money),
    ("_total", Money),
    ("_fee", Money),
    ("_balance", Money),
    ("_pct", Percent),
    ("_percent", Percent),
    ("_rate", Percent),
    ("_ratio", Percent),
    ("_ip", Ip),
    ("_hash", PasswordHash),
    ("_token", PasswordHash),
    ("_secret", PasswordHash),
    ("_key", PasswordHash),
    ("_code", ShortCode),
    ("_city", City),
    ("_country", Country),
    ("_zip", Zipcode),
    ("_postcode", Zipcode),
    ("_slug", Slug),
    ("_json", Json),
    ("_size", FileSize),
    ("_bytes", FileSize),
    ("_version", Version),
    ("_color", Color),
    ("_lat", Lat),
    ("_lng", Lng),
    ("_lon", Lng),
    ("_username", Username),
    ("_address", StreetAddress),
    ("_status", StatusEnum),
    ("_state", StatusEnum),
    ("_type", KindEnum),
    ("_kind", KindEnum),
    ("_category", KindEnum),
    ("_name", LoremShort),
    ("_title", LoremShort),
    ("_text", LoremLong),
    ("_description", LoremLong),
];

static PREFIX: &[(&str, SemanticType)] = &[
    ("is_", Bool),
    ("has_", Bool),
    ("can_", Bool),
    ("should_", Bool),
    ("was_", Bool),
    ("did_", Bool),
    ("allow_", Bool),
    ("num_", IntSmall),
    ("count_", IntSmall),
    ("total_", IntSmall),
    ("max_", IntSmall),
    ("min_", IntSmall),
    ("avg_", Float),
];

static TOKEN: &[(&str, SemanticType)] = &[
    ("uuid", IdUuid),
    ("email", Email),
    ("phone", Phone),
    ("ip", Ip),
    ("url", Url),
    ("price", Money),
    ("cost", Money),
    ("amount", Money),
    ("salary", Money),
    ("lat", Lat),
    ("lng", Lng),
    ("lon", Lng),
    ("city", City),
    ("country", Country),
    ("zip", Zipcode),
    ("address", StreetAddress),
    ("color", Color),
    ("colour", Color),
    ("status", StatusEnum),
    ("date", Date),
    ("time", Timestamp),
    ("timestamp", Timestamp),
    ("age", IntSmall),
    ("year", IntSmall),
    ("password", PasswordHash),
    ("token", PasswordHash),
    ("currency", CurrencyCode),
    ("username", Username),
    ("company", Company),
    ("description", LoremLong),
    ("comment", LoremLong),
    ("title", LoremShort),
    ("name", FullName),
    ("slug", Slug),
    ("version", Version),
    ("json", Json),
    ("size", FileSize),
    ("count", IntSmall),
    ("percent", Percent),
    ("rate", Percent),
    ("score", Float),
    ("rating", Float),
];

/// Lowercase and convert camelCase to snake_case so one rule set covers both
/// conventions (`userId` -> `user_id`).
fn normalize(name: &str) -> String {
    let mut out = String::with_capacity(name.len() + 4);
    let mut prev_lower = false;
    for c in name.chars() {
        if c.is_ascii_uppercase() {
            if prev_lower {
                out.push('_');
            }
            out.push(c.to_ascii_lowercase());
            prev_lower = false;
        } else {
            prev_lower = c.is_ascii_lowercase() || c.is_ascii_digit();
            out.push(c);
        }
    }
    out
}

pub fn infer(name: &str) -> SemanticType {
    let n = normalize(name.trim().trim_matches('"'));
    let squashed: String = n.chars().filter(|c| *c != '_').collect();

    for (pat, st) in EXACT {
        if n == *pat || squashed == *pat {
            return *st;
        }
    }
    for (pat, st) in SUFFIX {
        if n.ends_with(pat) {
            return *st;
        }
    }
    for (pat, st) in PREFIX {
        if n.starts_with(pat) {
            return *st;
        }
    }
    let tokens: Vec<&str> = n.split(['_', '.', ' ']).filter(|t| !t.is_empty()).collect();
    for (pat, st) in TOKEN {
        if tokens.iter().any(|t| t == pat) {
            return *st;
        }
    }
    LoremShort
}

/// Resolve a name through user rules first, falling back to the built-ins.
pub fn infer_with(name: &str, rules: &Rules) -> SemanticType {
    rules.infer(name).unwrap_or_else(|| infer(name))
}

/// Parse a semantic-type name (as used in a `--rules` file) into the enum.
/// Accepts the catalog's snake_case names plus a few friendly aliases.
pub fn semantic_from_str(s: &str) -> Option<SemanticType> {
    Some(match s.to_ascii_lowercase().as_str() {
        "id_int" | "id" => IdInt,
        "id_uuid" | "uuid" | "guid" => IdUuid,
        "fk_int" | "fk" => FkInt,
        "bool" | "boolean" => Bool,
        "int_small" | "int" | "integer" => IntSmall,
        "int_big" | "bigint" | "long" => IntBig,
        "float" | "double" => Float,
        "money" | "decimal" | "currency_amount" => Money,
        "percent" | "percentage" => Percent,
        "timestamp" | "datetime" => Timestamp,
        "date" => Date,
        "time" => Time,
        "email" => Email,
        "phone" => Phone,
        "url" | "uri" => Url,
        "ip" | "ip_address" => Ip,
        "first_name" => FirstName,
        "last_name" => LastName,
        "full_name" | "name" => FullName,
        "username" | "handle" => Username,
        "company" => Company,
        "city" => City,
        "country" => Country,
        "country_code" => CountryCode,
        "street_address" | "address" => StreetAddress,
        "zipcode" | "zip" => Zipcode,
        "color" | "colour" => Color,
        "hex_color" => HexColor,
        "lat" | "latitude" => Lat,
        "lng" | "longitude" => Lng,
        "status_enum" | "status" => StatusEnum,
        "kind_enum" | "kind" | "type" => KindEnum,
        "slug" => Slug,
        "password_hash" | "hash" | "password" => PasswordHash,
        "json" | "jsonb" => Json,
        "lorem_short" | "text" | "lorem" => LoremShort,
        "lorem_long" | "paragraph" => LoremLong,
        "currency_code" => CurrencyCode,
        "language_code" | "locale" => LanguageCode,
        "mime_type" => MimeType,
        "user_agent" => UserAgent,
        "credit_card" => CreditCard,
        "file_size" => FileSize,
        "short_code" | "code" => ShortCode,
        "version" => Version,
        _ => return None,
    })
}

/// How a rule pattern is matched against a (normalized) column name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchKind {
    Exact,
    Suffix,
    Prefix,
    Token,
}

#[derive(Debug, Clone)]
struct Rule {
    kind: MatchKind,
    pat: String,
    st: SemanticType,
}

/// User-supplied inference rules, layered over the built-ins. Loaded from a
/// simple line-oriented file (no dependencies):
///
/// ```text
/// # kind   pattern        type
/// exact    coupon_code    short_code
/// suffix   _balance       money
/// prefix   flag_          bool
/// token    gateway        ip
/// ```
#[derive(Debug, Clone, Default)]
pub struct Rules {
    rules: Vec<Rule>,
}

impl Rules {
    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    pub fn len(&self) -> usize {
        self.rules.len()
    }

    /// Parse rules from text, reporting the 1-based line number on error.
    pub fn parse(text: &str) -> Result<Rules, String> {
        let mut rules = Vec::new();
        for (i, raw) in text.lines().enumerate() {
            let line = raw.split('#').next().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }
            let mut f = line.split_whitespace();
            let (Some(kind), Some(pat), Some(ty)) = (f.next(), f.next(), f.next()) else {
                return Err(format!(
                    "line {}: expected `<kind> <pattern> <type>`",
                    i + 1
                ));
            };
            if f.next().is_some() {
                return Err(format!("line {}: too many fields", i + 1));
            }
            let kind = match kind.to_ascii_lowercase().as_str() {
                "exact" => MatchKind::Exact,
                "suffix" => MatchKind::Suffix,
                "prefix" => MatchKind::Prefix,
                "token" => MatchKind::Token,
                other => {
                    return Err(format!(
                        "line {}: unknown match kind `{other}` (exact|suffix|prefix|token)",
                        i + 1
                    ));
                }
            };
            let st = semantic_from_str(ty)
                .ok_or_else(|| format!("line {}: unknown semantic type `{ty}`", i + 1))?;
            rules.push(Rule {
                kind,
                pat: normalize(pat),
                st,
            });
        }
        Ok(Rules { rules })
    }

    /// Resolve a name through the user rules. Built-in precedence is preserved
    /// (exact, then suffix, then prefix, then token); within a kind, file order.
    pub fn infer(&self, name: &str) -> Option<SemanticType> {
        if self.rules.is_empty() {
            return None;
        }
        let n = normalize(name.trim().trim_matches('"'));
        let squashed: String = n.chars().filter(|c| *c != '_').collect();
        let tokens: Vec<&str> = n.split(['_', '.', ' ']).filter(|t| !t.is_empty()).collect();
        for kind in [
            MatchKind::Exact,
            MatchKind::Suffix,
            MatchKind::Prefix,
            MatchKind::Token,
        ] {
            for r in self.rules.iter().filter(|r| r.kind == kind) {
                let hit = match kind {
                    MatchKind::Exact => n == r.pat || squashed == r.pat,
                    MatchKind::Suffix => n.ends_with(&r.pat),
                    MatchKind::Prefix => n.starts_with(&r.pat),
                    MatchKind::Token => tokens.iter().any(|t| *t == r.pat),
                };
                if hit {
                    return Some(r.st);
                }
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_matches() {
        assert_eq!(infer("id"), IdInt);
        assert_eq!(infer("email"), Email);
        assert_eq!(infer("status"), StatusEnum);
        assert_eq!(infer("price"), Money);
        assert_eq!(infer("uuid"), IdUuid);
    }

    #[test]
    fn suffix_and_prefix_rules() {
        assert_eq!(infer("user_id"), FkInt);
        assert_eq!(infer("created_at"), Timestamp);
        assert_eq!(infer("is_active"), Bool);
        assert_eq!(infer("has_children"), Bool);
        assert_eq!(infer("unit_price"), Money);
        assert_eq!(infer("avatar_url"), Url);
        assert_eq!(infer("num_retries"), IntSmall);
        assert_eq!(infer("product_name"), LoremShort);
    }

    #[test]
    fn camel_case_is_normalized() {
        assert_eq!(infer("userId"), FkInt);
        assert_eq!(infer("createdAt"), Timestamp);
        assert_eq!(infer("isActive"), Bool);
        assert_eq!(infer("firstName"), FirstName);
    }

    #[test]
    fn token_fallback() {
        assert_eq!(infer("billing_email_backup"), Email);
        assert_eq!(infer("shipping_city_2"), City);
    }

    #[test]
    fn default_is_lorem() {
        assert_eq!(infer("frobnicator"), LoremShort);
        assert_eq!(infer("xyz"), LoremShort);
    }

    #[test]
    fn exact_beats_token() {
        // "name" alone is a full name; "first_name" is exact via squashing.
        assert_eq!(infer("name"), FullName);
        assert_eq!(infer("first_name"), FirstName);
        assert_eq!(infer("last_name"), LastName);
    }

    #[test]
    fn rules_parse_and_apply() {
        let rules = Rules::parse(
            "# custom rules\n\
             exact   coupon_code   short_code\n\
             suffix  _balance      money\n\
             prefix  flag_         bool\n\
             token   gateway       ip\n",
        )
        .unwrap();
        assert_eq!(rules.len(), 4);
        assert_eq!(rules.infer("coupon_code"), Some(ShortCode));
        assert_eq!(rules.infer("wallet_balance"), Some(Money));
        assert_eq!(rules.infer("flag_enabled"), Some(Bool));
        assert_eq!(rules.infer("primary_gateway_host"), Some(Ip));
        assert_eq!(rules.infer("unrelated"), None);
    }

    #[test]
    fn rules_win_over_builtins() {
        // built-in: "status" -> StatusEnum; override it to short_code.
        let rules = Rules::parse("exact status short_code").unwrap();
        assert_eq!(infer("status"), StatusEnum);
        assert_eq!(infer_with("status", &rules), ShortCode);
        // names the rules don't cover still fall through to built-ins.
        assert_eq!(infer_with("email", &rules), Email);
    }

    #[test]
    fn rules_normalize_camel_case() {
        let rules = Rules::parse("suffix _token password_hash").unwrap();
        assert_eq!(rules.infer("apiToken"), Some(PasswordHash)); // camel -> api_token
    }

    #[test]
    fn rules_reject_bad_input() {
        assert!(Rules::parse("nope foo bar").is_err()); // unknown kind
        assert!(Rules::parse("exact foo not_a_type").is_err()); // unknown type
        assert!(Rules::parse("exact foo").is_err()); // too few fields
        // comments and blank lines are fine
        assert!(Rules::parse("\n  # just a comment\n\n").unwrap().is_empty());
    }
}
