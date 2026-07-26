//! Media reader for the managed tools service.
//!
//! Supports image optimization for LLM consumption and PDF text extraction
//! with explicit page selection.

use std::path::{Path, PathBuf};

use {
    anyhow::{Context, Result, anyhow, bail},
    chelix_protocol::{ReadMediaRequest, ReadMediaResponse},
    tokio::{
        fs::{self, File},
        io::AsyncReadExt,
    },
    tracing::instrument,
};

const MIME_SNIFF_BYTES: usize = 8 * 1024;
const MAX_PDF_PAGES_PER_REQUEST: usize = 20;

#[instrument(skip_all, fields(path = %request.file_path.trim()))]
pub(crate) async fn run_tool(request: ReadMediaRequest) -> Result<ReadMediaResponse> {
    request.validate().map_err(anyhow::Error::from)?;
    let path = Path::new(request.file_path.trim());
    let pdf_pages = request.pdf.as_ref().and_then(|pdf| pdf.pages.as_deref());
    if !path.is_absolute() {
        bail!("filePath must be absolute.");
    }

    let metadata = fs::metadata(path)
        .await
        .with_context(|| format!("failed to inspect file '{}'", path.display()))?;
    if !metadata.is_file() {
        bail!("Path points to a directory, not a file: {}", path.display());
    }

    let detected_mime = detect_media_mime(path).await?;
    if detected_mime == "application/pdf" {
        return read_pdf(path.to_path_buf(), pdf_pages.map(str::to_owned)).await;
    }
    if detected_mime.starts_with("image/") {
        if pdf_pages.is_some() {
            bail!("pdf.pages is only supported for PDF files.");
        }
        return read_image(path.to_path_buf()).await;
    }

    bail!(
        "read_media supports only PDF and image files; detected MIME type '{}' for '{}'.",
        detected_mime,
        path.display()
    );
}

async fn detect_media_mime(path: &Path) -> Result<String> {
    let mut file = File::open(path)
        .await
        .with_context(|| format!("failed to open file '{}'", path.display()))?;
    let mut buffer = vec![0_u8; MIME_SNIFF_BYTES];
    let bytes_read = file
        .read(&mut buffer)
        .await
        .with_context(|| format!("failed to read file '{}'", path.display()))?;
    buffer.truncate(bytes_read);

    let extension_hint = path
        .extension()
        .and_then(|ext| ext.to_str())
        .and_then(chelix_media::mime::mime_from_extension);
    Ok(chelix_media::mime::detect_mime(&buffer, extension_hint))
}

async fn read_image(path: PathBuf) -> Result<ReadMediaResponse> {
    let bytes = fs::read(&path)
        .await
        .with_context(|| format!("failed to read image '{}'", path.display()))?;
    let path_display = path.display().to_string();

    tokio::task::spawn_blocking(move || -> Result<ReadMediaResponse> {
        let optimized =
            chelix_media::image_ops::optimize_for_llm(&bytes, None).map_err(|error| {
                anyhow!(
                    "failed to decode or optimize image '{}': {error}",
                    path_display
                )
            })?;
        let media_type =
            chelix_media::mime::detect_mime(&optimized.data, Some(&optimized.media_type));
        use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};

        Ok(ReadMediaResponse::Image {
            file_path: path_display,
            media_type,
            original_width: optimized.original_width,
            original_height: optimized.original_height,
            final_width: optimized.final_width,
            final_height: optimized.final_height,
            was_resized: optimized.was_resized,
            bytes: optimized.data.len(),
            base64: BASE64.encode(&optimized.data),
        })
    })
    .await
    .map_err(|error| {
        anyhow!(
            "image processing task failed for '{}': {error}",
            path.display()
        )
    })?
}

async fn read_pdf(path: PathBuf, pages: Option<String>) -> Result<ReadMediaResponse> {
    let path_display = path.display().to_string();
    let task_path_display = path_display.clone();

    tokio::task::spawn_blocking(move || -> Result<ReadMediaResponse> {
        let all_pages = pdf_extract::extract_text_by_pages(&path)
            .map_err(|error| anyhow!("failed to decode PDF '{}': {error}", task_path_display))?;
        let total_pages = all_pages.len();
        if total_pages == 0 {
            bail!("PDF '{}' contains no pages", task_path_display);
        }

        let (start_page, end_page) = if let Some(range_str) = pages.as_deref() {
            let (start, end) = parse_page_range(range_str)?;
            if start > total_pages || end > total_pages {
                bail!(
                    "page range {start}-{end} exceeds total pages {total_pages}"
                );
            }
            let page_count = end - start + 1;
            if page_count > MAX_PDF_PAGES_PER_REQUEST {
                bail!(
                    "page range {start}-{end} spans {page_count} pages; maximum is {MAX_PDF_PAGES_PER_REQUEST} per request"
                );
            }
            (start, end)
        } else {
            (1, total_pages.min(MAX_PDF_PAGES_PER_REQUEST))
        };

        let selected_pages = &all_pages[start_page - 1..end_page];
        let mut content = String::new();
        for (index, page_text) in selected_pages.iter().enumerate() {
            let page_number = start_page + index;
            if !content.is_empty() {
                content.push_str("\n\n");
            }
            content.push_str(&format!("--- Page {page_number} ---\n"));
            content.push_str(page_text.trim());
        }

        Ok(ReadMediaResponse::Pdf {
            file_path: task_path_display,
            total_pages,
            pages_returned: selected_pages.len(),
            start_page,
            end_page,
            truncated: end_page < total_pages,
            content,
        })
    })
    .await
    .map_err(|error| anyhow!("PDF extraction task failed for '{}': {error}", path_display))?
}

