use super::common::{strip_ansi, truncate_chars, MAX_DEFAULT_LINE_CHARS, MAX_DEFAULT_LINES};

pub fn filter(raw: &str) -> String {
    let cleaned = strip_ansi(raw);
    let mut output = String::new();
    for (index, line) in cleaned.lines().enumerate() {
        if index == MAX_DEFAULT_LINES {
            output.push_str("[… output truncated]\n");
            break;
        }
        output.push_str(&truncate_chars(line, MAX_DEFAULT_LINE_CHARS));
        output.push('\n');
    }
    output
}
