//! The agent harness for codeit.
//!
//! [`Harness`] holds what is shared by every session in a project: the providers, the config,
//! agents, skills, commands, MCP servers and extensions. [`Run`] drives one turn of a
//! [`session::Session`]: it sends the conversation to the model, runs the tools the model
//! calls, sends the results back, and repeats until the model answers. Everything the user
//! should see arrives as [`event::Event`]s.
//!
//! Context is kept small on purpose: short tool descriptions, compact tool output (cleaned
//! terminal output, long output cut to its start and end and saved to a file), repeated reads
//! of unchanged files answered with a pointer, old tool outputs pruned in batches at the end of
//! a turn (so prompt caches survive), and a structured summary when the context fills up.

pub mod agent;
pub mod command;
mod compaction;
pub mod condense;
pub mod config;
pub mod event;
pub mod extension;
pub mod instructions;
pub mod lsp;
pub mod mcp;
pub mod mcp_oauth;
pub mod permission;
mod prompt;
pub mod review;
mod run;
pub mod session;
pub mod skill;
pub mod snapshot;
pub mod tools;
pub mod util;

/// Points the data, cache and config folders at one temporary home for this test binary, so tests
/// never touch the user's real ones. Set once: the variables are process-wide and tests run in
/// parallel.
#[cfg(test)]
pub(crate) fn test_home() -> &'static std::path::Path {
    static HOME: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
    HOME.get_or_init(|| {
        let home = std::env::temp_dir().join(format!("codeit-test-home-{}", uuid::Uuid::new_v4()));
        unsafe {
            std::env::set_var("XDG_DATA_HOME", home.join("data"));
            std::env::set_var("XDG_CACHE_HOME", home.join("cache"));
            std::env::set_var("XDG_CONFIG_HOME", home.join("config"));
        }
        home
    })
}

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Result, anyhow};
use codeit_providers::{ModelInfo, Provider};

pub use run::{Input, Run};

use agent::Agent;
use command::Command;
use config::Config;
use extension::Extension;
use mcp::Mcp;
use permission::Ruleset;
use session::Approval;
use skill::Skill;
use tools::Tool;

pub struct Harness {
    /// Where codeit was started.
    pub cwd: PathBuf,
    /// The git worktree root, or `cwd`.
    pub root: PathBuf,
    pub config: Config,
    /// Config problems to show the user.
    pub problems: Vec<String>,
    pub providers: Vec<Arc<dyn Provider>>,
    pub agents: Vec<Agent>,
    /// Re-read from disk by `reload`, so new ones work without a restart.
    skills: Mutex<Vec<Skill>>,
    commands: Mutex<Vec<Command>>,
    pub extensions: Vec<Arc<dyn Extension>>,
    pub mcp: Arc<Mcp>,
    pub lsp: Arc<lsp::Lsp>,
    /// Language servers in effect (built-ins and the `lsp` config).
    pub servers: Vec<lsp::Server>,
    /// Instruction URLs, fetched once at startup.
    remote_instructions: Vec<(String, String)>,
    user_rules: Ruleset,
    models: Mutex<HashMap<String, ModelInfo>>,
    /// `provider/model` for helper calls (condensing, summaries); the interface may change it.
    small_model: Mutex<Option<String>>,
    /// The review open in the interface, for the `review_comment` tool.
    review: Mutex<Option<review::Shared>>,
}

impl Harness {
    pub async fn new(
        cwd: &Path,
        providers: Vec<Arc<dyn Provider>>,
        extensions: Vec<Arc<dyn Extension>>,
    ) -> Arc<Harness> {
        let cwd = util::normalize(cwd);
        let root = util::git_root(&cwd).unwrap_or_else(|| cwd.clone());
        let (mut config, problems) = Config::load(&cwd, &root);
        for e in &extensions {
            e.config(&mut config);
        }
        let project_dirs = config::dirs_between(&root, &cwd);
        let skills = skill::discover(&skill::roots(&project_dirs, &config.skills.paths));
        let commands = command::discover(&project_dirs, &config);
        let agents = agent::load(&project_dirs, &config);
        let user_rules = config.permission.as_ref().map(Ruleset::from_config).unwrap_or_default();
        let mcp = Arc::new(Mcp::default());
        mcp.start(&config.mcp, &cwd);
        let remote_instructions = instructions::fetch_urls(&config.instructions).await;
        let small_model = config.small_model.clone();
        let servers = lsp::servers(config.lsp.as_ref());
        Arc::new(Harness {
            cwd,
            root,
            config,
            problems,
            providers,
            agents,
            skills: Mutex::new(skills),
            commands: Mutex::new(commands),
            extensions,
            mcp,
            lsp: Arc::default(),
            servers,
            remote_instructions,
            user_rules,
            models: Mutex::new(HashMap::new()),
            small_model: Mutex::new(small_model),
            review: Mutex::new(None),
        })
    }

    pub fn skills(&self) -> Vec<Skill> {
        self.skills.lock().unwrap().clone()
    }

    pub fn commands(&self) -> Vec<Command> {
        self.commands.lock().unwrap().clone()
    }

    /// Reads skills and prompt commands again, picking up ones written since startup.
    pub fn reload(&self) {
        let project_dirs = config::dirs_between(&self.root, &self.cwd);
        *self.skills.lock().unwrap() = skill::discover(&skill::roots(&project_dirs, &self.config.skills.paths));
        *self.commands.lock().unwrap() = command::discover(&project_dirs, &self.config);
    }

    pub fn provider(&self, id: &str) -> Result<Arc<dyn Provider>> {
        self.providers.iter().find(|p| p.id() == id).cloned().ok_or_else(|| anyhow!("unknown provider `{id}`"))
    }

