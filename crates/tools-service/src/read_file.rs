//! File reader for the managed tools service.
//!
//! Text reads support bounded offset/limit paging, tail mode, and multiple
//! inclusive ranges. Binary files are returned as bounded hexadecimal dumps.

use {
    anyhow::{Context, Result, anyhow, bail},
    chelix_protocol::{ReadFileRange, ReadFileRequest},
    content_inspector::inspect,
    std::{collections::VecDeque, path::Path},
    tokio::{
        fs::File,
        io::{AsyncBufReadExt, AsyncReadExt, AsyncSeekExt, BufReader, SeekFrom},
    },
    tracing::instrument,
};

const BINARY_INSPECTION_BYTES: usize = 515;
const MAX_BINARY_HEXDUMP_BYTES: u64 = 512;
const BYTES_PER_HEXDUMP_ROW: usize = 16;
const DEFAULT_BINARY_READ_BYTES: u64 = 128;
const MAX_LINES_PER_READ: u64 = 2_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LineEnding {
    None,
    Lf,
    Cr,
    CrLf,
}

impl LineEnding {
    fn as_str(self) -> &'static str {
        match self {
            Self::None => "",
            Self::Lf => "\n",
            Self::Cr => "\r",
            Self::CrLf => "\r\n",
        }
    }
}

#[derive(Debug)]
struct TextLine {
    number: u64,
    text: String,
    ending: LineEnding,
}

struct LogicalLineReader {
    reader: BufReader<File>,
    emit_empty_at_eof: bool,
    finished: bool,
    line_number: u64,
    first_line: bool,
    last_byte: Option<u8>,
}

impl LogicalLineReader {
    fn new(file: File) -> Self {
        Self {
            reader: BufReader::new(file),
            emit_empty_at_eof: false,
            finished: false,
            line_number: 0,
            first_line: true,
            last_byte: None,
        }
    }

    async fn next_line(&mut self, path: &Path) -> Result<Option<TextLine>> {
        if self.finished {
            return Ok(None);
        }

        let mut bytes = Vec::new();
        let ending = loop {
            let buffer = self
                .reader
                .fill_buf()
                .await
                .with_context(|| format!("failed to read file '{}'", path.display()))?;
            if buffer.is_empty() {
                self.finished = true;
                if bytes.is_empty() && !self.emit_empty_at_eof {
                    return Ok(None);
                }
                self.emit_empty_at_eof = false;
                break LineEnding::None;
            }

            let separator = buffer.iter().position(|byte| matches!(byte, b'\n' | b'\r'));
            let Some(separator) = separator else {
                self.last_byte = buffer.last().copied();
                bytes.extend_from_slice(buffer);
                let consumed = buffer.len();
                self.reader.consume(consumed);
                self.emit_empty_at_eof = false;
                continue;
            };

            bytes.extend_from_slice(&buffer[..separator]);
            let separator_byte = buffer[separator];
            self.last_byte = Some(separator_byte);
            self.reader.consume(separator + 1);
            self.emit_empty_at_eof = true;

            if separator_byte == b'\r' {
                let next = self
                    .reader
                    .fill_buf()
                    .await
                    .with_context(|| format!("failed to read file '{}'", path.display()))?;
                if next.first() == Some(&b'\n') {
                    self.reader.consume(1);
                    self.last_byte = Some(b'\n');
                    break LineEnding::CrLf;
                }
                break LineEnding::Cr;
            }
            break LineEnding::Lf;
        };

        self.line_number = self
            .line_number
            .checked_add(1)
            .context("file line count exceeds the supported range")?;
        let mut text = String::from_utf8(bytes).map_err(|error| {
            anyhow!(
                "file '{}' contains invalid UTF-8 on line {}: {error}",
                path.display(),
                self.line_number
            )
        })?;
        if self.first_line {
            self.first_line = false;
            if let Some(without_bom) = text.strip_prefix('\u{feff}') {
                text = without_bom.to_string();
            }
        }
        Ok(Some(TextLine {
            number: self.line_number,
            text,
            ending,
        }))
    }

