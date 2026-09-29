use std::panic::{AssertUnwindSafe, catch_unwind};

use anyhow::{Result, anyhow};

/// Convert PDF bytes to Markdown text via the in-tree `pdf-extract` layer
/// (same minor as `memvid-core`'s pin — zero new version islands).
///
/// # Parameters
/// - `bytes` — the raw PDF file bytes (any read source: host staging,
///   ingest drop, URL cache).
///
/// # Returns
/// The extracted plain text. Empty output (scanned/image-only PDFs) is
/// returned as `Ok("")` — the caller decides quarantine, this function only
/// reports what the extractor saw.
///
/// # Errors
/// - Non-PDF input (missing `%PDF` header) — rejected before the parser so
///   obviously-wrong bytes get a clean message instead of a parser panic.
/// - Parser failure (encrypted/corrupt PDF).
/// - **Panic containment**: `pdf-extract` has a known panic-on-malformed-input
///   history (upstream #133/#154/#160, actively fixed); the call runs inside
///   `catch_unwind` so a parser panic becomes a normal `Err` and a hostile
///   document can never take down the distill worker.
///
/// # Examples
/// ```
/// let md = zen_vault::ingest::convert::pdf_to_markdown(&pdf_bytes)?;
/// ```
pub fn pdf_to_markdown(bytes: &[u8]) -> Result<String> {
    if !bytes.starts_with(b"%PDF") {
        return Err(anyhow!("not a PDF file (missing %PDF header)"));
    }
    let extracted = catch_unwind(AssertUnwindSafe(|| {
        pdf_extract::extract_text_from_mem(bytes)
    }))
    .map_err(|_| anyhow!("pdf extraction panicked on malformed input (contained)"))?
    .map_err(|e| anyhow!("pdf text extraction failed: {e}"))?;
    Ok(extracted)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Assemble a minimal one-page PDF with correct xref offsets so the
    /// fixture needs no binary blob in the repo and stays diffable.
    fn tiny_pdf(text: &str) -> Vec<u8> {
        let stream = format!("BT /F1 12 Tf 72 720 Td ({text}) Tj ET");
        let objects = [
            "%PDF-1.4\n".to_string(),
            "1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n".to_string(),
            "2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n".to_string(),
            "3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>\nendobj\n".to_string(),
            format!(
                "4 0 obj\n<< /Length {} >>\nstream\n{stream}\nendstream\nendobj\n",
                stream.len()
            ),
            "5 0 obj\n<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>\nendobj\n".to_string(),
        ];
        let mut body = Vec::new();
        let mut offsets = Vec::new();
        for obj in &objects {
            offsets.push(body.len());
            body.extend_from_slice(obj.as_bytes());
        }
        let xref_offset = body.len();
        body.extend_from_slice(format!("xref\n0 {}\n", objects.len() + 1).as_bytes());
        body.extend_from_slice(b"0000000000 65535 f \n");
        for off in &offsets {
            body.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
        }
        body.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n",
                objects.len() + 1
            )
            .as_bytes(),
        );
        body
    }

    #[test]
    fn extracts_text_from_minimal_pdf() {
        let pdf = tiny_pdf("Hello Zen");
        let text = pdf_to_markdown(&pdf).expect("valid pdf must extract");
        assert!(text.contains("Hello"), "got: {text:?}");
        assert!(text.contains("Zen"), "got: {text:?}");
    }

    #[test]
    fn textless_pdf_extracts_empty_not_error() {
        let stream = String::new();
        let pdf = tiny_pdf(&stream);
        let text = pdf_to_markdown(&pdf).expect("valid pdf must parse");
        assert!(text.trim().is_empty(), "got: {text:?}");
    }

    #[test]
    fn garbage_bytes_fail_without_panic() {
        let err = pdf_to_markdown(b"definitely not a pdf at all")
            .expect_err("non-pdf header must be rejected");
        assert!(err.to_string().contains("%PDF"));
    }

    #[test]
    fn truncated_pdf_fails_cleanly() {
        let mut pdf = tiny_pdf("Hello Zen");
        pdf.truncate(pdf.len() / 2);
        let result = pdf_to_markdown(&pdf);
        // Either the header check or the parser rejects it — the only
        // contract is Err, never a panic escaping to the caller.
        assert!(result.is_err());
    }
}
