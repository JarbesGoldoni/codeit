//! Command line: opens the TUI, or runs one of the non-interactive commands.

use std::io::Write;
use std::sync::{Arc, Mutex};

use anyhow::{Result, anyhow, bail};
use codeit_harness::event::{Ask, Event, Reply};
use codeit_harness::session::Session;
use codeit_harness::{Harness, Input, Run};
use codeit_providers::AuthStatus;
use tokio_util::sync::CancellationToken;

use crate::state::State;

pub const USAGE: &str = "\
codeit: a terminal coding agent

Usage:
  codeit                                   Open the TUI
  codeit -c, --continue                    Open the TUI on the last session in this folder
  codeit -s, --session ID                  Open the TUI on a session
  codeit run [options] PROMPT...           Run the agent on one prompt and print what it does
      -m, --model PROVIDER/MODEL         (default: the last model used)
      -e, --effort LEVEL
      --agent NAME                       build (default) or plan
      -c, --continue                     continue the last session in this folder
      -y, --yes                          approve everything that would ask (default: refuse)
  codeit sessions                          List the sessions started in this folder
  codeit status                            Show which providers are logged in
  codeit models                            List the models each provider offers
  codeit login [PROVIDER]                  Log in to a provider (asks which, and how)
  codeit logout PROVIDER                   Remove the login codeit saved for a provider
  codeit providers                         List every provider codeit can log in to

Providers are opencode's: GitHub Copilot, OpenAI (ChatGPT Plus/Pro or API key), Anthropic,
Google, OpenRouter and the rest of the models.dev catalog, plus Z.ai's GLM Coding Plan.
codeit also uses your opencode logins.
Debug log: set CODEIT_DEBUG=1, then read ~/.cache/codeit/debug.log";

fn last_session() -> Result<Session> {
    let cwd = std::env::current_dir()?;
    let meta =
        Session::list(Some(&cwd)).into_iter().next().ok_or_else(|| anyhow!("no saved session in this folder"))?;
    Session::load(&meta.id)
}

pub async fn run(args: Vec<String>) -> Result<()> {
    let providers = crate::providers();
    let a: Vec<&str> = args.iter().map(String::as_str).collect();
    match a.as_slice() {
        [] => crate::tui::run(None).await?,
        ["-c" | "--continue"] => crate::tui::run(Some(last_session()?)).await?,
        ["-s" | "--session", id] => crate::tui::run(Some(Session::load(id)?)).await?,
        ["status"] => {
            let mut out = 0;
            for p in &providers {
                match p.status().await {
                    AuthStatus::LoggedIn(s) => println!("{:<24} logged in: {s}", p.name()),
                    AuthStatus::LoggedOut(_) => out += 1,
                }
            }
            println!("({out} more providers not logged in: `codeit providers` lists them, `codeit login` adds one)");
        }
        ["providers"] => {
            for c in crate::login::choices(&providers).await {
                let mark = if c.logged_in { "●" } else { " " };
                println!("{mark} {:<28} {:<36} {}", c.id, c.name, c.hint);
            }
        }
        ["models"] => {
            for p in &providers {
                if matches!(p.status().await, AuthStatus::LoggedOut(_)) {
                    continue;
                }
                println!("{} ({})", p.name(), p.id());
                match p.models().await {
                    Ok(models) => {
                        for m in models {
                            let efforts = if m.efforts.is_empty() {
                                String::new()
                            } else {
                                format!("  effort: {}", m.efforts.join("/"))
                            };
                            println!("  {:<40} {}{efforts}", m.key(), m.name);
                        }
                    }
                    Err(e) => println!("  error: {e:#}"),
                }
            }
        }
        ["sessions"] => {
            let cwd = std::env::current_dir()?;
            let list = Session::list(Some(&cwd));
            if list.is_empty() {
                println!("No sessions in this folder.");
            }
            for m in list {
                println!("{}  {}", m.id, m.title);
            }
        }
        ["login", rest @ ..] => login(&providers, rest.first().copied()).await?,
        ["logout", id] => {
            if crate::login::logout(id)? {
                println!("Removed codeit's login for {id}.");
            } else {
                println!(
                    "codeit had no login saved for {id}. (An opencode login or an environment variable, if any, still apply.)"
                );
            }
        }
        ["run", rest @ ..] => run_once(rest).await?,
        ["help" | "-h" | "--help"] => println!("{USAGE}"),
        ["--version" | "-V"] => println!("codeit {}", env!("CARGO_PKG_VERSION")),
        _ => bail!("unknown command\n\n{USAGE}"),
    }
    Ok(())
}