    fn ends_with_lf(&self) -> bool {
        self.last_byte == Some(b'\n')
    }
}

#[derive(Debug)]
struct RequestedRange {
    start_line: u64,
    end_line: u64,
}

impl TryFrom<&ReadFileRange> for RequestedRange {
    type Error = anyhow::Error;

    fn try_from(range: &ReadFileRange) -> Result<Self> {
        let start_line = u64::try_from(range.start_line)
            .context("range start line exceeds the supported range")?;
        let end_line = u64::try_from(range.end_line.unwrap_or(range.start_line))
            .context("range end line exceeds the supported range")?;
        Ok(if start_line <= end_line {
            Self {
                start_line,
                end_line,
            }
        } else {
            Self {
                start_line: end_line,
                end_line: start_line,
            }
        })
    }
}

#[derive(Debug)]
struct RangeLine {
    number: u64,
    text: String,
}

#[instrument(skip_all, fields(path = %request.file_path.trim()))]
pub(crate) async fn run_tool(request: ReadFileRequest) -> Result<String> {
    request.validate().map_err(anyhow::Error::from)?;
    let path = Path::new(request.file_path.trim());
    if !path.is_absolute() {
        bail!("filePath must be absolute.");
    }

    let mut file = File::open(path)
        .await
        .with_context(|| format!("failed to open file '{}'", path.display()))?;
    let metadata = file
        .metadata()
        .await
        .with_context(|| format!("failed to inspect file '{}'", path.display()))?;
    if !metadata.is_file() {
        bail!("Path points to a directory, not a file: {}", path.display());
    }
    if metadata.len() == 0 {
        return Ok(empty_file_message(path));
    }

    let mut prefix = vec![0_u8; BINARY_INSPECTION_BYTES];
    let prefix_len = file
        .read(&mut prefix)
        .await
        .with_context(|| format!("failed to read file '{}'", path.display()))?;
    prefix.truncate(prefix_len);
    if inspect(&prefix).is_binary() {
        return read_binary(file, metadata.len(), &request, path).await;
    }

    file.seek(SeekFrom::Start(0))
        .await
        .with_context(|| format!("failed to seek file '{}'", path.display()))?;
    if request.ranges.is_empty() {
        return read_text_offset_or_tail(file, &request, path).await;
    }
    read_text_ranges(file, &request, path).await
}

async fn read_binary(
    mut file: File,
    total_bytes: u64,
    request: &ReadFileRequest,
    path: &Path,
) -> Result<String> {
    let (start_byte, end_byte, truncated) = binary_byte_range(total_bytes, request)?;
    file.seek(SeekFrom::Start(start_byte))
        .await
        .with_context(|| format!("failed to seek file '{}'", path.display()))?;
    let byte_count = usize::try_from(end_byte - start_byte)
        .context("binary byte range exceeds the supported range")?;
    let mut data = vec![0_u8; byte_count];
    file.read_exact(&mut data)
        .await
        .with_context(|| format!("file '{}' changed while it was being read", path.display()))?;

    let hexdump = format_hexdump(&data, start_byte);
    if !truncated {
        return Ok(hexdump);
    }
    Ok(format!(
        "{hexdump}\n[Binary content truncated at byte {end_byte}. Request a smaller or later byte range to inspect more.]"
    ))
}

fn binary_byte_range(total_bytes: u64, request: &ReadFileRequest) -> Result<(u64, u64, bool)> {
    let limit = request.limit.map(u64::try_from).transpose()?;
    if request.offset == Some(-1) {
        let requested_length = limit.unwrap_or(1);
        let effective_length = requested_length.min(MAX_BINARY_HEXDUMP_BYTES);
        let start_byte = total_bytes.saturating_sub(effective_length);
        return Ok((
            start_byte,
            total_bytes,
            requested_length > effective_length && start_byte > 0,
        ));
    }

    let requested_start = request.offset.map(u64::try_from).transpose()?.unwrap_or(0);
    let requested_length = if request.offset.is_some() && limit.is_some() {
        limit.unwrap_or(DEFAULT_BINARY_READ_BYTES)
    } else {
        DEFAULT_BINARY_READ_BYTES
    };
    let requested_end = requested_start
        .checked_add(requested_length)
        .context("requested binary byte range exceeds the supported range")?;
    let start_byte = requested_start.min(total_bytes);
    let available_end = requested_end.min(total_bytes);
    let capped_end = start_byte
        .checked_add(MAX_BINARY_HEXDUMP_BYTES)
        .context("binary byte range exceeds the supported range")?;
    let end_byte = available_end.min(capped_end);
    Ok((
        start_byte,
        end_byte,
        start_byte != requested_start || end_byte < available_end,
    ))
}

