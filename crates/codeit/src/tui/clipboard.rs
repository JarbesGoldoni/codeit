//! The clipboard: pasting a screenshot (saved to a file the prompt can attach) and copying text.

use std::path::PathBuf;
use std::process::Command;

fn is_wsl() -> bool {
    std::fs::read_to_string("/proc/version").is_ok_and(|v| v.to_lowercase().contains("microsoft"))
}

fn ok(cmd: &mut Command) -> bool {
    cmd.output().is_ok_and(|o| o.status.success())
}

/// Saves the clipboard's image as PNG under the cache dir and returns its path; `None` when
/// the clipboard holds no image or no clipboard tool works.
pub fn save_image() -> Option<PathBuf> {
    let dir = codeit_providers::cache_dir().join("paste");
    std::fs::create_dir_all(&dir).ok()?;
    let path = dir.join(format!("{}-{}.png", codeit_harness::util::now(), std::process::id()));
    let p = path.to_string_lossy().into_owned();
    let saved = if is_wsl() {
        // Windows' clipboard, through PowerShell (STA is required for clipboard access).
        let win = Command::new("wslpath")
            .args(["-w", &p])
            .output()
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())?;
        let script = format!(
            "Add-Type -AssemblyName System.Windows.Forms; $i = [Windows.Forms.Clipboard]::GetImage(); if ($i) {{ $i.Save('{}', [System.Drawing.Imaging.ImageFormat]::Png); exit 0 }} else {{ exit 1 }}",
            win.replace('\'', "''")
        );
        ok(Command::new("powershell.exe").args(["-NoProfile", "-STA", "-Command", &script]))
    } else if cfg!(target_os = "macos") {
        ok(Command::new("osascript").args([
            "-e",
            &format!("set f to (open for access POSIX file \"{p}\" with write permission)"),
            "-e",
            "write (the clipboard as «class PNGf») to f",
            "-e",
            "close access f",
        ]))
    } else {
        let to_file = |prog: &str, args: &[&str]| {
            Command::new(prog)
                .args(args)
                .output()
                .ok()
                .filter(|o| o.status.success() && !o.stdout.is_empty())
                .is_some_and(|o| std::fs::write(&path, o.stdout).is_ok())
        };
        to_file("wl-paste", &["--type", "image/png"])
            || to_file("xclip", &["-selection", "clipboard", "-t", "image/png", "-o"])
    };
    let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    if saved && size > 0 {
        Some(path)
    } else {
        let _ = std::fs::remove_file(&path);
        None
    }
}

/// Puts `text` on the clipboard: through the system's tool when there is one, else with the
/// OSC 52 escape (which the terminal handles, so it works over SSH too).
pub fn copy_text(text: &str) {
    use std::io::Write;
    let tools: &[(&str, &[&str])] = if is_wsl() {
        &[("clip.exe", &[])]
    } else if cfg!(target_os = "macos") {
        &[("pbcopy", &[])]
    } else {
        &[("wl-copy", &[]), ("xclip", &["-selection", "clipboard"]), ("xsel", &["--clipboard", "--input"])]
    };
    for (prog, args) in tools {
        let Ok(mut child) = Command::new(prog)
            .args(*args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
        else {
            continue;
        };
        let wrote = child.stdin.take().is_some_and(|mut i| i.write_all(text.as_bytes()).is_ok());
        if wrote && child.wait().is_ok_and(|s| s.success()) {
            return;
        }
    }
    let mut out = std::io::stdout();
    let _ = write!(out, "\x1b]52;c;{}\x07", base64(text.as_bytes()));
    let _ = out.flush();
}

fn base64(bytes: &[u8]) -> String {
    const ABC: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let n = chunk.iter().enumerate().fold(0u32, |n, (i, b)| n | (*b as u32) << (16 - 8 * i));
        for i in 0..4 {
            out.push(if i <= chunk.len() { ABC[(n >> (18 - 6 * i)) as usize & 63] as char } else { '=' });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn encodes_base64() {
        assert_eq!(super::base64(b"done!"), "ZG9uZSE=");
        assert_eq!(super::base64(b"ab"), "YWI=");
    }
}
