use termimad::*;

pub fn render_output(result: &str, show_success: bool) {
    let skin = MadSkin::default();
    skin.print_text(result);

    if show_success {
        // Count tokens approximately (words)
        let word_count = result.split_whitespace().count();
        use chrono::Local;
        let now = Local::now();
        println!("\n\x1b[90m✓ result has been copied to clipboard · tokens: {} · {}",
                 word_count, now.format("%H:%M:%S").to_string());
    }
}