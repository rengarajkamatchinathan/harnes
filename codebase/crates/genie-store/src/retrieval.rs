//! Retrieval index storage and ranking (04 §7, D32).
//!
//! Covered here: `kchunk` rows (+ `kchunk_fts`), the embedding cache by content hash,
//! BM25 search, brute-force cosine (SA-15) and reciprocal rank fusion. Chunking, the
//! indexing job, the full/retrieval mode switch and the capacity cap are not
//! implemented yet (README gaps).

use std::collections::HashMap;

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::{Result, StoreError};
use crate::util::{fts_query, new_id, now_ms};

/// RRF constant (the usual k = 60).
pub const RRF_K: f64 = 60.0;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Chunk {
    pub id: String,
    pub project_id: String,
    pub file_id: String,
    pub ordinal: i64,
    pub start_byte: i64,
    pub end_byte: i64,
    pub tokens: i64,
    pub text: String,
    pub content_hash: Vec<u8>,
}

/// A chunk to insert; the id and content hash are computed.
#[derive(Debug, Clone)]
pub struct NewChunk<'a> {
    pub ordinal: i64,
    pub start_byte: i64,
    pub end_byte: i64,
    pub tokens: i64,
    pub text: &'a str,
}

/// Replace a file's chunks.
pub fn replace_file_chunks(
    conn: &Connection,
    project_id: &str,
    file_id: &str,
    chunks: &[NewChunk<'_>],
) -> Result<Vec<String>> {
    conn.execute("DELETE FROM kchunk WHERE file_id = ?1", [file_id])?;
    let mut st = conn.prepare(
        "INSERT INTO kchunk (id, project_id, file_id, ordinal, start_byte, end_byte, tokens, text,
                             content_hash)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
    )?;
    let mut ids = vec![];
    for c in chunks {
        let id = new_id("kch");
        let hash = Sha256::digest(c.text.as_bytes()).to_vec();
        st.execute(params![
            id,
            project_id,
            file_id,
            c.ordinal,
            c.start_byte,
            c.end_byte,
            c.tokens,
            c.text,
            hash
        ])?;
        ids.push(id);
    }
    Ok(ids)
}

pub fn chunk(conn: &Connection, id: &str) -> Result<Option<Chunk>> {
    Ok(conn
        .query_row(
            "SELECT id, project_id, file_id, ordinal, start_byte, end_byte, tokens, text, content_hash
             FROM kchunk WHERE id = ?1",
            [id],
            |r| {
                Ok(Chunk {
                    id: r.get(0)?,
                    project_id: r.get(1)?,
                    file_id: r.get(2)?,
                    ordinal: r.get(3)?,
                    start_byte: r.get(4)?,
                    end_byte: r.get(5)?,
                    tokens: r.get(6)?,
                    text: r.get(7)?,
                    content_hash: r.get(8)?,
                })
            },
        )
        .optional()?)
}

/// BM25 ranking within a project: chunk ids, best first.
pub fn bm25(conn: &Connection, project_id: &str, query: &str, limit: u32) -> Result<Vec<String>> {
    let Some(q) = fts_query(query) else {
        return Ok(vec![]);
    };
    let mut st = conn.prepare(&format!(
        "SELECT c.id FROM kchunk_fts f JOIN kchunk c ON c.rowid = f.rowid
         WHERE kchunk_fts MATCH ?1 AND c.project_id = ?2
         ORDER BY bm25(kchunk_fts) LIMIT {limit}"
    ))?;
    let rows = st.query_map(params![q, project_id], |r| r.get(0))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

fn encode_vec(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_le_bytes()).collect()
}

fn decode_vec(b: &[u8]) -> Result<Vec<f32>> {
    let (chunks, rest) = b.as_chunks::<4>();
    if !rest.is_empty() {
        return Err(StoreError::Format(
            "embedding length is not a multiple of 4".into(),
        ));
    }
    Ok(chunks.iter().map(|c| f32::from_le_bytes(*c)).collect())
}

/// Cache an embedding by content hash and model (D32).
pub fn put_embedding(
    conn: &Connection,
    content_hash: &[u8],
    model: &str,
    vector: &[f32],
) -> Result<()> {
    let dim = i64::try_from(vector.len()).map_err(|_| StoreError::Invalid("dim".into()))?;
    conn.execute(
        "INSERT OR REPLACE INTO kembedding (content_hash, model, dim, vector, created)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![content_hash, model, dim, encode_vec(vector), now_ms()],
    )?;
    Ok(())
}

