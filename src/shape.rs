//! Query shape extraction: learn just enough from raw SQL to fake a result.
//! Not a SQL parser — a scanner that finds column names/aliases, the table
//! hint, LIMIT, and the statement kind, while ignoring everything else.

use crate::infer::{WireType, wire_type_from_sql};

#[derive(Debug, Clone, PartialEq)]
pub enum StmtKind {
    Select,
    Insert,
    Update,
    Delete,
    /// Anything else; payload is the CommandComplete tag to send (e.g. "BEGIN").
    Command(String),
    Empty,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ColumnSpec {
    pub name: String,
    /// Explicit `::type` cast found in the query.
    pub cast: Option<WireType>,
    /// Literal value to echo back (e.g. `SELECT 1`), with its wire type.
    pub literal: Option<(String, WireType)>,
    /// True for count()/sum()/avg()/min()/max() — an aggregate-only select
    /// returns exactly one row.
    pub aggregate: bool,
}

impl ColumnSpec {
    fn named(name: impl Into<String>) -> Self {
        Self { name: name.into(), cast: None, literal: None, aggregate: false }
    }
}

#[derive(Debug, Clone)]
pub struct ResultShape {
    pub kind: StmtKind,
    pub columns: Vec<ColumnSpec>,
    pub table_hint: Option<String>,
    pub limit: Option<usize>,
}

/// Columns conjured for `SELECT *` — we have no schema, so every table
/// conveniently has these.
static STAR_COLUMNS: &[&str] = &["id", "name", "email", "status", "created_at"];

/// Split a query buffer into statements on top-level semicolons.
pub fn split_statements(sql: &str) -> Vec<&str> {
    split_top_level(sql, ';')
}

/// Split on `sep` outside parens, single quotes, and double quotes.
fn split_top_level(s: &str, sep: char) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut depth = 0i32;
    let mut in_sq = false;
    let mut in_dq = false;
    let mut start = 0;
    for (i, c) in s.char_indices() {
        match c {
            '\'' if !in_dq => in_sq = !in_sq,
            '"' if !in_sq => in_dq = !in_dq,
            '(' | '[' if !in_sq && !in_dq => depth += 1,
            ')' | ']' if !in_sq && !in_dq => depth -= 1,
            c if c == sep && depth <= 0 && !in_sq && !in_dq => {
                parts.push(&s[start..i]);
                start = i + sep.len_utf8();
            }
            _ => {}
        }
    }
    parts.push(&s[start..]);
    parts.into_iter().map(str::trim).filter(|p| !p.is_empty()).collect()
}

/// Word tokens (lowercased) at top level, with byte offsets.
fn scan_words(s: &str) -> Vec<(String, usize, usize)> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut in_sq = false;
    let mut in_dq = false;
    let mut word_start: Option<usize> = None;
    for (i, c) in s.char_indices() {
        let is_word = c.is_ascii_alphanumeric() || c == '_';
        if in_sq || in_dq || depth > 0 {
            // close out any word that started before we entered a nested region
            if let Some(ws) = word_start.take() {
                out.push((s[ws..i].to_ascii_lowercase(), ws, i));
            }
        } else if is_word {
            if word_start.is_none() {
                word_start = Some(i);
            }
        } else if let Some(ws) = word_start.take() {
            out.push((s[ws..i].to_ascii_lowercase(), ws, i));
        }
        match c {
            '\'' if !in_dq => in_sq = !in_sq,
            '"' if !in_sq => in_dq = !in_dq,
            '(' | '[' if !in_sq && !in_dq => depth += 1,
            ')' | ']' if !in_sq && !in_dq => depth -= 1,
            _ => {}
        }
    }
    if let Some(ws) = word_start {
        out.push((s[ws..].to_ascii_lowercase(), ws, s.len()));
    }
    out
}