async fn read_text_offset_or_tail(
    file: File,
    request: &ReadFileRequest,
    path: &Path,
) -> Result<String> {
    if request.offset == Some(-1) {
        return read_text_tail(file, request, path).await;
    }

    let start_line = request.offset.map(u64::try_from).transpose()?.unwrap_or(1);
    let requested_limit = request.limit.map(u64::try_from).transpose()?;
    let effective_limit = requested_limit
        .unwrap_or(MAX_LINES_PER_READ)
        .min(MAX_LINES_PER_READ);
    let selection_end = start_line
        .checked_add(effective_limit - 1)
        .context("requested line range exceeds the supported range")?;
    let mut reader = LogicalLineReader::new(file);
    let mut selected = Vec::new();
    let mut total_lines = 0_u64;
    let mut has_non_whitespace = false;

    while let Some(line) = reader.next_line(path).await? {
        total_lines = line.number;
        has_non_whitespace |= line
            .text
            .chars()
            .any(|character| !character.is_whitespace());
        if line.number >= start_line && line.number <= selection_end {
            selected.push(line);
        }
    }
    if !has_non_whitespace {
        return Ok(whitespace_file_message(path));
    }
    if start_line > total_lines {
        bail!(
            "Invalid offset {start_line}: file only has {total_lines} line{}. Line numbers are 1-indexed.",
            if total_lines == 1 {
                ""
            } else {
                "s"
            }
        );
    }

    let end_line = selection_end.min(total_lines);
    let mut output = render_text_lines(&selected);
    if requested_limit != Some(effective_limit) && end_line < total_lines {
        output.push_str(&format!(
            "\n[File content truncated at line {end_line}. Use read_file with offset/limit parameters to view more.]"
        ));
    }
    Ok(output)
}

async fn read_text_tail(file: File, request: &ReadFileRequest, path: &Path) -> Result<String> {
    let requested_limit = request.limit.map(u64::try_from).transpose()?.unwrap_or(1);
    let effective_limit = requested_limit.min(MAX_LINES_PER_READ);
    let retained_limit = usize::try_from(effective_limit + 1)
        .context("tail line limit exceeds the supported range")?;
    let mut reader = LogicalLineReader::new(file);
    let mut retained = VecDeque::with_capacity(retained_limit);
    let mut has_non_whitespace = false;

    while let Some(line) = reader.next_line(path).await? {
        has_non_whitespace |= line
            .text
            .chars()
            .any(|character| !character.is_whitespace());
        retained.push_back(line);
        if retained.len() > retained_limit {
            retained.pop_front();
        }
    }
    if !has_non_whitespace {
        return Ok(whitespace_file_message(path));
    }
    if reader.ends_with_lf() && retained.len() > 1 {
        retained.pop_back();
    }
    let effective_limit =
        usize::try_from(effective_limit).context("tail line limit exceeds the supported range")?;
    while retained.len() > effective_limit {
        retained.pop_front();
    }
    Ok(render_text_lines(retained.make_contiguous()))
}

