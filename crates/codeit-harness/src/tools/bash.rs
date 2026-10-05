//! bash: runs a command in the project's shell, with a timeout, its own process group (so an
//! interrupt stops the whole command), and output cleaned of terminal escapes.

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Result, bail};
use async_trait::async_trait;
use codeit_providers::ToolSpec;
use serde_json::{Value, json};
use tokio::io::AsyncReadExt;

use super::{Ctx, Output, Tool, arg, opt_u64, spec};
use crate::permission::{command_prefix, split_commands};
use crate::{Harness, util};

const DEFAULT_TIMEOUT_MS: u64 = 120_000;
const MAX_TIMEOUT_MS: u64 = 600_000;

pub struct Bash;

#[async_trait]
impl Tool for Bash {
    fn name(&self) -> &str {
        "bash"
    }

    fn spec(&self, h: &Harness) -> ToolSpec {
        spec(
            "bash",
            &format!(
                "Run a shell command ({}) in the working directory and return its output (stdout and stderr together) \
and exit code. Use it for builds, tests, git and other programs, not for reading, searching or editing files. \
Each call starts a new shell: use `workdir` instead of `cd`. Commands time out after {}s by default (`timeout` in ms, \
up to 600000). Long output keeps its start and end and is saved to a file you can grep. Don't run interactive \
commands or ones that never exit (servers, watchers) unless backgrounded with `&` and redirected to a file.",
                h.shell(),
                DEFAULT_TIMEOUT_MS / 1000
            ),
            json!({
                "type": "object",
                "properties": {
                    "command": { "type": "string" },
                    "workdir": { "type": "string", "description": "Directory to run in (default: the working directory)" },
                    "timeout": { "type": "integer", "description": "Timeout in milliseconds" },
                    "description": { "type": "string", "description": "What the command does, in 5-10 words" }
                },
                "required": ["command"]
            }),
        )
    }

    fn title(&self, input: &Value, _: &Path) -> String {
        input["command"].as_str().unwrap_or_default().to_string()
    }

    async fn run(&self, input: Value, ctx: &Ctx) -> Result<Output> {
        let command = arg(&input, "command")?;
        let workdir = match input["workdir"].as_str() {
            Some(w) => ctx.resolve(w),
            None => ctx.cwd().to_path_buf(),
        };
        if !workdir.is_dir() {
            bail!("workdir {} is not a directory.", workdir.display());
        }
        if !workdir.starts_with(&ctx.harness.root) && !workdir.starts_with(ctx.cwd()) {
            let d = workdir.to_string_lossy().into_owned();
            ctx.ask(
                "external_directory",
                std::slice::from_ref(&d),
                &[format!("{d}/*")],
                format!("run in {d}"),
                Some(command.to_string()),
            )
            .await?;
        }
        let parts = split_commands(command);
        let always: Vec<String> = parts.iter().map(|p| format!("{} *", command_prefix(p))).collect();
        ctx.ask("bash", &parts, &always, format!("run `{command}`"), Some(command.to_string())).await?;

        let timeout = opt_u64(&input, "timeout").unwrap_or(DEFAULT_TIMEOUT_MS).clamp(1000, MAX_TIMEOUT_MS);
        let mut env = std::collections::BTreeMap::new();
        for e in &ctx.harness.extensions {
            e.shell_env(&mut env);
        }
        let (code, output, timed_out) = run(&ctx.harness.shell(), command, &workdir, &env, timeout, ctx).await?;
        let output = util::clean_terminal(&output);
        let output = output.trim_end();
        let mut text = ctx.limit(if output.is_empty() { "(no output)" } else { output });
        if timed_out {
            text.push_str(&format!(
                "\n\n(Stopped after {}s. If it needs longer and isn't waiting for input, retry with a larger timeout.)",
                timeout / 1000
            ));
        } else if ctx.cancel.is_cancelled() {
            text.push_str("\n\n(Interrupted by the user.)");
        } else if code != 0 {
            text = format!("Exit code {code}\n{text}");
        }
        Ok(Output::text(text))
    }
}

/// Runs `command` and returns (exit code, combined output, timed out).
pub async fn run(
    shell: &str,
    command: &str,
    dir: &Path,
    env: &std::collections::BTreeMap<String, String>,
    timeout_ms: u64,
    ctx: &Ctx,
) -> Result<(i32, String, bool)> {
    let mut cmd = tokio::process::Command::new(shell);
    cmd.arg("-c")
        .arg(command)
        .current_dir(dir)
        .envs(env)
        .env("TERM", "dumb")
        .env("NO_COLOR", "1")
        .env("GIT_PAGER", "cat")
        .env("PAGER", "cat")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    cmd.process_group(0);
    let mut child = cmd.spawn()?;
    let pid = child.id();
    let mut stdout = child.stdout.take().expect("piped");
    let mut stderr = child.stderr.take().expect("piped");
    // Both streams into one buffer, in arrival order.
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();
    let tx2 = tx.clone();
    let out_task = tokio::spawn(async move {
        let mut buf = [0u8; 8192];
        while let Ok(n) = stdout.read(&mut buf).await {
            if n == 0 || tx.send(buf[..n].to_vec()).is_err() {
                break;
            }
        }
    });
    let err_task = tokio::spawn(async move {
        let mut buf = [0u8; 8192];
        while let Ok(n) = stderr.read(&mut buf).await {
            if n == 0 || tx2.send(buf[..n].to_vec()).is_err() {
                break;
            }
        }
    });

    let mut output: Vec<u8> = Vec::new();
    let deadline = tokio::time::sleep(Duration::from_millis(timeout_ms));
    tokio::pin!(deadline);
    let mut timed_out = false;
    let status = loop {
        tokio::select! {
            Some(chunk) = rx.recv() => {
                // Keep memory bounded on runaway output: the middle is dropped later anyway.
                if output.len() < 20_000_000 {
                    output.extend_from_slice(&chunk);
                }
            }
            status = child.wait() => break status.ok(),
            _ = &mut deadline => {
                timed_out = true;
                kill_group(pid);
                break child.wait().await.ok();
            }
            _ = ctx.cancel.cancelled() => {
                kill_group(pid);
                break child.wait().await.ok();
            }
        }
    };
    // Drain what's left (the pipes close when the process group is gone).
    let _ = tokio::time::timeout(Duration::from_millis(500), async {
        let _ = out_task.await;
        let _ = err_task.await;
    })
    .await;
    while let Ok(chunk) = rx.try_recv() {
        output.extend_from_slice(&chunk);
    }
    let code = status.and_then(|s| s.code()).unwrap_or(-1);
    Ok((code, String::from_utf8_lossy(&output).into_owned(), timed_out))
}

fn kill_group(pid: Option<u32>) {
    #[cfg(unix)]
    if let Some(pid) = pid {
        // SAFETY: plain syscall; a negative pid addresses the process group we created.
        unsafe {
            libc::kill(-(pid as i32), libc::SIGKILL);
        }
    }
    #[cfg(not(unix))]
    let _ = pid;
}
