use std::path::PathBuf;

fn home() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("."))
}

/// `$XDG_DATA_HOME/codeit` (default `~/.local/share/codeit`). Holds logins and saved state.
pub fn data_dir() -> PathBuf {
    xdg("XDG_DATA_HOME", ".local/share").join("codeit")
}

/// `$XDG_CACHE_HOME/codeit` (default `~/.cache/codeit`). Holds the debug log.
pub fn cache_dir() -> PathBuf {
    xdg("XDG_CACHE_HOME", ".cache").join("codeit")
}

/// Moves what codeit saved under its old name, done, to codeit's folders: settings
/// (`done.json` becomes `codeit.json`), logins, sessions and cache. Anything already there under
/// the new name is kept, and the old folder is removed once empty.
pub fn migrate_from_done() {
    let config = match std::env::var_os("XDG_CONFIG_HOME") {
        Some(v) if !v.is_empty() => PathBuf::from(v),
        _ => home().join(".config"),
    };
    for base in [config.clone(), xdg("XDG_DATA_HOME", ".local/share"), xdg("XDG_CACHE_HOME", ".cache")] {
        merge(&base.join("done"), &base.join("codeit"));
    }
}

/// Moves everything in `old` into `new`, keeping what `new` already has, then removes `old`.
fn merge(old: &std::path::Path, new: &std::path::Path) {
    let Ok(entries) = std::fs::read_dir(old) else { return };
    let _ = std::fs::create_dir_all(new);
    for entry in entries.flatten() {
        let mut name = entry.file_name().to_string_lossy().into_owned();
        if let Some(ext) = name.strip_prefix("done.").filter(|e| *e == "json" || *e == "jsonc") {
            name = format!("codeit.{ext}");
        }
        let to = new.join(name);
        if !to.exists() {
            let _ = std::fs::rename(entry.path(), to);
        } else if to.is_dir() && entry.path().is_dir() {
            merge(&entry.path(), &to);
        }
    }
    let _ = std::fs::remove_dir(old);
}

/// XDG on every OS except Windows, the same layout opencode uses.
pub(crate) fn xdg(var: &str, fallback: &str) -> PathBuf {
    if cfg!(windows) {
        return dirs::data_local_dir().unwrap_or_else(home);
    }
    match std::env::var_os(var) {
        Some(v) if !v.is_empty() => PathBuf::from(v),
        _ => home().join(fallback),
    }
}

/// Appends a line to `~/.cache/codeit/debug.log` when CODEIT_DEBUG is set. Never pass tokens here.
pub fn debug(message: &str, data: impl std::fmt::Display) {
    if std::env::var_os("CODEIT_DEBUG").is_none() {
        return;
    }
    use std::io::Write;
    let dir = cache_dir();
    let _ = std::fs::create_dir_all(&dir);
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(dir.join("debug.log")) {
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        let _ = writeln!(f, "{now} {message} {data}");
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn merge_moves_what_is_missing_and_keeps_the_rest() {
        let root = std::env::temp_dir().join(format!("codeit-merge-{}", std::process::id()));
        let (old, new) = (root.join("done"), root.join("codeit"));
        std::fs::create_dir_all(old.join("reviews")).unwrap();
        std::fs::create_dir_all(new.join("reviews")).unwrap();
        std::fs::write(old.join("done.json"), "old config").unwrap();
        std::fs::write(old.join("auth.json"), "login").unwrap();
        std::fs::write(old.join("reviews/a.json"), "old a").unwrap();
        std::fs::write(old.join("reviews/b.json"), "old b").unwrap();
        std::fs::write(new.join("reviews/b.json"), "new b").unwrap();

        super::merge(&old, &new);
        let read = |p: &str| std::fs::read_to_string(new.join(p)).unwrap();
        assert_eq!(read("codeit.json"), "old config");
        assert_eq!(read("auth.json"), "login");
        assert_eq!(read("reviews/a.json"), "old a");
        assert_eq!(read("reviews/b.json"), "new b");
        // Only the file that was already there under the new name is left behind.
        assert!(old.join("reviews/b.json").exists() && !old.join("auth.json").exists());
        let _ = std::fs::remove_dir_all(&root);
    }
}
