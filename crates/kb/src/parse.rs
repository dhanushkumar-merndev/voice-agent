//! Text extraction from uploaded documents.

use std::sync::LazyLock;

use regex::Regex;

use crate::KbError;

/// Supported document kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocKind {
    /// Plain text.
    Text,
    /// Markdown (syntax is stripped).
    Markdown,
    /// PDF (text layer only; scanned PDFs need OCR first).
    Pdf,
}

/// Picks the document kind from the filename extension.
pub fn kind_for(filename: &str) -> Result<DocKind, KbError> {
    let ext = filename
        .rsplit_once('.')
        .map(|(_, e)| e.to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "txt" | "text" => Ok(DocKind::Text),
        "md" | "markdown" => Ok(DocKind::Markdown),
        "pdf" => Ok(DocKind::Pdf),
        _ => Err(KbError::Unsupported(filename.to_owned())),
    }
}

/// Extracts plain text.
pub fn extract_text(kind: DocKind, bytes: &[u8]) -> Result<String, KbError> {
    let text = match kind {
        DocKind::Text => String::from_utf8_lossy(bytes).into_owned(),
        DocKind::Markdown => strip_markdown(&String::from_utf8_lossy(bytes)),
        DocKind::Pdf => {
            pdf_extract::extract_text_from_mem(bytes).map_err(|e| KbError::Parse(e.to_string()))?
        }
    };
    let text = text.replace('\r', "");
    if text.trim().is_empty() {
        return Err(KbError::Empty);
    }
    Ok(text)
}

static MD_LINK: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"!?\[([^\]]*)\]\([^)]*\)").ok());
static MD_MARKS: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"(?m)^\s{0,3}(#{1,6}\s+|>\s?|[-*+]\s+)|[*_`~]{1,3}").ok());

/// Removes common markdown syntax, keeping link text.
pub fn strip_markdown(md: &str) -> String {
    let mut out = md.to_owned();
    if let Some(re) = MD_LINK.as_ref() {
        out = re.replace_all(&out, "$1").into_owned();
    }
    if let Some(re) = MD_MARKS.as_ref() {
        out = re.replace_all(&out, "").into_owned();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_by_extension() {
        assert_eq!(kind_for("a.TXT").unwrap(), DocKind::Text);
        assert_eq!(kind_for("notes.md").unwrap(), DocKind::Markdown);
        assert_eq!(kind_for("x.pdf").unwrap(), DocKind::Pdf);
        assert!(kind_for("x.docx").is_err());
    }

    #[test]
    fn markdown_is_stripped() {
        let md = "# Pricing\n- **Ads**: from ₹10,000 [details](https://x.y)\n> note";
        let text = extract_text(DocKind::Markdown, md.as_bytes()).unwrap();
        assert_eq!(text, "Pricing\nAds: from ₹10,000 details\nnote");
    }

    #[test]
    fn empty_is_rejected() {
        assert!(matches!(
            extract_text(DocKind::Text, b"  \n "),
            Err(KbError::Empty)
        ));
    }
}
