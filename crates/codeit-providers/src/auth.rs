//! Saved logins, in opencode's `auth.json` format: `{ "<provider>": { "type": "api", "key": … } }`
//! or `{ "type": "oauth", "access": …, "refresh": …, "expires": <ms>, "accountId": … }`.
//!
//! codeit's own file (`~/.local/share/codeit/auth.json`) wins; an opencode login
//! (`~/.local/share/opencode/auth.json`) is used when codeit has none for that provider.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::{Value, json};

pub fn codeit_path() -> PathBuf {
    crate::data_dir().join("auth.json")
}

fn opencode_path() -> PathBuf {
    crate::paths::xdg("XDG_DATA_HOME", ".local/share").join("opencode").join("auth.json")
}

fn read(path: &Path) -> Value {
    std::fs::read_to_string(path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or(json!({}))
}

/// The saved login for `provider` and where it came from ("codeit login" or "opencode login").
pub fn get(provider: &str) -> Option<(Value, &'static str)> {
    for (path, from) in [(codeit_path(), "codeit login"), (opencode_path(), "opencode login")] {
        let v = read(&path)[provider].clone();
        if v.is_object() {
            return Some((v, from));
        }
    }
    None
}

/// The saved API key for `provider`, if its login is a key.
pub fn api_key(provider: &str) -> Option<(String, &'static str)> {
    let (v, from) = get(provider)?;
    (v["type"] == "api").then(|| v["key"].as_str().map(|k| (k.to_string(), from))).flatten().filter(|k| !k.0.is_empty())
}

pub fn set(provider: &str, entry: Value) -> Result<()> {
    let path = codeit_path();
    let mut all = read(&path);
    all[provider] = entry;
    write_private(&path, &serde_json::to_string_pretty(&all)?)
}

/// Removes codeit's login for `provider`. Returns false when there was none.
pub fn remove(provider: &str) -> Result<bool> {
    let path = codeit_path();
    let mut all = read(&path);
    let removed = all.as_object_mut().and_then(|o| o.remove(provider)).is_some();
    if removed {
        write_private(&path, &serde_json::to_string_pretty(&all)?)?;
    }
    Ok(removed)
}

/// Every provider codeit has a saved login for.
pub fn saved() -> Vec<String> {
    read(&codeit_path()).as_object().map(|o| o.keys().cloned().collect()).unwrap_or_default()
}

/// Writes a file only you can read.
pub(crate) fn write_private(path: &Path, text: &str) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    use std::io::Write;
    let mut f = opts.open(path).with_context(|| format!("writing {}", path.display()))?;
    f.write_all(text.as_bytes())?;
    Ok(())
}
