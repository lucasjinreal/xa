//! Terminal light/dark detection (COLORFGBG + OSC 11).

use super::ColorMode;

/// Relative luminance threshold: backgrounds brighter than this are "light".
const LUMINANCE_LIGHT_THRESHOLD: f32 = 0.55;

/// Detect whether the terminal background is dark or light.
///
/// Order:
/// 1. `COLORFGBG` environment variable (cheap, no I/O)
/// 2. OSC 11 background-color query (when a controlling TTY is available)
/// 3. Default → [`ColorMode::Dark`]
pub fn detect_color_mode() -> ColorMode {
    if let Ok(v) = std::env::var("COLORFGBG") {
        if let Some(mode) = mode_from_colorfgbg(&v) {
            return mode;
        }
    }
    if let Some(mode) = query_osc11_mode() {
        return mode;
    }
    ColorMode::Dark
}

pub fn mode_from_rgb(r: u8, g: u8, b: u8) -> ColorMode {
    if relative_luminance(r, g, b) > LUMINANCE_LIGHT_THRESHOLD {
        ColorMode::Light
    } else {
        ColorMode::Dark
    }
}

fn relative_luminance(r: u8, g: u8, b: u8) -> f32 {
    // sRGB relative luminance (linear-ish; good enough for bg classification).
    let r = r as f32 / 255.0;
    let g = g as f32 / 255.0;
    let b = b as f32 / 255.0;
    0.2126 * r + 0.7152 * g + 0.0722 * b
}

/// `COLORFGBG` is typically `fg;bg` with ANSI color indices (e.g. `15;0`).
///
/// Heuristic used by many CLIs: bg in `{0..=6, 8}` → dark, otherwise light.
pub fn mode_from_colorfgbg(value: &str) -> Option<ColorMode> {
    let bg = value
        .split(';')
        .last()?
        .split(':')
        .next()?
        .trim()
        .parse::<u8>()
        .ok()?;
    Some(match bg {
        0..=6 | 8 => ColorMode::Dark,
        _ => ColorMode::Light,
    })
}

/// Parse an OSC 11 reply payload into 8-bit RGB.
///
/// Accepts both BEL-terminated (`\x07`) and ST-terminated (`ESC \`) forms, and
/// `rgb:rrrr/gggg/bbbb` with 1–4 hex digits per channel.
pub fn parse_osc11_response(buf: &[u8]) -> Option<(u8, u8, u8)> {
    let s = std::str::from_utf8(buf).ok()?;
    // Find `rgb:` case-insensitively.
    let lower = s.to_ascii_lowercase();
    let idx = lower.find("rgb:")?;
    let rest = &s[idx + 4..];
    let end = rest
        .find(['\x07', '\x1b', '\x5c', ';', '\n', '\r'])
        .unwrap_or(rest.len());
    let body = rest[..end].trim();
    let mut parts = body.split('/');
    let r = parse_osc_component(parts.next()?)?;
    let g = parse_osc_component(parts.next()?)?;
    let b = parse_osc_component(parts.next()?)?;
    Some((r, g, b))
}

fn parse_osc_component(hex: &str) -> Option<u8> {
    let hex = hex.trim();
    if hex.is_empty() || hex.len() > 4 {
        return None;
    }
    let v = u16::from_str_radix(hex, 16).ok()?;
    // Scale n-digit hex (0..16^n-1) into 0..255.
    let max = match hex.len() {
        1 => 0xF,
        2 => 0xFF,
        3 => 0xFFF,
        _ => 0xFFFF,
    };
    Some(((v as u32 * 255) / max as u32) as u8)
}

#[cfg(unix)]
fn query_osc11_mode() -> Option<ColorMode> {
    let (r, g, b) = query_osc11_bg()?;
    Some(mode_from_rgb(r, g, b))
}

#[cfg(not(unix))]
fn query_osc11_mode() -> Option<ColorMode> {
    None
}

/// Query the terminal background via OSC 11 on `/dev/tty`.
#[cfg(unix)]
fn query_osc11_bg() -> Option<(u8, u8, u8)> {
    use std::fs::OpenOptions;
    use std::io::{Read, Write};
    use std::os::unix::io::AsRawFd;
    use std::time::{Duration, Instant};

    // Skip when stdout is not a TTY (pipes, CI).
    if !crossterm::tty::IsTty::is_tty(&std::io::stdout()) {
        return None;
    }

    let mut tty = OpenOptions::new().read(true).write(true).open("/dev/tty").ok()?;
    let fd = tty.as_raw_fd();

    // Make the fd non-blocking so we can timeout the read.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        return None;
    }
    if unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return None;
    }

    // Best-effort: temporarily put the tty into non-canonical mode so the
    // response is available without a newline. Restore on exit.
    let restored = TtyRawGuard::enter(fd);

    // Prefer ST terminator; many terminals accept either.
    let _ = tty.write_all(b"\x1b]11;?\x1b\\");
    let _ = tty.flush();

    let deadline = Instant::now() + Duration::from_millis(80);
    let mut buf = Vec::with_capacity(64);
    let mut tmp = [0u8; 128];

    while Instant::now() < deadline {
        match tty.read(&mut tmp) {
            Ok(0) => break,
            Ok(n) => {
                buf.extend_from_slice(&tmp[..n]);
                if let Some(rgb) = parse_osc11_response(&buf) {
                    drop(restored);
                    let _ = unsafe { libc::fcntl(fd, libc::F_SETFL, flags) };
                    return Some(rgb);
                }
                // Cap runaway buffers.
                if buf.len() > 512 {
                    break;
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(_) => break,
        }
    }

    drop(restored);
    let _ = unsafe { libc::fcntl(fd, libc::F_SETFL, flags) };
    None
}

/// RAII restore of termios after briefly enabling non-canonical reads.
#[cfg(unix)]
struct TtyRawGuard {
    fd: std::os::unix::io::RawFd,
    original: libc::termios,
}

#[cfg(unix)]
impl TtyRawGuard {
    fn enter(fd: std::os::unix::io::RawFd) -> Option<Self> {
        unsafe {
            let mut original: libc::termios = std::mem::zeroed();
            if libc::tcgetattr(fd, &mut original) != 0 {
                return None;
            }
            let mut raw = original;
            // Non-canonical, no echo — enough to receive OSC replies.
            raw.c_lflag &= !(libc::ICANON | libc::ECHO);
            raw.c_cc[libc::VMIN] = 0;
            raw.c_cc[libc::VTIME] = 0;
            if libc::tcsetattr(fd, libc::TCSANOW, &raw) != 0 {
                return None;
            }
            Some(Self { fd, original })
        }
    }
}

#[cfg(unix)]
impl Drop for TtyRawGuard {
    fn drop(&mut self) {
        unsafe {
            let _ = libc::tcsetattr(self.fd, libc::TCSANOW, &self.original);
        }
    }
}