pub fn get_embedding(
    conn: &Connection,
    content_hash: &[u8],
    model: &str,
) -> Result<Option<Vec<f32>>> {
    let b: Option<Vec<u8>> = conn
        .query_row(
            "SELECT vector FROM kembedding WHERE content_hash = ?1 AND model = ?2",
            params![content_hash, model],
            |r| r.get(0),
        )
        .optional()?;
    b.map(|b| decode_vec(&b)).transpose()
}

/// Chunks of a project that have no embedding for `model` yet: `(content_hash, text)`.
pub fn missing_embeddings(
    conn: &Connection,
    project_id: &str,
    model: &str,
) -> Result<Vec<(Vec<u8>, String)>> {
    let mut st = conn.prepare(
        "SELECT DISTINCT c.content_hash, c.text FROM kchunk c
         LEFT JOIN kembedding e ON e.content_hash = c.content_hash AND e.model = ?2
         WHERE c.project_id = ?1 AND e.content_hash IS NULL",
    )?;
    let rows = st.query_map(params![project_id, model], |r| Ok((r.get(0)?, r.get(1)?)))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let (mut dot, mut na, mut nb) = (0f32, 0f32, 0f32);
    for (x, y) in a.iter().zip(b) {
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    if na == 0.0 || nb == 0.0 {
        0.0
    } else {
        dot / (na.sqrt() * nb.sqrt())
    }
}

/// Brute-force cosine over the project's vectors (SA-15): chunk ids, best first.
pub fn vector_search(
    conn: &Connection,
    project_id: &str,
    model: &str,
    query: &[f32],
    limit: usize,
) -> Result<Vec<String>> {
    let mut st = conn.prepare(
        "SELECT c.id, e.vector FROM kchunk c
         JOIN kembedding e ON e.content_hash = c.content_hash AND e.model = ?2
         WHERE c.project_id = ?1",
    )?;
    let mut scored: Vec<(f32, String)> = vec![];
    let rows = st.query_map(params![project_id, model], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, Vec<u8>>(1)?))
    })?;
    for row in rows {
        let (id, bytes) = row?;
        scored.push((cosine(query, &decode_vec(&bytes)?), id));
    }
    scored.sort_by(|a, b| b.0.total_cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    Ok(scored.into_iter().take(limit).map(|(_, id)| id).collect())
}

/// Reciprocal rank fusion of several rankings (ids best first).
pub fn rrf(rankings: &[Vec<String>], limit: usize) -> Vec<String> {
    let mut score: HashMap<&str, f64> = HashMap::new();
    for ranking in rankings {
        for (i, id) in ranking.iter().enumerate() {
            *score.entry(id.as_str()).or_default() += 1.0 / (RRF_K + i as f64 + 1.0);
        }
    }
    let mut v: Vec<(&str, f64)> = score.into_iter().collect();
    v.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(b.0)));
    v.into_iter()
        .take(limit)
        .map(|(id, _)| id.to_string())
        .collect()
}

/// Hybrid ranking (D32): BM25 + cosine fused with RRF; BM25 alone without a vector.
pub fn hybrid_search(
    conn: &Connection,
    project_id: &str,
    query: &str,
    query_vector: Option<(&str, &[f32])>,
    limit: usize,
) -> Result<Vec<String>> {
    let depth = u32::try_from(limit.saturating_mul(4).max(20)).unwrap_or(u32::MAX);
    let lexical = bm25(conn, project_id, query, depth)?;
    let Some((model, v)) = query_vector else {
        return Ok(lexical.into_iter().take(limit).collect());
    };
    let semantic = vector_search(conn, project_id, model, v, depth as usize)?;
    Ok(rrf(&[lexical, semantic], limit))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cosine_and_rrf() {
        assert!((cosine(&[1.0, 0.0], &[1.0, 0.0]) - 1.0).abs() < 1e-6);
        assert!(cosine(&[1.0, 0.0], &[0.0, 1.0]).abs() < 1e-6);
        assert_eq!(cosine(&[0.0], &[1.0]), 0.0);
        let a = vec!["x".to_string(), "y".to_string(), "z".to_string()];
        let b = vec!["y".to_string(), "z".to_string()];
        assert_eq!(rrf(&[a, b], 2), vec!["y".to_string(), "z".to_string()]);
        assert_eq!(
            decode_vec(&encode_vec(&[1.5, -2.0])).unwrap(),
            vec![1.5, -2.0]
        );
    }
}
