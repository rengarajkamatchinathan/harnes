//! Attachments in prompts (06 §2.5).
//!
//! - **On admission** ([`Attachments::input_parts`]): an `attachment` input part becomes
//!   - an image → a media part referencing the blob,
//!   - a PDF → a media part referencing the blob, with the extracted text kept as the
//!     fallback for models without native PDF input (03 §4 step 9),
//!   - a text-like file → `<attachment name="…">…</attachment>` text (≤ 256 KiB),
//!   - anything else → a note that the file type can't be read.
//! - **At assembly** ([`Attachments::prepare`]): blob media are resolved to base64 for the
//!   request; images for a model without vision are replaced by a clear note, PDFs for a
//!   model without PDF input by their text; the **image budget** (KB ch.04) replaces the
//!   oldest inline images by a note once they exceed 25 MiB, cutting down to ≤ 15 MiB in
//!   10 MiB steps so the cached prefix only changes when a cut moves.
//!
//! Media count against the context meter through the normal estimate (an image ~1,600
//! tokens, a PDF its extracted text).

use std::collections::HashMap;
use std::sync::Arc;

use base64::Engine as _;
use genie_protocol::message::InputPart;
use genie_providers::compat::ModelSpec;
use genie_providers::model::{BlobId, MediaPart, MediaSource, Message, Part};

use crate::runtime::extract;
use crate::runtime::wire;

/// Text attachments are inlined up to this size.
pub const TEXT_CAP: usize = 256 * 1024;
/// Image budget (06 §2.5): trigger, target and step.
pub const IMAGE_BUDGET: u64 = 25 * 1024 * 1024;
pub const IMAGE_TARGET: u64 = 15 * 1024 * 1024;
pub const IMAGE_STEP: u64 = 10 * 1024 * 1024;

pub const IMAGE_REMOVED: &str =
    "[An image was removed to save space; ask the user to re-attach it if needed.]";

fn attr(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
}

/// How many of the oldest images (sizes oldest first) the image budget removes: none
/// until the total exceeds 25 MiB, then enough to get under 15 MiB, with the cut rounded
/// up to a 10 MiB step so it stays put while new images arrive.
pub fn images_to_remove(sizes: &[u64]) -> usize {
    let total: u64 = sizes.iter().sum();
    if total <= IMAGE_BUDGET {
        return 0;
    }
    let cut = (total - IMAGE_TARGET).div_ceil(IMAGE_STEP) * IMAGE_STEP;
    let mut gone = 0u64;
    let mut n = 0;
    for s in sizes {
        if gone >= cut {
            break;
        }
        gone += s;
        n += 1;
    }
    n
}

pub struct Attachments {
    store: Arc<genie_store::Store>,
}

impl Attachments {
    pub fn new(store: Arc<genie_store::Store>) -> Arc<Self> {
        Arc::new(Attachments { store })
    }

    /// Canonical parts for a user message.
    pub fn input_parts(&self, parts: &[InputPart]) -> Vec<Part> {
        parts
            .iter()
            .map(|p| match p {
                InputPart::Attachment {
                    blob_id,
                    name,
                    mime,
                } => self.attachment(blob_id, name, mime),
                other => wire::input_parts(std::slice::from_ref(other))
                    .into_iter()
                    .next()
                    .unwrap_or_else(|| Part::text("")),
            })
            .collect()
    }

    fn attachment(&self, blob_id: &str, name: &str, mime: &str) -> Part {
        let mime = if mime.is_empty() || mime == "application/octet-stream" {
            extract::guess_mime(name).to_string()
        } else {
            mime.to_string()
        };
        if extract::is_image(&mime) {
            if self.store.blob_row(blob_id).ok().flatten().is_none() {
                return Part::text(format!("[The attachment {name} is no longer available.]"));
            }
            return Part::Media(MediaPart {
                mime,
                source: MediaSource::Blob {
                    id: BlobId::new(blob_id),
                },
                filename: Some(name.to_string()),
                extracted_text: None,
                cache: None,
            });
        }
        let bytes = match self.store.read_blob(blob_id) {
            Ok(b) => b,
            Err(_) => {
                return Part::text(format!("[The attachment {name} is no longer available.]"));
            }
        };
        if extract::is_pdf(&mime, name) {
            return Part::Media(MediaPart {
                mime: "application/pdf".into(),
                source: MediaSource::Blob {
                    id: BlobId::new(blob_id),
                },
                filename: Some(name.to_string()),
                extracted_text: extract::pdf_text(&bytes),
                cache: None,
            });
        }
        if extract::is_text(&mime, name, &bytes) {
            let mut text = String::from_utf8_lossy(&bytes).into_owned();
            let mut note = String::new();
            if text.len() > TEXT_CAP {
                let mut end = TEXT_CAP;
                while !text.is_char_boundary(end) {
                    end -= 1;
                }
                text.truncate(end);
                note = format!(
                    "\n[Truncated: showing the first {TEXT_CAP} bytes of {} bytes.]",
                    bytes.len()
                );
            }
            return Part::text(format!(
                "<attachment name=\"{}\" mime=\"{}\">\n{}{note}\n</attachment>",
                attr(name),
                attr(&mime),
                text.trim_end()
            ));
        }
        Part::text(format!(
            "[The user attached {name} ({mime}); this file type can't be read. Ask for a text, \
             PDF or image version if its content matters.]"
        ))
    }