async fn read_text_ranges(file: File, request: &ReadFileRequest, path: &Path) -> Result<String> {
    let requested_ranges = request
        .ranges
        .iter()
        .map(RequestedRange::try_from)
        .collect::<Result<Vec<_>>>()?;
    let mut selected: Vec<Vec<RangeLine>> = requested_ranges.iter().map(|_| Vec::new()).collect();
    let mut reader = LogicalLineReader::new(file);
    let mut total_lines = 0_u64;
    let mut has_non_whitespace = false;

    while let Some(line) = reader.next_line(path).await? {
        total_lines = line.number;
        has_non_whitespace |= line
            .text
            .chars()
            .any(|character| !character.is_whitespace());
        for (index, range) in requested_ranges.iter().enumerate() {
            if line.number >= range.start_line && line.number <= range.end_line {
                selected[index].push(RangeLine {
                    number: line.number,
                    text: line.text.clone(),
                });
            }
        }
    }
    if !has_non_whitespace {
        return Ok(whitespace_file_message(path));
    }
    for (index, range) in requested_ranges.iter().enumerate() {
        if range.start_line > total_lines {
            bail!(
                "Invalid ranges[{index}].startLine {}: file only has {total_lines} line{}. Line numbers are 1-indexed.",
                range.start_line,
                if total_lines == 1 {
                    ""
                } else {
                    "s"
                }
            );
        }
    }

    let blocks = requested_ranges
        .iter()
        .zip(selected)
        .map(|(range, lines)| {
            let end_line = range.end_line.min(total_lines);
            let line_output = lines
                .iter()
                .map(|line| {
                    let blank = line.text.is_empty();
                    if request.include_line_numbers && (!blank || request.number_blank_lines) {
                        format!("{}\t{}", line.number, line.text)
                    } else {
                        line.text.clone()
                    }
                })
                .collect::<Vec<_>>()
                .join("\n");
            if !request.include_range_headers {
                return line_output;
            }
            let header = format!("--- lines {}-{end_line} ---", range.start_line);
            if line_output.is_empty() {
                header
            } else {
                format!("{header}\n{line_output}")
            }
        })
        .collect::<Vec<_>>();
    Ok(blocks.join(if request.include_range_headers {
        "\n\n"
    } else {
        "\n"
    }))
}

fn render_text_lines(lines: &[TextLine]) -> String {
    let mut output = String::new();
    for (index, line) in lines.iter().enumerate() {
        output.push_str(&line.text);
        if index + 1 < lines.len() {
            output.push_str(line.ending.as_str());
        }
    }
    output
}

