pub fn copy_to_clipboard(text: &str) -> Result<(), Box<dyn std::error::Error>> {
    #[cfg(target_os = "linux")]
    {
        // On Linux, try to use xclip or xsel
        use std::process::Command;

        // Try xclip first
        if Command::new("xclip")
            .args(&["-selection", "clipboard"])
            .stdin(std::process::Stdio::piped())
            .spawn()
            .is_ok()
        {
            let mut child = Command::new("xclip")
                .args(&["-selection", "clipboard"])
                .stdin(std::process::Stdio::piped())
                .spawn()?;

            if let Some(ref mut stdin) = child.stdin {
                use std::io::Write;
                stdin.write_all(text.as_bytes())?;
            }

            child.wait()?;
        } else if Command::new("xsel")
            .args(&["-bi"]) // -b for clipboard, -i for input
            .stdin(std::process::Stdio::piped())
            .spawn()
            .is_ok()
        {
            // Try xsel as fallback
            let mut child = Command::new("xsel")
                .args(&["-bi"]) // -b for clipboard, -i for input
                .stdin(std::process::Stdio::piped())
                .spawn()?;

            if let Some(ref mut stdin) = child.stdin {
                use std::io::Write;
                stdin.write_all(text.as_bytes())?;
            }

            child.wait()?;
        } else {
            // Neither xclip nor xsel found
            eprintln!("Warning: Could not copy to clipboard. Install 'xclip' or 'xsel' to enable clipboard functionality:");
            eprintln!("  - Ubuntu/Debian: sudo apt-get install xclip");
            eprintln!("  - Fedora/RHEL: sudo dnf install xclip");
            eprintln!("  - Arch: sudo pacman -S xclip");
            eprintln!("  - Or install xsel: sudo apt-get install xsel");
            return Err("Clipboard utilities not found".into());
        }
    }

    #[cfg(target_os = "macos")]
    {
        use std::process::Command;
        if Command::new("pbcopy")
            .stdin(std::process::Stdio::piped())
            .spawn()
            .is_ok()
        {
            let mut child = Command::new("pbcopy")
                .stdin(std::process::Stdio::piped())
                .spawn()?;

            if let Some(ref mut stdin) = child.stdin {
                use std::io::Write;
                stdin.write_all(text.as_bytes())?;
            }

            child.wait()?;
        } else {
            eprintln!("Warning: Could not copy to clipboard. 'pbcopy' command not found.");
            return Err("pbcopy command not found".into());
        }
    }

    #[cfg(target_os = "windows")]
    {
        use clipboard::ClipboardContext;
        use clipboard::ClipboardProvider;

        let mut ctx: ClipboardContext = ClipboardProvider::new()?;
        ctx.set_contents(text.to_string())?;
    }

    Ok(())
}