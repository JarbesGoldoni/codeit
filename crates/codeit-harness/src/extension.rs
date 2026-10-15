//! Extensions: code that adds to codeit without changing it, the way opencode plugins do.
//!
//! An extension can change the config before anything reads it (add skill folders, instruction
//! files, commands, MCP servers), add to every system prompt, set environment variables for
//! shell commands, and provide tools. Every hook has a default that does nothing.
//!
//! It can show a few words in the footer ([`Extension::status`]), and add slash commands that open a **panel**: a titled list of sections and rows,
//! each with a tone, plus the keys the panel handles. The interface draws it; the extension
//! only answers [`Action`]s with a [`Reply`], so the same extension works in any interface.
//!
//! It can also ask for a site's cookies from your browser ([`Extension::sites`]): with
//! `codeit web` and its browser extension running, codeit reads them as they change, hands them
//! to [`Extension::cookies`], and keeps the site's login alive by loading one of its pages now
//! and then.

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;

use crate::config::Config;
use crate::tools::Tool;

#[async_trait]
pub trait Extension: Send + Sync {
    fn name(&self) -> &str;

    /// Runs once at startup, after the config files are read.
    fn config(&self, _config: &mut Config) {}

    /// Adds sections to the system prompt of every request.
    fn system(&self, _system: &mut Vec<String>) {}

    /// Environment variables for every bash call.
    fn shell_env(&self, _env: &mut BTreeMap<String, String>) {}

    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        Vec::new()
    }

    /// Slash commands that open a panel (prompt commands go through `config` instead).
    fn commands(&self) -> Vec<ExtCommand> {
        Vec::new()
    }

    /// A few words for the footer (a quota, say), in pieces with their tone. Asked shortly
    /// after start, every 5 minutes, and after a turn (at most once a minute).
    async fn status(&self) -> Vec<(String, Tone)> {
        Vec::new()
    }

    /// Answers an action on one of its commands' panels.
    async fn act(&self, _command: &str, _action: Action) -> Reply {
        Reply::Close
    }

    /// Sites whose cookies it wants from the browser.
    async fn sites(&self) -> Vec<Site> {
        Vec::new()
    }

    /// The cookies of one of its [`sites`](Extension::sites), sent when they change (and once
    /// when the browser connects). Says whether the login works with them.
    async fn cookies(&self, _site: &str, _cookies: Vec<Cookie>) -> SiteStatus {
        SiteStatus::default()
    }
}

/// A site whose login an extension keeps from the browser.
#[derive(Clone, Debug, Default)]
pub struct Site {
    /// Passed back to [`Extension::cookies`].
    pub id: String,
    pub title: String,
    /// A page of the site: its cookies are read for this URL, and it is loaded in the background
    /// every `keepalive` minutes so the login doesn't expire.
    pub url: String,
    /// The cookies wanted, by name (`JSESSIONID.*`: every name starting with `JSESSIONID.`);
    /// empty for all of the URL's.
    pub cookies: Vec<String>,
    pub keepalive: Option<u64>,
}

impl Site {
    /// Whether the site wants the cookie called `name`.
    pub fn wants(&self, name: &str) -> bool {
        self.cookies.is_empty()
            || self.cookies.iter().any(|w| match w.strip_suffix('*') {
                Some(prefix) => name.starts_with(prefix),
                None => w == name,
            })
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Cookie {
    pub name: String,
    pub value: String,
    pub domain: String,
}

#[derive(Clone, Debug, Default)]
pub struct SiteStatus {
    /// The login works. When false, the browser reloads the site's page (at most every few
    /// minutes), which often renews it.
    pub ok: bool,
    /// A few words to show: "connected", why it failed.
    pub detail: String,
}

#[derive(Clone, Debug)]
pub struct ExtCommand {
    pub name: String,
    pub description: String,
    /// A sentence or two shown under the command list when it is the one being typed.
    pub help: String,
    /// Opens a panel through [`Extension::act`]; false for prompt commands (added through
    /// `config`) listed only for their help.
    pub panel: bool,
}

#[derive(Clone, Debug)]
pub enum Action {
    /// The command was typed, with its arguments.
    Open(String),
    /// Enter on a row (its id).
    Enter(String),
    /// A key the panel listed, with the selected row's id if any.
    Key(char, Option<String>),
    /// The text asked for with [`Reply::Input`].
    Text(String),
    /// The panel asked to be polled (`Panel::poll_ms`).
    Poll,
}

/// A session for a piece of work: the saved session of this folder whose title starts with
/// `prefix` (a task key), or a new one titled `title`.
#[derive(Clone, Debug)]
pub struct SessionTarget {
    pub prefix: String,
    pub title: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Tone {
    #[default]
    Normal,
    Ok,
    Warn,
    Bad,
    Muted,
    Accent,
}

#[derive(Clone, Debug, Default)]
pub struct Row {
    /// Passed back in actions; rows without one can't be selected.
    pub id: Option<String>,
    pub label: String,
    /// Shown after the label.
    pub value: String,
    pub tone: Tone,
    /// More lines under the row, dimmer.
    pub detail: Vec<String>,
    /// A share from 0 to 1, drawn as a bar.
    pub bar: Option<f64>,
}

#[derive(Clone, Debug, Default)]
pub struct Section {
    pub title: String,
    pub rows: Vec<Row>,
}

#[derive(Clone, Debug, Default)]
pub struct Panel {
    pub title: String,
    pub sections: Vec<Section>,
    /// A line under the title: what's going on, or the last result.
    pub message: Option<(String, Tone)>,
    /// Keys the panel handles besides ↑↓, Enter and Esc: (key, what it does).
    pub keys: Vec<(char, String)>,
    /// Ask to be polled every this many milliseconds (a login waiting for approval).
    pub poll_ms: Option<u64>,
    /// Row to select when shown.
    pub select: Option<String>,
}

#[derive(Clone, Debug)]
pub enum Reply {
    Panel(Panel),
    /// Ask for a line of text; the answer comes back as [`Action::Text`].
    Input {
        title: String,
        hint: String,
        /// Don't show what is typed (tokens).
        secret: bool,
    },
    /// Close the panel and send this to the agent, in the current session or in `session`.
    /// Empty text only opens `session`.
    Prompt {
        text: String,
        session: Option<SessionTarget>,
    },
    /// Close the panel and leave this line in the history.
    Notice(String),
    Close,
}

impl Row {
    pub fn new(id: impl Into<String>, label: impl Into<String>, value: impl Into<String>, tone: Tone) -> Self {
        Row { id: Some(id.into()), label: label.into(), value: value.into(), tone, ..Default::default() }
    }

    /// A row that can't be selected.
    pub fn info(label: impl Into<String>, value: impl Into<String>, tone: Tone) -> Self {
        Row { id: None, label: label.into(), value: value.into(), tone, ..Default::default() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sites_want_cookies_by_name_or_prefix() {
        let site = Site { cookies: vec!["session".into(), "JSESSIONID.*".into()], ..Default::default() };
        assert!(site.wants("session"));
        assert!(site.wants("JSESSIONID.abc123"));
        assert!(!site.wants("sessions"));
        assert!(!site.wants("theme"));
        assert!(Site::default().wants("anything"));
    }
}
