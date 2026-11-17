//! Text from uploaded files (06 §2.5, 04 §7): plain text / code / Markdown as UTF-8, and
//! PDFs through a small built-in text extractor (03 §4 step 9: "PDF → text fallback").
//!
//! The PDF extractor reads uncompressed and `FlateDecode` content streams and the text
//! operators `Tj`, `TJ`, `'`, `"`, `T*`, `Td`/`TD`/`Tm` (line breaks) inside `BT … ET`.
//! Literal and hex strings are decoded as Latin-1 (UTF-16BE with a BOM). Fonts with custom
//! CMaps (most CID fonts) come out garbled or empty; that is a known gap (README).
//! Pages are separated by a form feed (`\x0c`), which the chunker uses as page breaks.

use std::io::Read;

/// Page separator in extracted PDF text.
pub const PAGE_BREAK: char = '\x0c';

/// Cap on inflated stream size (zip-bomb guard).
const MAX_STREAM: u64 = 64 * 1024 * 1024;

const TEXT_MIMES: &[&str] = &[
    "application/json",
    "application/xml",
    "application/javascript",
    "application/typescript",
    "application/x-yaml",
    "application/yaml",
    "application/toml",
    "application/x-sh",
    "application/sql",
    "application/x-python",
    "image/svg+xml",
];

const TEXT_EXTS: &[&str] = &[
    "txt", "md", "markdown", "rst", "csv", "tsv", "json", "jsonl", "yaml", "yml", "toml", "xml",
    "html", "htm", "css", "js", "jsx", "ts", "tsx", "mjs", "cjs", "py", "rb", "rs", "go", "java",
    "kt", "swift", "c", "h", "cc", "cpp", "hpp", "cs", "php", "sh", "bash", "zsh", "ps1", "sql",
    "ini", "cfg", "conf", "log", "tex", "svg", "lua", "r", "scala", "dart", "vue", "svelte",
];

const CODE_EXTS: &[&str] = &[
    "js", "jsx", "ts", "tsx", "mjs", "cjs", "py", "rb", "rs", "go", "java", "kt", "swift", "c",
    "h", "cc", "cpp", "hpp", "cs", "php", "sh", "bash", "zsh", "ps1", "sql", "lua", "r", "scala",
    "dart", "vue", "svelte",
];

fn ext(name: &str) -> String {
    name.rsplit_once('.')
        .map(|(_, e)| e.to_ascii_lowercase())
        .unwrap_or_default()
}

/// What kind of text a file holds (drives chunking, 04 §7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextKind {
    Markdown,
    Code,
    Pdf,
    Plain,
}

pub fn is_pdf(mime: &str, name: &str) -> bool {
    mime == "application/pdf" || ext(name) == "pdf"
}

pub fn is_image(mime: &str) -> bool {
    mime.starts_with("image/") && mime != "image/svg+xml"
}

/// Whether the bytes are readable as text (by MIME, extension, or content sniffing).
pub fn is_text(mime: &str, name: &str, bytes: &[u8]) -> bool {
    if mime.starts_with("text/") || TEXT_MIMES.contains(&mime) {
        return true;
    }
    if TEXT_EXTS.contains(&ext(name).as_str()) {
        return true;
    }
    if is_image(mime) || is_pdf(mime, name) {
        return false;
    }
    let head = &bytes[..bytes.len().min(8192)];
    !head.contains(&0) && std::str::from_utf8(head).is_ok()
}

pub fn text_kind(mime: &str, name: &str) -> TextKind {
    let e = ext(name);
    if is_pdf(mime, name) {
        TextKind::Pdf
    } else if mime == "text/markdown" || e == "md" || e == "markdown" {
        TextKind::Markdown
    } else if CODE_EXTS.contains(&e.as_str()) {
        TextKind::Code
    } else {
        TextKind::Plain
    }
}

/// Best-effort MIME type from a file name (uploads may not carry one).
pub fn guess_mime(name: &str) -> &'static str {
    match ext(name).as_str() {
        "pdf" => "application/pdf",
        "md" | "markdown" => "text/markdown",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "json" => "application/json",
        "csv" => "text/csv",
        "html" | "htm" => "text/html",
        "svg" => "image/svg+xml",
        e if TEXT_EXTS.contains(&e) => "text/plain",
        _ => "application/octet-stream",
    }
}