    fn blob_size(&self, id: &str, sizes: &mut HashMap<String, u64>) -> u64 {
        *sizes
            .entry(id.to_string())
            .or_insert_with(|| self.store.blob_row(id).ok().flatten().map_or(0, |r| r.size))
    }

    /// Resolve media for the request (see the module docs).
    pub fn prepare(&self, mut messages: Vec<Message>, model: &ModelSpec) -> Vec<Message> {
        // Image budget: total inline image bytes, oldest first.
        let mut sizes = HashMap::new();
        let mut images: Vec<(usize, usize, u64)> = vec![];
        for (mi, m) in messages.iter().enumerate() {
            for (pi, p) in m.parts.iter().enumerate() {
                if let Part::Media(media) = p
                    && media.is_image()
                    && let MediaSource::Blob { id } = &media.source
                {
                    images.push((mi, pi, self.blob_size(id.as_str(), &mut sizes)));
                }
            }
        }
        let removed: Vec<(usize, usize)> = if model.input.image {
            let sizes: Vec<u64> = images.iter().map(|i| i.2).collect();
            images
                .iter()
                .take(images_to_remove(&sizes))
                .map(|(m, p, _)| (*m, *p))
                .collect()
        } else {
            vec![]
        };
        for (mi, m) in messages.iter_mut().enumerate() {
            for (pi, p) in m.parts.iter_mut().enumerate() {
                let Part::Media(media) = p else { continue };
                let MediaSource::Blob { id } = &media.source else {
                    continue;
                };
                let name = media
                    .filename
                    .clone()
                    .unwrap_or_else(|| "attachment".into());
                if media.is_image() {
                    if !model.input.image {
                        *p = Part::text(format!(
                            "[The user attached the image {name}, but {} can't read images. \
                             Tell the user to pick a model with vision to discuss it.]",
                            model.wire_id
                        ));
                        continue;
                    }
                    if removed.contains(&(mi, pi)) {
                        *p = Part::text(IMAGE_REMOVED);
                        continue;
                    }
                } else if media.is_pdf() && !model.input.pdf {
                    *p = Part::text(match &media.extracted_text {
                        Some(t) => {
                            format!("<attachment name=\"{}\">\n{t}\n</attachment>", attr(&name))
                        }
                        None => format!(
                            "[The user attached {name}, but no text could be extracted from it \
                             and {} can't read PDFs directly.]",
                            model.wire_id
                        ),
                    });
                    continue;
                }
                match self.store.read_blob(id.as_str()) {
                    Ok(bytes) => {
                        media.source = MediaSource::Base64 {
                            data: base64::engine::general_purpose::STANDARD.encode(bytes),
                        };
                    }
                    Err(_) => {
                        *p = Part::text(format!("[The attachment {name} is no longer available.]"));
                    }
                }
            }
        }
        messages
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_budget_cuts_in_steps() {
        const M: u64 = 1024 * 1024;
        assert_eq!(images_to_remove(&[5 * M; 5]), 0, "25 MiB is within budget");
        // 26 MiB → cut 20 MiB (11 rounded up to the 10 MiB step) → 4 images go.
        assert_eq!(images_to_remove(&[5 * M, 5 * M, 5 * M, 5 * M, 6 * M]), 4);
        // A small new image doesn't move the cut.
        assert_eq!(images_to_remove(&[5 * M, 5 * M, 5 * M, 5 * M, 6 * M, M]), 4);
        assert_eq!(images_to_remove(&[]), 0);
    }
}