fn format_hexdump(data: &[u8], start_byte: u64) -> String {
    data.chunks(BYTES_PER_HEXDUMP_ROW)
        .enumerate()
        .map(|(index, chunk)| {
            let row_offset = start_byte + (index * BYTES_PER_HEXDUMP_ROW) as u64;
            let hex = chunk
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<Vec<_>>()
                .join(" ");
            let ascii = chunk
                .iter()
                .map(|byte| {
                    if (0x20..=0x7e).contains(byte) {
                        char::from(*byte)
                    } else {
                        '.'
                    }
                })
                .collect::<String>();
            format!("{row_offset:08x}  {hex:<47}  {ascii}")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn empty_file_message(path: &Path) -> String {
    format!("(The file `{}` exists, but is empty)", path.display())
}

fn whitespace_file_message(path: &Path) -> String {
    format!(
        "(The file `{}` exists, but contains only whitespace)",
        path.display()
    )
}

#[allow(clippy::unwrap_used, clippy::expect_used)]
#[cfg(test)]
mod tests {
    use super::*;

    fn request(path: &Path) -> ReadFileRequest {
        ReadFileRequest {
            file_path: path.to_string_lossy().into_owned(),
            offset: None,
            limit: None,
            ranges: Vec::new(),
            include_line_numbers: false,
            number_blank_lines: false,
            include_range_headers: false,
        }
    }

    #[tokio::test]
    async fn reads_text_offset_tail_and_line_endings() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("sample.txt");
        tokio::fs::write(&path, "line 1\r\nline 2\r\n\r\nline 4\r\nline 5\r\n")
            .await
            .unwrap();

        let mut input = request(&path);
        input.offset = Some(2);
        input.limit = Some(4);
        assert_eq!(
            run_tool(input).await.unwrap(),
            "line 2\r\n\r\nline 4\r\nline 5"
        );

        let mut input = request(&path);
        input.offset = Some(-1);
        input.limit = Some(2);
        assert_eq!(run_tool(input).await.unwrap(), "line 4\r\nline 5");
    }

    #[tokio::test]
    async fn reads_ranges_with_numbers_headers_and_reversed_bounds() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("sample.txt");
        tokio::fs::write(&path, "line 1\nline 2\n\nline 4\nline 5")
            .await
            .unwrap();
        let mut input = request(&path);
        input.ranges = vec![
            ReadFileRange {
                start_line: 4,
                end_line: Some(2),
            },
            ReadFileRange {
                start_line: 5,
                end_line: None,
            },
        ];
        input.include_line_numbers = true;
        input.number_blank_lines = true;
        input.include_range_headers = true;

        assert_eq!(
            run_tool(input).await.unwrap(),
            "--- lines 2-4 ---\n2\tline 2\n3\t\n4\tline 4\n\n--- lines 5-5 ---\n5\tline 5"
        );
    }

    #[tokio::test]
    async fn returns_messages_for_empty_and_whitespace_files() {
        let directory = tempfile::tempdir().unwrap();
        let empty = directory.path().join("empty.txt");
        let whitespace = directory.path().join("whitespace.txt");
        tokio::fs::write(&empty, "").await.unwrap();
        tokio::fs::write(&whitespace, " \t\r\n").await.unwrap();

        assert_eq!(
            run_tool(request(&empty)).await.unwrap(),
            empty_file_message(&empty)
        );
        assert_eq!(
            run_tool(request(&whitespace)).await.unwrap(),
            whitespace_file_message(&whitespace)
        );
    }

    #[tokio::test]
    async fn returns_bounded_binary_hexdumps_and_tail() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("sample.bin");
        tokio::fs::write(&path, [0x4d, 0x5a, 0x00, 0x03, 0x00, 0x00, 0xff, 0xfe])
            .await
            .unwrap();

        let mut input = request(&path);
        input.offset = Some(1);
        input.limit = Some(8);
        let output = run_tool(input).await.unwrap();
        assert!(output.contains("00000001"));
        assert!(output.contains("5a 00 03"));
        assert!(output.contains('Z'));

        let mut input = request(&path);
        input.offset = Some(-1);
        input.limit = Some(2);
        let output = run_tool(input).await.unwrap();
        assert!(output.contains("00000006"));
        assert!(output.contains("ff fe"));
        assert!(!output.contains("4d 5a"));
    }

    #[tokio::test]
    async fn marks_only_implicit_or_capped_text_reads_as_truncated() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("large.txt");
        let content = (1..=2_005)
            .map(|line| format!("line {line}"))
            .collect::<Vec<_>>()
            .join("\n");
        tokio::fs::write(&path, content).await.unwrap();

        let output = run_tool(request(&path)).await.unwrap();
        assert!(output.contains("line 2000"));
        assert!(!output.contains("line 2001"));
        assert!(output.ends_with(
            "[File content truncated at line 2000. Use read_file with offset/limit parameters to view more.]"
        ));

        let mut explicit = request(&path);
        explicit.limit = Some(2_000);
        let output = run_tool(explicit).await.unwrap();
        assert!(!output.contains("[File content truncated"));
    }

    #[tokio::test]
    async fn rejects_invalid_paths_offsets_and_ranges() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("sample.txt");
        tokio::fs::write(&path, "one\ntwo").await.unwrap();

        let mut invalid_offset = request(&path);
        invalid_offset.offset = Some(3);
        assert!(
            run_tool(invalid_offset)
                .await
                .unwrap_err()
                .to_string()
                .contains("Invalid offset 3")
        );

        let mut invalid_range = request(&path);
        invalid_range.ranges = vec![ReadFileRange {
            start_line: 3,
            end_line: None,
        }];
        assert!(
            run_tool(invalid_range)
                .await
                .unwrap_err()
                .to_string()
                .contains("Invalid ranges[0].startLine 3")
        );

        let relative = ReadFileRequest {
            file_path: "relative.txt".into(),
            ..request(&path)
        };
        assert_eq!(
            run_tool(relative).await.unwrap_err().to_string(),
            "filePath must be absolute."
        );
        assert!(run_tool(request(directory.path())).await.is_err());
    }
}