fn parse_page_range(raw: &str) -> Result<(usize, usize)> {
    let raw = raw.trim();
    if let Some((start_raw, end_raw)) = raw.split_once('-') {
        let start: usize = start_raw
            .trim()
            .parse()
            .map_err(|_| anyhow!("invalid page range start: '{start_raw}'"))?;
        let end: usize = end_raw
            .trim()
            .parse()
            .map_err(|_| anyhow!("invalid page range end: '{end_raw}'"))?;
        if start == 0 || end == 0 {
            bail!("page numbers are 1-indexed (0 is not valid)");
        }
        if start > end {
            bail!("page range start ({start}) must be <= end ({end})");
        }
        return Ok((start, end));
    }

    let page: usize = raw
        .parse()
        .map_err(|_| anyhow!("invalid page number: '{raw}'"))?;
    if page == 0 {
        bail!("page numbers are 1-indexed (0 is not valid)");
    }
    Ok((page, page))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tiny_jpeg_bytes() -> Vec<u8> {
        #[rustfmt::skip]
        const TINY_JPEG: &[u8] = &[
            0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10, 0x4A, 0x46, 0x49, 0x46, 0x00, 0x01, 0x01, 0x00, 0x00,
            0x01, 0x00, 0x01, 0x00, 0x00, 0xFF, 0xDB, 0x00, 0x43, 0x00, 0x08, 0x06, 0x06, 0x07, 0x06,
            0x05, 0x08, 0x07, 0x07, 0x07, 0x09, 0x09, 0x08, 0x0A, 0x0C, 0x14, 0x0D, 0x0C, 0x0B, 0x0B,
            0x0C, 0x19, 0x12, 0x13, 0x0F, 0x14, 0x1D, 0x1A, 0x1F, 0x1E, 0x1D, 0x1A, 0x1C, 0x1C, 0x20,
            0x24, 0x2E, 0x27, 0x20, 0x22, 0x2C, 0x23, 0x1C, 0x1C, 0x28, 0x37, 0x29, 0x2C, 0x30, 0x31,
            0x34, 0x34, 0x34, 0x1F, 0x27, 0x39, 0x3D, 0x38, 0x32, 0x3C, 0x2E, 0x33, 0x34, 0x32, 0xFF,
            0xC0, 0x00, 0x0B, 0x08, 0x00, 0x01, 0x00, 0x01, 0x01, 0x01, 0x11, 0x00, 0xFF, 0xC4, 0x00,
            0x1F, 0x00, 0x00, 0x01, 0x05, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0A, 0x0B,
            0xFF, 0xC4, 0x00, 0xB5, 0x10, 0x00, 0x02, 0x01, 0x03, 0x03, 0x02, 0x04, 0x03, 0x05, 0x05,
            0x04, 0x04, 0x00, 0x00, 0x01, 0x7D, 0x01, 0x02, 0x03, 0x00, 0x04, 0x11, 0x05, 0x12, 0x21,
            0x31, 0x41, 0x06, 0x13, 0x51, 0x61, 0x07, 0x22, 0x71, 0x14, 0x32, 0x81, 0x91, 0xA1, 0x08,
            0x23, 0x42, 0xB1, 0xC1, 0x15, 0x52, 0xD1, 0xF0, 0x24, 0x33, 0x62, 0x72, 0x82, 0x09, 0x0A,
            0x16, 0x17, 0x18, 0x19, 0x1A, 0x25, 0x26, 0x27, 0x28, 0x29, 0x2A, 0x34, 0x35, 0x36, 0x37,
            0x38, 0x39, 0x3A, 0x43, 0x44, 0x45, 0x46, 0x47, 0x48, 0x49, 0x4A, 0x53, 0x54, 0x55, 0x56,
            0x57, 0x58, 0x59, 0x5A, 0x63, 0x64, 0x65, 0x66, 0x67, 0x68, 0x69, 0x6A, 0x73, 0x74, 0x75,
            0x76, 0x77, 0x78, 0x79, 0x7A, 0x83, 0x84, 0x85, 0x86, 0x87, 0x88, 0x89, 0x8A, 0x92, 0x93,
            0x94, 0x95, 0x96, 0x97, 0x98, 0x99, 0x9A, 0xA2, 0xA3, 0xA4, 0xA5, 0xA6, 0xA7, 0xA8, 0xA9,
            0xAA, 0xB2, 0xB3, 0xB4, 0xB5, 0xB6, 0xB7, 0xB8, 0xB9, 0xBA, 0xC2, 0xC3, 0xC4, 0xC5, 0xC6,
            0xC7, 0xC8, 0xC9, 0xCA, 0xD2, 0xD3, 0xD4, 0xD5, 0xD6, 0xD7, 0xD8, 0xD9, 0xDA, 0xE1, 0xE2,
            0xE3, 0xE4, 0xE5, 0xE6, 0xE7, 0xE8, 0xE9, 0xEA, 0xF1, 0xF2, 0xF3, 0xF4, 0xF5, 0xF6, 0xF7,
            0xF8, 0xF9, 0xFA, 0xFF, 0xDA, 0x00, 0x08, 0x01, 0x01, 0x00, 0x00, 0x3F, 0x00, 0xFB, 0xD5,
            0xDB, 0x20, 0xA8, 0xBA, 0xA3, 0xE8, 0xEB, 0xEC, 0x00, 0x3C, 0xF4, 0x76, 0x19, 0xE8, 0x78,
            0xAD, 0x99, 0xA0, 0x19, 0xE0, 0xD0, 0x6A, 0x40, 0x23, 0x9C, 0xD0, 0x07, 0xFF, 0xD9,
        ];

        TINY_JPEG.to_vec()
    }

    fn request(path: &Path) -> ReadMediaRequest {
        ReadMediaRequest {
            file_path: path.to_string_lossy().into_owned(),
            pdf: None,
        }
    }

    #[tokio::test]
    async fn reads_images_by_content_without_needing_an_image_extension() {
        let directory =
            tempfile::tempdir().unwrap_or_else(|error| panic!("tempdir failed: {error}"));
        let path = directory.path().join("sample.bin");
        fs::write(&path, tiny_jpeg_bytes())
            .await
            .unwrap_or_else(|error| panic!("write failed: {error}"));

        let result = run_tool(request(&path))
            .await
            .unwrap_or_else(|error| panic!("read_media failed: {error}"));

        match result {
            ReadMediaResponse::Image {
                media_type,
                original_width,
                original_height,
                final_width,
                final_height,
                bytes,
                base64,
                ..
            } => {
                assert_eq!(media_type, "image/jpeg");
                assert_eq!(original_width, 1);
                assert_eq!(original_height, 1);
                assert_eq!(final_width, 1);
                assert_eq!(final_height, 1);
                assert!(bytes > 0);
                assert!(!base64.is_empty());
            },
            other => panic!("expected image response, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn image_extension_fallback_surfaces_decode_failures_as_errors() {
        let directory =
            tempfile::tempdir().unwrap_or_else(|error| panic!("tempdir failed: {error}"));
        let path = directory.path().join("broken.png");
        fs::write(&path, b"this is not really a png")
            .await
            .unwrap_or_else(|error| panic!("write failed: {error}"));

        let error = match run_tool(request(&path)).await {
            Ok(result) => panic!("broken image must fail, got {result:?}"),
            Err(error) => error,
        };

        assert!(
            error
                .to_string()
                .contains("failed to decode or optimize image")
        );
    }

    #[tokio::test]
    async fn rejects_pdf_pages_for_images() {
        let directory =
            tempfile::tempdir().unwrap_or_else(|error| panic!("tempdir failed: {error}"));
        let path = directory.path().join("sample.png");
        fs::write(&path, tiny_jpeg_bytes())
            .await
            .unwrap_or_else(|error| panic!("write failed: {error}"));

        let mut input = request(&path);
        input.pdf = Some(chelix_protocol::ReadMediaPdfOptions {
            pages: Some("1".into()),
        });
        let error = match run_tool(input).await {
            Ok(result) => panic!("pages on image must fail, got {result:?}"),
            Err(error) => error,
        };

        assert_eq!(
            error.to_string(),
            "pdf.pages is only supported for PDF files."
        );
    }

    #[tokio::test]
    async fn rejects_non_media_files() {
        let directory =
            tempfile::tempdir().unwrap_or_else(|error| panic!("tempdir failed: {error}"));
        let path = directory.path().join("sample.txt");
        fs::write(&path, "plain text")
            .await
            .unwrap_or_else(|error| panic!("write failed: {error}"));

        let error = match run_tool(request(&path)).await {
            Ok(result) => panic!("non-media input must fail, got {result:?}"),
            Err(error) => error,
        };

        assert!(
            error
                .to_string()
                .contains("read_media supports only PDF and image files")
        );
    }

    #[test]
    fn parse_page_range_supports_single_pages_and_ranges() {
        let single_page = parse_page_range("3")
            .unwrap_or_else(|error| panic!("single page should parse: {error}"));
        assert_eq!(single_page, (3, 3));

        let range =
            parse_page_range("2-5").unwrap_or_else(|error| panic!("range should parse: {error}"));
        assert_eq!(range, (2, 5));
    }

    #[test]
    fn parse_page_range_rejects_invalid_values() {
        assert!(parse_page_range("0").is_err());
        assert!(parse_page_range("0-5").is_err());
        assert!(parse_page_range("5-2").is_err());
        assert!(parse_page_range("abc").is_err());
    }
}
