use termimad::*;
use chrono::Local;

pub fn render_output(result: &str, show_success: bool) {
    let mut skin = MadSkin::default();
    // Set up colors - using ANSI codes for better control
    skin.paragraph.set_fg(termimad::ansi(37)); // Light gray for text
    skin.bold.set_fg(termimad::ansi(33)); // Yellow for bold
    skin.italic.set_fg(termimad::ansi(36)); // Cyan for italic
    skin.inline_code.set_fg(termimad::ansi(35)); // Magenta for inline code

    skin.print_text(result);

    if show_success {
        // Count tokens approximately (words)
        let word_count = result.split_whitespace().count();
        let now = Local::now();
        println!("\n\x1b[90m✓ result has been copied to clipboard · tokens: {} · {}\x1b[0m",
                 word_count, now.format("%H:%M:%S").to_string());
    }
}