pub const DEFAULT_MAX_LINES: usize = 2000;
pub const DEFAULT_MAX_BYTES: usize = 12 * 1024;

#[derive(Debug, Clone)]
pub struct TruncationResult {
    pub content: String,
    pub truncated: bool,
}

pub fn format_size(bytes: usize) -> String {
    if bytes < 1024 {
        format!("{bytes}B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1}KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1}MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

pub fn append_notices(output: &mut String, notices: &[String]) {
    if !notices.is_empty() {
        output.push_str("\n\n[");
        output.push_str(&notices.join(". "));
        output.push(']');
    }
}

fn split_lines_for_counting(content: &str) -> Vec<&str> {
    if content.is_empty() {
        return Vec::new();
    }
    let mut lines: Vec<&str> = content.split('\n').collect();
    if content.ends_with('\n') {
        lines.pop();
    }
    lines
}

pub fn truncate_head(content: &str) -> TruncationResult {
    let max_lines = DEFAULT_MAX_LINES;
    let max_bytes = DEFAULT_MAX_BYTES;
    let total_bytes = content.len();
    let lines = split_lines_for_counting(content);
    let total_lines = lines.len();

    if total_lines <= max_lines && total_bytes <= max_bytes {
        return TruncationResult {
            content: content.to_string(),
            truncated: false,
        };
    }

    if !lines.is_empty() {
        let first_line_bytes = lines[0].len();
        if first_line_bytes > max_bytes {
            return TruncationResult {
                content: String::new(),
                truncated: true,
            };
        }
    }

    let mut output: Vec<&str> = Vec::new();
    let mut bytes_used: usize = 0;

    for line in lines.iter() {
        if output.len() >= max_lines {
            break;
        }
        let line_bytes = line.len() + if output.is_empty() { 0 } else { 1 };
        if bytes_used + line_bytes > max_bytes {
            break;
        }
        output.push(line);
        bytes_used += line_bytes;
    }

    let out_content = output.join("\n");

    TruncationResult {
        content: out_content,
        truncated: true,
    }
}