/// The text of a file, or why it has none.
pub fn extract_text(bytes: &[u8], mime: &str, name: &str) -> Result<String, String> {
    if is_pdf(mime, name) {
        return pdf_text(bytes).ok_or_else(|| format!("no text could be extracted from {name}"));
    }
    if is_text(mime, name, bytes) {
        return Ok(String::from_utf8_lossy(bytes).into_owned());
    }
    Err(format!(
        "{name} ({mime}) is not a text document; upload text, Markdown, code or PDF files"
    ))
}

// ------------------------------------------------------------------ PDF

fn find(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if from >= hay.len() {
        return None;
    }
    hay[from..]
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|p| p + from)
}

fn rfind(hay: &[u8], needle: &[u8], before: usize) -> Option<usize> {
    let end = before.min(hay.len());
    hay[..end].windows(needle.len()).rposition(|w| w == needle)
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

/// Content streams of a PDF, inflated where needed, in file order.
fn streams(pdf: &[u8]) -> Vec<Vec<u8>> {
    let mut out = vec![];
    let mut pos = 0;
    while let Some(s) = find(pdf, b"stream", pos) {
        // `endstream` also contains `stream`.
        if s >= 3 && &pdf[s - 3..s] == b"end" {
            pos = s + 6;
            continue;
        }
        let mut start = s + 6;
        if pdf.get(start) == Some(&b'\r') {
            start += 1;
        }
        if pdf.get(start) == Some(&b'\n') {
            start += 1;
        }
        let Some(end) = find(pdf, b"endstream", start) else {
            break;
        };
        let dict_start = rfind(pdf, b"<<", s).unwrap_or(s);
        let dict = &pdf[dict_start..s];
        pos = end + 9;
        if contains(dict, b"/Image")
            || contains(dict, b"/Length1")
            || contains(dict, b"/FontFile")
            || contains(dict, b"/ObjStm")
            || contains(dict, b"/XRef")
        {
            continue;
        }
        let raw = &pdf[start..end];
        let data = if contains(dict, b"/FlateDecode") {
            let mut d = flate2::read::ZlibDecoder::new(raw).take(MAX_STREAM);
            let mut v = vec![];
            if d.read_to_end(&mut v).is_err() && v.is_empty() {
                continue;
            }
            v
        } else if contains(dict, b"/Filter") {
            continue; // DCT, LZW, ASCII85 … are not text streams we can read.
        } else {
            raw.to_vec()
        };
        out.push(data);
    }
    out
}

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Str(Vec<u8>),
    Num(f64),
    Op(String),
    ArrOpen,
    ArrClose,
    Other,
}

fn decode_bytes(b: &[u8]) -> String {
    if b.len() >= 2 && b[0] == 0xFE && b[1] == 0xFF {
        let units: Vec<u16> = b[2..]
            .chunks(2)
            .map(|c| u16::from_be_bytes([c[0], *c.get(1).unwrap_or(&0)]))
            .collect();
        return String::from_utf16_lossy(&units);
    }
    b.iter().map(|&c| char::from(c)).collect()
}