    /// Sets the model for helper calls; `None` goes back to the config's `small_model`, else the
    /// session's model.
    pub fn set_small_model(&self, key: Option<String>) {
        *self.small_model.lock().unwrap() = key.or_else(|| self.config.small_model.clone());
    }

    pub fn small_model(&self) -> Option<String> {
        self.small_model.lock().unwrap().clone()
    }

    /// The model for a helper call: the small model if set, its provider known, and its
    /// context fits `needed` tokens; else the session's own model (`fallback`).
    pub async fn helper(&self, fallback: &str, needed: u64) -> Option<condense::Helper> {
        let pick = |key: &str| {
            let (pid, mid) = codeit_providers::split_key(key)?;
            let provider = self.provider(pid).ok()?;
            Some(condense::Helper { key: key.to_string(), provider, model: mid.to_string() })
        };
        if let Some(small) = self.small_model()
            && small != fallback
        {
            let fits = match self.model_info(&small).await {
                Some(info) => self.usable_context(&info).is_none_or(|u| u >= needed),
                None => false,
            };
            if fits && let Some(h) = pick(&small) {
                return Some(h);
            }
        }
        pick(fallback)
    }

    pub fn set_review(&self, review: Option<review::Shared>) {
        *self.review.lock().unwrap() = review;
    }

    pub fn review(&self) -> Option<review::Shared> {
        self.review.lock().unwrap().clone()
    }

    pub fn agent(&self, name: &str) -> Option<&Agent> {
        self.agents.iter().find(|a| a.name == name)
    }

    /// Primary agents, in the order Tab cycles through them.
    pub fn primary_agents(&self) -> Vec<&Agent> {
        self.agents.iter().filter(|a| a.primary() && !a.hidden).collect()
    }

    /// Shares a model list the interface already loaded, so limits are known without a request.
    pub fn remember_models(&self, list: &[ModelInfo]) {
        let mut m = self.models.lock().unwrap();
        for info in list {
            m.insert(info.key(), info.clone());
        }
    }

    /// What the provider says about a model (context limit, efforts), loading its list if needed.
    pub async fn model_info(&self, key: &str) -> Option<ModelInfo> {
        if let Some(i) = self.models.lock().unwrap().get(key) {
            return Some(i.clone());
        }
        let (pid, _) = codeit_providers::split_key(key)?;
        let list = self.provider(pid).ok()?.models().await.ok()?;
        self.remember_models(&list);
        self.models.lock().unwrap().get(key).cloned()
    }

    /// Instruction files that apply to the project (read fresh, so edits apply on the next turn).
    pub fn instruction_files(&self) -> Vec<PathBuf> {
        instructions::files(&self.cwd, &self.root, &self.config.instructions)
    }

    pub fn instructions(&self) -> Vec<(String, String)> {
        let mut v = instructions::read(&self.cwd, &self.instruction_files());
        v.extend(self.remote_instructions.iter().cloned());
        v
    }

    /// Folders the agent may always read: tool output, skills, the temp dir.
    fn allowed_dirs(&self) -> Vec<String> {
        let mut v = vec![
            tools::output_dir().to_string_lossy().into_owned(),
            std::env::temp_dir().to_string_lossy().into_owned(),
        ];
        v.extend(self.skills().iter().filter(|s| s.text.is_none()).map(|s| s.dir().to_string_lossy().into_owned()));
        v
    }

    /// The rules for an agent: defaults, then the user's config, the approval preset, the agent's own.
    pub fn rules(&self, agent: &Agent, approval: Approval) -> Ruleset {
        let allowed = self.allowed_dirs();
        let mut r = permission::defaults(&allowed);
        r.extend(&self.user_rules);
        if approval == Approval::Ask {
            r.extend(&permission::ask_preset());
        }
        r.extend(&agent.permission);
        for d in &allowed {
            r.push("external_directory", &format!("{}/*", d.trim_end_matches('/')), permission::Action::Allow);
        }
        r
    }

    /// The tools an agent gets with a model: built-ins, extension tools and MCP tools, minus the
    /// ones its rules deny outright.
    pub fn tools(&self, model_id: &str, rules: &Ruleset, depth: usize) -> Vec<Arc<dyn Tool>> {
        let mut all = tools::builtin(tools::uses_patch(model_id));
        for e in &self.extensions {
            all.extend(e.tools());
        }
        all.extend(self.mcp.tools());
        if self.review().is_some() {
            all.push(Arc::new(review::ReviewComment));
        }
        all.retain(|t| {
            !rules.disabled(t.permission())
                && !rules.disabled(t.name())
                // Subagents don't ask the user questions or start more subagents.
                && !(depth > 0 && matches!(t.name(), "question" | "task"))
        });
        all
    }

    /// The shell bash runs: the config's, else $SHELL if it is bash or zsh, else bash.
    pub fn shell(&self) -> String {
        if let Some(s) = &self.config.shell {
            return s.clone();
        }
        match std::env::var("SHELL") {
            Ok(s) if s.ends_with("/bash") || s.ends_with("/zsh") => s,
            _ => "bash".into(),
        }
    }

    /// (max lines, max bytes) of tool output kept in the context.
    pub fn output_limits(&self) -> (usize, usize) {
        (self.config.tool_output.max_lines.unwrap_or(400), self.config.tool_output.max_bytes.unwrap_or(24_000))
    }

    /// Tokens a request may use before the conversation is summarized.
    pub fn usable_context(&self, info: &ModelInfo) -> Option<u64> {
        let limit = info.max_input.or(info.context)?;
        let reserved = self.config.compaction.reserved.unwrap_or(20_000).min(limit / 4);
        Some(limit.saturating_sub(reserved))
    }
}