pub fn extract(stmt: &str) -> ResultShape {
    let stmt = stmt.trim().trim_end_matches(';').trim();
    if stmt.is_empty() {
        return ResultShape { kind: StmtKind::Empty, columns: vec![], table_hint: None, limit: None };
    }
    let words = scan_words(stmt);
    let first = words.first().map(|(w, _, _)| w.as_str()).unwrap_or("");

    match first {
        "select" | "with" | "table" => extract_select(stmt, &words),
        "insert" => ResultShape {
            kind: StmtKind::Insert,
            columns: vec![],
            table_hint: word_after(&words, stmt, "into"),
            limit: None,
        },
        "update" => ResultShape {
            kind: StmtKind::Update,
            columns: vec![],
            table_hint: word_after(&words, stmt, "update"),
            limit: None,
        },
        "delete" => ResultShape {
            kind: StmtKind::Delete,
            columns: vec![],
            table_hint: word_after(&words, stmt, "from"),
            limit: None,
        },
        "show" => {
            // SHOW x -> one column named x, one row. A couple of params get
            // believable fixed values; the rest get "on".
            let param = words.get(1).map(|(w, _, _)| w.clone()).unwrap_or_else(|| "setting".into());
            let value = match param.as_str() {
                "server_version" => "16.3 (EtherealDB 0.1.0)".to_string(),
                "server_encoding" | "client_encoding" => "UTF8".to_string(),
                "transaction_isolation" => "read committed".to_string(),
                _ => "on".to_string(),
            };
            ResultShape {
                kind: StmtKind::Select,
                columns: vec![ColumnSpec {
                    name: param,
                    cast: None,
                    literal: Some((value, WireType::Text)),
                    aggregate: false,
                }],
                table_hint: None,
                limit: Some(1),
            }
        }
        _ => {
            // BEGIN/COMMIT/SET/CREATE TABLE/... — ack with a plausible tag.
            let mut tag = first.to_ascii_uppercase();
            if matches!(first, "create" | "drop" | "alter" | "truncate") {
                if let Some((w, _, _)) = words.get(1) {
                    tag = format!("{tag} {}", w.to_ascii_uppercase());
                }
            }
            ResultShape { kind: StmtKind::Command(tag), columns: vec![], table_hint: None, limit: None }
        }
    }
}

fn word_after(words: &[(String, usize, usize)], stmt: &str, kw: &str) -> Option<String> {
    let pos = words.iter().position(|(w, _, _)| w == kw)?;
    // The table name may be quoted/schema-qualified, so read raw chars after
    // the keyword rather than relying on the word scanner.
    let after = stmt[words[pos].2..].trim_start();
    let end = after
        .find(|c: char| c.is_whitespace() || matches!(c, ',' | ';' | '(' | ')'))
        .unwrap_or(after.len());
    let raw = &after[..end];
    let name = raw.rsplit('.').next().unwrap_or(raw).trim_matches('"');
    if name.is_empty() { None } else { Some(name.to_ascii_lowercase()) }
}

fn extract_select(stmt: &str, words: &[(String, usize, usize)]) -> ResultShape {
    // For WITH queries the CTE bodies are inside parens (depth > 0), so the
    // first top-level "select" is the main one.
    let sel = words.iter().position(|(w, _, _)| w == "select");
    let Some(sel) = sel else {
        return ResultShape {
            kind: StmtKind::Command("SELECT".into()),
            columns: vec![],
            table_hint: None,
            limit: None,
        };
    };

    const LIST_END: &[&str] =
        &["from", "where", "group", "order", "having", "limit", "offset", "union", "except", "intersect", "into", "for"];
    let end = words[sel + 1..]
        .iter()
        .find(|(w, _, _)| LIST_END.contains(&w.as_str()))
        .map(|(_, s, _)| *s)
        .unwrap_or(stmt.len());

    let mut cols_text = stmt[words[sel].2..end].trim();
    // skip DISTINCT / ALL
    for kw in ["distinct", "all"] {
        let lower = cols_text.to_ascii_lowercase();
        if lower.starts_with(kw)
            && cols_text[kw.len()..].starts_with(|c: char| c.is_whitespace())
        {
            cols_text = cols_text[kw.len()..].trim_start();
        }
    }

    let mut columns = Vec::new();
    for item in split_top_level(cols_text, ',') {
        parse_item(item, &mut columns);
    }

    let table_hint = word_after(words, stmt, "from");
    let limit = words
        .iter()
        .position(|(w, _, _)| w == "limit")
        .and_then(|i| words.get(i + 1))
        .and_then(|(w, _, _)| w.parse::<usize>().ok());

    ResultShape { kind: StmtKind::Select, columns, table_hint, limit }
}

