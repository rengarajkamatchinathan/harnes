//! Ids, clocks and integer conversions shared by the store modules.

use std::time::{SystemTime, UNIX_EPOCH};

use crate::error::{Result, StoreError};

/// ULID with a type prefix (02 §6): `ses_…`, `msg_…`, `prt_…`.
pub fn new_id(prefix: &str) -> String {
    format!("{prefix}_{}", ulid::Ulid::new().to_string().to_lowercase())
}

/// Milliseconds since the Unix epoch.
pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

/// SQLite integers are i64; seqs and sizes are u64 in the API.
pub(crate) fn to_i64(v: u64) -> Result<i64> {
    i64::try_from(v).map_err(|_| StoreError::Invalid(format!("{v} does not fit in i64")))
}

pub(crate) fn to_u64(v: i64) -> u64 {
    u64::try_from(v).unwrap_or(0)
}

/// Build an FTS5 query from free text: each word quoted (so user input can't inject
/// FTS syntax), all words required, the last one as a prefix.
pub fn fts_query(input: &str) -> Option<String> {
    let words: Vec<String> = input
        .split_whitespace()
        .map(|w| format!("\"{}\"", w.replace('"', "\"\"")))
        .collect();
    let (last, rest) = words.split_last()?;
    let mut q = rest.join(" ");
    if !q.is_empty() {
        q.push(' ');
    }
    q.push_str(last);
    q.push('*');
    Some(q)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_have_prefix() {
        let id = new_id("ses");
        assert!(id.starts_with("ses_"));
        assert_eq!(id.len(), 4 + 26);
    }

    #[test]
    fn fts_query_quotes_words() {
        assert_eq!(fts_query("  "), None);
        assert_eq!(fts_query("hello"), Some("\"hello\"*".into()));
        assert_eq!(
            fts_query("a \"b OR c"),
            Some("\"a\" \"\"\"b\" \"OR\" \"c\"*".into())
        );
    }
}