fn ask(prompt: &str) -> Result<String> {
    eprint!("{prompt}");
    let _ = std::io::stderr().flush();
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    Ok(line.trim().to_string())
}

/// Reads a line without showing it (for keys).
fn ask_secret(prompt: &str) -> Result<String> {
    eprint!("{prompt}");
    let _ = std::io::stderr().flush();
    let stty = |arg: &str| std::process::Command::new("stty").arg(arg).stdin(std::process::Stdio::inherit()).status();
    let quiet = stty("-echo").is_ok_and(|s| s.success());
    let mut line = String::new();
    let read = std::io::stdin().read_line(&mut line);
    if quiet {
        let _ = stty("echo");
    }
    eprintln!();
    read?;
    Ok(line.trim().to_string())
}

/// `codeit login [provider]`: picks the provider and the way to log in, then logs in.
async fn login(providers: &[std::sync::Arc<dyn codeit_providers::Provider>], id: Option<&str>) -> Result<()> {
    use crate::login::Method;
    let all = crate::login::choices(providers).await;
    let id = match id {
        Some(id) => id.to_string(),
        None => {
            for (i, c) in all.iter().take(12).enumerate() {
                let mark = if c.logged_in { "●" } else { " " };
                eprintln!("{:>3}. {mark} {:<26} {}", i + 1, c.name, c.hint);
            }
            eprintln!("     … and {} more: `codeit providers` lists them all.", all.len().saturating_sub(12));
            let answer = ask("Provider (number or id): ")?;
            match answer.parse::<usize>() {
                Ok(n) if (1..=all.len()).contains(&n) => all[n - 1].id.clone(),
                _ => answer,
            }
        }
    };
    let Some(c) = all.iter().find(|c| c.id == id || c.name.eq_ignore_ascii_case(&id)) else {
        bail!("unknown provider `{id}`; `codeit providers` lists them");
    };
    let method = if c.methods.len() == 1 {
        c.methods[0]
    } else {
        for (i, m) in c.methods.iter().enumerate() {
            eprintln!("{:>3}. {}", i + 1, m.label());
        }
        let n: usize = ask("How: ")?.parse().unwrap_or(1);
        c.methods[n.clamp(1, c.methods.len()) - 1]
    };
    match method {
        Method::ApiKey => {
            if let Some(doc) = &c.doc {
                eprintln!("Get a key at {doc}");
            }
            if !c.env.is_empty() {
                eprintln!("(Setting {} works too.)", c.env.join(" or "));
            }
            let key = ask_secret(&format!("{} API key: ", c.name))?;
            crate::login::save_key(&c.id, &key)?;
            println!("Saved. `codeit models` lists its models.");
        }
        Method::Own => println!("{}", c.how),
        _ => {
            let enterprise = if method == Method::CopilotEnterprise {
                Some(ask("GitHub Enterprise domain (company.ghe.com): ")?)
            } else {
                None
            };
            let started = crate::login::start(method, enterprise.as_deref()).await?;
            if started.code.is_empty() {
                println!("Opening {} …", started.url);
                codeit_harness::util::open_url(&started.url);
            } else {
                println!("Open {} and enter the code: {}", started.url, started.code);
            }
            println!("Waiting for you to approve the login...");
            started.wait.await?;
            println!("Logged in to {}.", c.name);
        }
    }
    Ok(())
}

