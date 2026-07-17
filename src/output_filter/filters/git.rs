use super::common::words;

const MAX_DIFF_LINES: usize = 120;
const MAX_LOG_ENTRIES: usize = 20;

pub fn is_diff(command: &str) -> bool {
    let words = words(command);
    words.windows(2).any(|pair| pair == ["git", "diff"] || pair == ["git", "show"])
}

pub fn is_log(command: &str) -> bool {
    words(command).windows(2).any(|pair| pair == ["git", "log"])
}

pub fn is_status(command: &str) -> bool {
    words(command).windows(2).any(|pair| pair == ["git", "status"])
}

pub fn filter_diff(raw: &str) -> String {
    let mut output = String::new();
    let mut additions = 0usize;
    let mut removals = 0usize;
    let mut kept = 0usize;
    for line in raw.lines() {
        let header = line.starts_with("diff --git") || line.starts_with("---") || line.starts_with("+++");
        let change = line.starts_with("@@")
            || (line.starts_with('+') && !line.starts_with("+++"))
            || (line.starts_with('-') && !line.starts_with("---"));
        if !(header || change) {
            continue;
        }
        if kept == MAX_DIFF_LINES {
            output.push_str("[… additional diff lines omitted]\n");
            break;
        }
        if line.starts_with('+') && !line.starts_with("+++") { additions += 1; }
        if line.starts_with('-') && !line.starts_with("---") { removals += 1; }
        output.push_str(line);
        output.push('\n');
        kept += 1;
    }
    output.push_str(&format!("\n[{} additions, {} removals]\n", additions, removals));
    output
}

pub fn filter_log(raw: &str) -> String {
    let mut output = String::new();
    let mut entries = 0usize;
    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with("Author:") || line.starts_with("Date:") {
            continue;
        }
        if line.starts_with("commit ") || is_short_hash_line(line) {
            if entries == MAX_LOG_ENTRIES {
                output.push_str("[… additional commits omitted]\n");
                break;
            }
            output.push_str(line);
            output.push('\n');
            entries += 1;
        }
    }
    if output.is_empty() { super::default::filter(raw) } else { output }
}

pub fn filter_status(raw: &str) -> String {
    let mut output = String::new();
    let mut changed = 0usize;
    for line in raw.lines() {
        let keep = line.starts_with("On branch") || line.starts_with("Your branch")
            || line.starts_with("Changes") || line.starts_with("Untracked")
            || line.starts_with("nothing to commit") || line.starts_with('\t')
            || line.starts_with("??") || line.starts_with(" M") || line.starts_with("M ")
            || line.starts_with("A ") || line.starts_with(" D");
        if keep {
            output.push_str(line);
            output.push('\n');
            if line.starts_with('\t') || line.starts_with("??") || line.starts_with(" M") { changed += 1; }
        }
    }
    if output.is_empty() { super::default::filter(raw) } else {
        output.push_str(&format!("[{} changed paths shown]\n", changed));
        output
    }
}

fn is_short_hash_line(line: &str) -> bool {
    let Some(first) = line.split_whitespace().next() else { return false };
    first.len() >= 7 && first.chars().all(|ch| ch.is_ascii_hexdigit())
}
