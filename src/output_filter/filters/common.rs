pub const MAX_DEFAULT_LINES: usize = 100;
pub const MAX_DEFAULT_LINE_CHARS: usize = 200;

pub fn words(command: &str) -> Vec<&str> {
    command.split_whitespace().collect()
}

pub fn has_word(command: &str, word: &str) -> bool {
    words(command).iter().any(|part| *part == word)
}

pub fn truncate_chars(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let mut end = text.len().min(max_chars);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &text[..end])
}

pub fn strip_ansi(raw: &str) -> String {
    let mut output = String::with_capacity(raw.len());
    let mut chars = raw.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != '\x1b' {
            output.push(ch);
            continue;
        }
        if matches!(chars.peek(), Some('[')) {
            chars.next();
            for code in chars.by_ref() {
                if ('@'..='~').contains(&code) {
                    break;
                }
            }
        }
    }
    output
}