fn parse_item(item: &str, out: &mut Vec<ColumnSpec>) {
    let item = item.trim();
    if item.is_empty() {
        return;
    }
    if item == "*" || item.ends_with(".*") {
        out.extend(STAR_COLUMNS.iter().map(|c| ColumnSpec::named(*c)));
        return;
    }

    // top-level AS alias
    let words = scan_words(item);
    let (expr, alias) = match words.iter().position(|(w, _, _)| w == "as") {
        Some(i) => {
            let alias = item[words[i].2..].trim().trim_matches('"').to_string();
            (item[..words[i].1].trim(), Some(alias))
        }
        None => (item, None),
    };

    // top-level ::cast — take the last one ("a::text::int" is legal SQL)
    let (expr, cast) = match find_top_level_cast(expr) {
        Some(pos) => {
            let ty: String = expr[pos + 2..]
                .trim()
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                .collect();
            (expr[..pos].trim(), wire_type_from_sql(&ty))
        }
        None => (expr, None),
    };

    let mut spec = classify_expr(expr);
    if let Some(a) = alias {
        spec.name = a;
    }
    if cast.is_some() {
        spec.cast = cast;
        // a cast on a literal also retypes the echo
        if let Some((v, _)) = spec.literal.take() {
            spec.literal = Some((v, cast.unwrap()));
        }
    }
    out.push(spec);
}

fn find_top_level_cast(s: &str) -> Option<usize> {
    let mut depth = 0i32;
    let mut in_sq = false;
    let mut in_dq = false;
    let mut found = None;
    let bytes = s.as_bytes();
    for (i, c) in s.char_indices() {
        match c {
            '\'' if !in_dq => in_sq = !in_sq,
            '"' if !in_sq => in_dq = !in_dq,
            '(' | '[' if !in_sq && !in_dq => depth += 1,
            ')' | ']' if !in_sq && !in_dq => depth -= 1,
            ':' if depth == 0 && !in_sq && !in_dq && bytes.get(i + 1) == Some(&b':') => {
                found = Some(i);
            }
            _ => {}
        }
    }
    found
}