/// `codeit run`: one turn, printed as it happens. Text goes to stdout, the rest to stderr.
async fn run_once(args: &[&str]) -> Result<()> {
    let state = State::load();
    let mut model = state.model.clone();
    let mut effort = None;
    let mut agent = "build".to_string();
    let mut yes = false;
    let mut resume = false;
    let mut prompt = Vec::new();
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match *arg {
            "-m" | "--model" => model = it.next().map(|s| s.to_string()),
            "-e" | "--effort" => effort = it.next().map(|s| s.to_string()),
            "--agent" => agent = it.next().map(|s| s.to_string()).unwrap_or(agent),
            "-y" | "--yes" => yes = true,
            "-c" | "--continue" => resume = true,
            other => prompt.push(other),
        }
    }
    let model = model.ok_or_else(|| anyhow!("no model: pass -m provider/model (see `codeit models`)"))?;
    codeit_providers::split_key(&model).ok_or_else(|| anyhow!("model must look like provider/model"))?;
    if prompt.is_empty() {
        bail!("no prompt given");
    }
    let cwd = std::env::current_dir()?;
    let harness = Harness::new(&cwd, crate::providers(), crate::extensions()).await;
    for p in &harness.problems {
        eprintln!("config: {p}");
    }
    if harness.agent(&agent).is_none_or(|a| !a.primary()) {
        bail!("unknown agent `{agent}`");
    }
    let mut session = if resume { last_session()? } else { Session::new(&cwd, &agent, None, None) };
    session.model = Some(model);
    session.effort = effort;
    session.agent = agent;
    session.approval = state.approval;
    let session = Arc::new(Mutex::new(session));

    let text = prompt.join(" ");
    let input = match text.strip_prefix('/').and_then(|t| {
        let (name, args) = t.split_once(' ').unwrap_or((t, ""));
        harness.commands().iter().any(|c| c.name == name).then(|| (name.to_string(), args.to_string()))
    }) {
        Some((name, args)) => Input::Command { name, args },
        None => Input::Prompt(text),
    };

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let cancel = CancellationToken::new();
    let run = Run { harness: harness.clone(), session: session.clone(), events: tx, cancel: cancel.clone(), depth: 0 };
    let task = tokio::spawn(async move { run.start(input).await });
    let ctrl_c = cancel.clone();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            ctrl_c.cancel();
        }
    });

    let mut out = std::io::stdout();
    let mut failed = false;
    // Whether the cursor is at the start of a line (stdout and stderr share the terminal).
    let mut at_line_start = true;
    let mut total = codeit_providers::Usage::default();
    let mut reasoning = false;
    let dim = |s: &str| format!("\x1b[2m{s}\x1b[0m");
    let newline = |at: &mut bool| {
        if !*at {
            eprintln!();
            *at = true;
        }
    };
    while let Some(ev) = rx.recv().await {
        match ev {
            Event::Text(t) => {
                if reasoning {
                    newline(&mut at_line_start);
                    reasoning = false;
                }
                print!("{t}");
                let _ = out.flush();
                at_line_start = t.ends_with('\n');
            }
            Event::Reasoning(t) => {
                reasoning = true;
                eprint!("{}", dim(&t));
                at_line_start = t.ends_with('\n');
            }
            Event::ToolStart { name, title, .. } => {
                newline(&mut at_line_start);
                reasoning = false;
                eprintln!("{}", dim(&format!("→ {name} {title}")));
            }
            Event::ToolDone { error: true, output, .. } => {
                eprintln!("{}", dim(&format!("  ✗ {}", output.lines().next().unwrap_or_default())))
            }
            Event::Ask(Ask::Permission(p)) => {
                let reply = if yes {
                    Reply::Once
                } else {
                    Reply::Reject(Some("Not allowed in a non-interactive run; continue without it.".into()))
                };
                eprintln!(
                    "{}",
                    dim(&format!("  {} {} ({})", if yes { "approved" } else { "refused" }, p.title, p.permission))
                );
                let _ = p.reply.send(reply);
            }
            Event::Ask(Ask::Question(q)) => {
                eprintln!("{}", dim("  (a question was answered automatically: nobody is here to answer it)"));
                let answer =
                    "(No one can answer in a non-interactive run; use your best judgment and say what you chose.)";
                let _ = q.reply.send(Some(q.questions.iter().map(|_| vec![answer.to_string()]).collect()));
            }
            Event::Todos(todos) => {
                for t in todos {
                    let mark = match t.status.as_str() {
                        "completed" => "✔",
                        "in_progress" => "◼",
                        _ => "□",
                    };
                    eprintln!("{}", dim(&format!("  {mark} {}", t.content)));
                }
            }
            Event::Notice(n) => {
                newline(&mut at_line_start);
                eprintln!("{}", dim(&n));
            }
            Event::Error(e) => {
                newline(&mut at_line_start);
                eprintln!("error: {e}");
                failed = true;
            }
            Event::Step { usage, .. } => {
                total.input_tokens = usage.input_tokens;
                total.output_tokens += usage.output_tokens;
                if let Some(c) = usage.credits {
                    total.credits = Some(total.credits.unwrap_or(0.0) + c);
                }
            }
            Event::Done => break,
            _ => {}
        }
    }
    newline(&mut at_line_start);
    task.await?;
    let id = session.lock().unwrap().id.clone();
    let credits = total.credits.map(|c| format!(", {c:.3} credits")).unwrap_or_default();
    eprintln!("{}", dim(&format!("[{} in, {} out{credits}] session {id}", total.input_tokens, total.output_tokens)));
    if failed {
        std::process::exit(1);
    }
    Ok(())
}