fn tokens(s: &[u8]) -> Vec<Tok> {
    let mut out = vec![];
    let mut i = 0;
    while i < s.len() {
        let c = s[i];
        match c {
            b' ' | b'\n' | b'\r' | b'\t' | b'\x0c' | 0 => i += 1,
            b'%' => {
                while i < s.len() && s[i] != b'\n' && s[i] != b'\r' {
                    i += 1;
                }
            }
            b'(' => {
                let mut depth = 1;
                let mut buf = vec![];
                i += 1;
                while i < s.len() && depth > 0 {
                    let c = s[i];
                    match c {
                        b'\\' if i + 1 < s.len() => {
                            i += 1;
                            let e = s[i];
                            match e {
                                b'n' => buf.push(b'\n'),
                                b'r' => buf.push(b'\r'),
                                b't' => buf.push(b'\t'),
                                b'b' | b'f' => {}
                                b'0'..=b'7' => {
                                    let mut v: u32 = 0;
                                    let mut k = 0;
                                    while k < 3 && i < s.len() && (b'0'..=b'7').contains(&s[i]) {
                                        v = v * 8 + u32::from(s[i] - b'0');
                                        i += 1;
                                        k += 1;
                                    }
                                    buf.push(u8::try_from(v & 0xff).unwrap_or(0));
                                    continue;
                                }
                                b'\r' | b'\n' => {}
                                other => buf.push(other),
                            }
                        }
                        b'(' => {
                            depth += 1;
                            buf.push(c);
                        }
                        b')' => {
                            depth -= 1;
                            if depth > 0 {
                                buf.push(c);
                            }
                        }
                        _ => buf.push(c),
                    }
                    i += 1;
                }
                out.push(Tok::Str(buf));
            }
            b'<' if s.get(i + 1) == Some(&b'<') => {
                i += 2;
                out.push(Tok::Other);
            }
            b'>' if s.get(i + 1) == Some(&b'>') => {
                i += 2;
                out.push(Tok::Other);
            }
            b'<' => {
                let end = find(s, b">", i).unwrap_or(s.len());
                let hex: Vec<u8> = s[i + 1..end]
                    .iter()
                    .copied()
                    .filter(u8::is_ascii_hexdigit)
                    .collect();
                let mut bytes = vec![];
                for pair in hex.chunks(2) {
                    let hi = char::from(pair[0]).to_digit(16).unwrap_or(0);
                    let lo = pair
                        .get(1)
                        .and_then(|c| char::from(*c).to_digit(16))
                        .unwrap_or(0);
                    bytes.push(u8::try_from(hi * 16 + lo).unwrap_or(0));
                }
                out.push(Tok::Str(bytes));
                i = end + 1;
            }
            b'[' => {
                out.push(Tok::ArrOpen);
                i += 1;
            }
            b']' => {
                out.push(Tok::ArrClose);
                i += 1;
            }
            b'/' => {
                i += 1;
                while i < s.len() && !b" \n\r\t/[]()<>{}%".contains(&s[i]) {
                    i += 1;
                }
                out.push(Tok::Other);
            }
            b'0'..=b'9' | b'-' | b'+' | b'.' => {
                let st = i;
                i += 1;
                while i < s.len() && (s[i].is_ascii_digit() || s[i] == b'.') {
                    i += 1;
                }
                let n = std::str::from_utf8(&s[st..i])
                    .ok()
                    .and_then(|t| t.parse::<f64>().ok())
                    .unwrap_or(0.0);
                out.push(Tok::Num(n));
            }
            _ => {
                let st = i;
                while i < s.len() && !b" \n\r\t/[]()<>{}%".contains(&s[i]) {
                    i += 1;
                }
                if i == st {
                    i += 1;
                    out.push(Tok::Other);
                } else {
                    out.push(Tok::Op(String::from_utf8_lossy(&s[st..i]).into_owned()));
                }
            }
        }
    }
    out
}

fn newline(out: &mut String) {
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
}

/// Text of one content stream.
fn stream_text(data: &[u8]) -> String {
    let toks = tokens(data);
    let mut out = String::new();
    let mut operands: Vec<Tok> = vec![];
    let mut in_text = false;
    for t in toks {
        let Tok::Op(op) = &t else {
            operands.push(t);
            continue;
        };
        match op.as_str() {
            "BT" => in_text = true,
            "ET" => {
                in_text = false;
                newline(&mut out);
            }
            "Tj" | "'" | "\"" if in_text => {
                if op != "Tj" {
                    newline(&mut out);
                }
                if let Some(Tok::Str(b)) = operands.iter().rev().find(|t| matches!(t, Tok::Str(_)))
                {
                    out.push_str(&decode_bytes(b));
                }
            }
            "TJ" if in_text => {
                for o in &operands {
                    match o {
                        Tok::Str(b) => out.push_str(&decode_bytes(b)),
                        Tok::Num(n) if *n < -200.0 && !out.ends_with(' ') => out.push(' '),
                        _ => {}
                    }
                }
            }
            "T*" if in_text => newline(&mut out),
            "Td" | "TD" if in_text => {
                let ty = match operands.as_slice() {
                    [.., Tok::Num(_), Tok::Num(y)] => *y,
                    _ => 0.0,
                };
                if ty.abs() > 0.01 {
                    newline(&mut out);
                } else if !out.ends_with(' ') && !out.ends_with('\n') && !out.is_empty() {
                    out.push(' ');
                }
            }
            "Tm" if in_text => newline(&mut out),
            _ => {}
        }
        operands.clear();
    }
    out
}