fn classify_expr(expr: &str) -> ColumnSpec {
    let expr = expr.trim();

    // numeric literal
    if !expr.is_empty()
        && expr.chars().enumerate().all(|(i, c)| {
            c.is_ascii_digit() || c == '.' || (i == 0 && c == '-')
        })
        && expr.chars().any(|c| c.is_ascii_digit())
    {
        let wt = if expr.contains('.') { WireType::Numeric } else { WireType::Int4 };
        return ColumnSpec {
            name: "?column?".into(),
            cast: None,
            literal: Some((expr.to_string(), wt)),
            aggregate: false,
        };
    }

    // string literal
    if expr.len() >= 2 && expr.starts_with('\'') && expr.ends_with('\'') {
        let inner = expr[1..expr.len() - 1].replace("''", "'");
        return ColumnSpec {
            name: "?column?".into(),
            cast: None,
            literal: Some((inner, WireType::Text)),
            aggregate: false,
        };
    }

    // function call: name(...)
    if let Some(paren) = expr.find('(') {
        let fname = expr[..paren].trim().to_ascii_lowercase();
        if !fname.is_empty() && fname.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            let aggregate = matches!(fname.as_str(), "count" | "sum" | "avg" | "min" | "max");
            let mut spec = ColumnSpec::named(fname.rsplit('.').next().unwrap_or(&fname));
            spec.aggregate = aggregate;
            return spec;
        }
    }

    // plain (possibly qualified) identifier: u.email -> email
    let name = expr.rsplit('.').next().unwrap_or(expr).trim().trim_matches('"');
    if !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == ' ') {
        return ColumnSpec::named(name);
    }

    // arbitrary expression we can't name
    ColumnSpec::named("?column?")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(shape: &ResultShape) -> Vec<&str> {
        shape.columns.iter().map(|c| c.name.as_str()).collect()
    }

    #[test]
    fn basic_select() {
        let s = extract("SELECT id, email, created_at FROM users");
        assert_eq!(s.kind, StmtKind::Select);
        assert_eq!(names(&s), ["id", "email", "created_at"]);
        assert_eq!(s.table_hint.as_deref(), Some("users"));
        assert_eq!(s.limit, None);
    }

    #[test]
    fn star_expansion_and_limit() {
        let s = extract("select * from orders limit 5");
        assert_eq!(names(&s), ["id", "name", "email", "status", "created_at"]);
        assert_eq!(s.table_hint.as_deref(), Some("orders"));
        assert_eq!(s.limit, Some(5));
    }

    #[test]
    fn aliases_and_qualified_names() {
        let s = extract("SELECT u.email AS contact, o.total FROM users u JOIN orders o ON o.user_id = u.id");
        assert_eq!(names(&s), ["contact", "total"]);
        assert_eq!(s.table_hint.as_deref(), Some("users"));
    }

    #[test]
    fn aggregates() {
        let s = extract("select count(*) from events");
        assert_eq!(names(&s), ["count"]);
        assert!(s.columns[0].aggregate);
    }

    #[test]
    fn literals_echo() {
        let s = extract("SELECT 1");
        assert_eq!(s.columns[0].literal, Some(("1".into(), WireType::Int4)));
        let s = extract("SELECT 'hello'");
        assert_eq!(s.columns[0].literal, Some(("hello".into(), WireType::Text)));
    }

    #[test]
    fn casts() {
        let s = extract("select amount::numeric, id::text from payments");
        assert_eq!(s.columns[0].cast, Some(WireType::Numeric));
        assert_eq!(s.columns[1].cast, Some(WireType::Text));
    }

    #[test]
    fn dml_kinds() {
        assert_eq!(extract("INSERT INTO t (a) VALUES (1)").kind, StmtKind::Insert);
        assert_eq!(extract("update t set a = 1").kind, StmtKind::Update);
        assert_eq!(extract("DELETE FROM t WHERE id = 3").kind, StmtKind::Delete);
    }

    #[test]
    fn commands_get_tags() {
        assert_eq!(extract("BEGIN").kind, StmtKind::Command("BEGIN".into()));
        assert_eq!(
            extract("CREATE TABLE foo (id int)").kind,
            StmtKind::Command("CREATE TABLE".into())
        );
    }

    #[test]
    fn with_cte_finds_main_select() {
        let s = extract("WITH x AS (SELECT 1 AS n FROM a) SELECT id, total FROM x");
        assert_eq!(names(&s), ["id", "total"]);
    }

    #[test]
    fn function_columns_in_select_list_dont_confuse_scanner() {
        let s = extract("select lower(email), count(*) as n from users where city = 'Oslo; drop'");
        assert_eq!(names(&s), ["lower", "n"]);
    }

    #[test]
    fn statement_splitting() {
        let stmts = split_statements("select 1; select 2; insert into t values (';')");
        assert_eq!(stmts.len(), 3);
    }

    #[test]
    fn show_returns_value() {
        let s = extract("SHOW server_version");
        assert_eq!(s.columns[0].name, "server_version");
        assert!(s.columns[0].literal.as_ref().unwrap().0.contains("EtherealDB"));
    }
}
