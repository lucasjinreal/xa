use super::common::has_word;

const MAX_DIAGNOSTICS: usize = 40;

pub fn matches(command: &str) -> bool {
    has_word(command, "cargo")
}

pub fn filter(raw: &str) -> String {
    let mut output = String::new();
    let mut diagnostics = 0usize;
    let mut summaries = 0usize;
    for line in raw.lines() {
        let trimmed = line.trim_start();
        let diagnostic = trimmed.starts_with("error") || trimmed.starts_with("warning")
            || trimmed.starts_with("-->") || trimmed.starts_with("help:")
            || trimmed.starts_with("note:");
        let summary = trimmed.starts_with("test result:") || trimmed.starts_with("running ")
            || trimmed.starts_with("Finished ") || trimmed.starts_with("error: could not compile");
        if diagnostic {
            if diagnostics == MAX_DIAGNOSTICS {
                output.push_str("[… additional diagnostics omitted]\n");
                continue;
            }
            output.push_str(trimmed);
            output.push('\n');
            diagnostics += 1;
        } else if summary {
            output.push_str(trimmed);
            output.push('\n');
            summaries += 1;
        }
    }
    if diagnostics == 0 && summaries == 0 { super::default::filter(raw) } else { output }
}