/// Extracted text, pages separated by [`PAGE_BREAK`]; `None` when nothing readable.
pub fn pdf_text(pdf: &[u8]) -> Option<String> {
    if !pdf.starts_with(b"%PDF") {
        return None;
    }
    let pages: Vec<String> = streams(pdf)
        .iter()
        .map(|s| stream_text(s))
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .collect();
    if pages.is_empty() {
        return None;
    }
    Some(pages.join(&format!("\n{PAGE_BREAK}\n")))
}

#[cfg(test)]
pub(crate) mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use std::io::Write;

    /// A minimal two-page PDF: one plain and one Flate-compressed content stream.
    pub(crate) fn sample_pdf(page1: &str, page2: &str) -> Vec<u8> {
        let s1 =
            format!("BT /F1 12 Tf 72 712 Td ({page1}) Tj 0 -14 Td [(Second) -300 (line)] TJ ET");
        let s2 = format!("BT /F1 12 Tf 72 712 Td ({page2}) Tj T* (end\\051) Tj ET");
        let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        z.write_all(s2.as_bytes()).unwrap();
        let c2 = z.finish().unwrap();
        let mut pdf = b"%PDF-1.4\n1 0 obj << /Type /Catalog >> endobj\n".to_vec();
        pdf.extend_from_slice(
            format!(
                "4 0 obj << /Length {} >>\nstream\n{s1}\nendstream endobj\n",
                s1.len()
            )
            .as_bytes(),
        );
        pdf.extend_from_slice(
            format!(
                "5 0 obj << /Length {} /Filter /FlateDecode >>\nstream\n",
                c2.len()
            )
            .as_bytes(),
        );
        pdf.extend_from_slice(&c2);
        pdf.extend_from_slice(b"\nendstream endobj\n");
        pdf.extend_from_slice(
            b"6 0 obj << /Subtype /Image /Length 3 >>\nstream\nabc\nendstream endobj\n%%EOF\n",
        );
        pdf
    }

    #[test]
    fn pdf_text_reads_plain_and_flate_streams() {
        let pdf = sample_pdf("Hello (nested) PDF", "Page two text");
        let t = pdf_text(&pdf).unwrap();
        let pages: Vec<&str> = t.split(PAGE_BREAK).map(str::trim).collect();
        assert_eq!(pages.len(), 2, "{t:?}");
        assert_eq!(pages[0], "Hello (nested) PDF\nSecond line");
        assert_eq!(pages[1], "Page two text\nend)");
        assert_eq!(pdf_text(b"not a pdf"), None);
        assert!(extract_text(&pdf, "application/pdf", "a.pdf").is_ok());
    }

    #[test]
    fn text_detection() {
        assert!(is_text("text/plain", "a", b"x"));
        assert!(is_text(
            "application/octet-stream",
            "main.rs",
            b"fn main(){}"
        ));
        assert!(is_text("", "notes", b"plain words"));
        assert!(!is_text("", "blob", b"\0\x01\x02"));
        assert!(!is_text("image/png", "a.png", b"\x89PNG"));
        assert_eq!(text_kind("", "README.md"), TextKind::Markdown);
        assert_eq!(text_kind("", "lib.rs"), TextKind::Code);
        assert_eq!(text_kind("application/pdf", "x"), TextKind::Pdf);
        assert_eq!(guess_mime("a.PDF"), "application/pdf");
        assert!(extract_text(b"\0\x01", "application/zip", "a.zip").is_err());
    }
}
