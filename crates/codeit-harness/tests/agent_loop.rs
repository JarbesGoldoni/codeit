//! The agent loop against a scripted provider: tool calls, results, permissions, subagents, undo.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::Result;
use async_trait::async_trait;
use codeit_harness::event::{Ask, Event, Reply};
use codeit_harness::session::{Approval, Session};
use codeit_harness::{Harness, Input, Run};
use codeit_providers::{AuthStatus, ChatEvent, ChatRequest, ModelInfo, Part, Provider, Role, ToolCall, Usage};
use serde_json::{Value, json};
use tokio::sync::mpsc::UnboundedSender;
use tokio_util::sync::CancellationToken;

/// Replies with the next scripted step: tool calls (name, input) or text.
struct Scripted {
    steps: Mutex<Vec<Vec<(String, Value)>>>,
    seen: Mutex<Vec<ChatRequest>>,
}

impl Scripted {
    fn new(steps: Vec<Vec<(&str, Value)>>) -> Arc<Self> {
        let steps = steps.into_iter().map(|s| s.into_iter().map(|(n, v)| (n.to_string(), v)).collect()).collect();
        Arc::new(Self { steps: Mutex::new(steps), seen: Mutex::new(Vec::new()) })
    }
}

#[async_trait]
impl Provider for Scripted {
    fn id(&self) -> &'static str {
        "fake"
    }
    fn name(&self) -> &'static str {
        "Fake"
    }
    async fn status(&self) -> AuthStatus {
        AuthStatus::LoggedIn("test".into())
    }
    async fn models(&self) -> Result<Vec<ModelInfo>> {
        Ok(vec![ModelInfo {
            provider: "fake",
            id: "m".into(),
            name: "M".into(),
            context: Some(200_000),
            max_input: Some(200_000),
            efforts: vec![],
            default_effort: None,
            vision: true,
        }])
    }
    async fn chat(&self, req: ChatRequest, events: UnboundedSender<ChatEvent>) -> Result<()> {
        // Condensing requests (no tools, the condense prompt) get a short digest.
        if req.tools.is_empty() && req.system.as_deref().is_some_and(|s| s.contains("You condense the output")) {
            self.seen.lock().unwrap().push(req);
            let _ = events.send(ChatEvent::Text("Ran 3000 lines of build output; the build failed.".into()));
            return Ok(());
        }
        // Session titles.
        if req.system.as_deref().is_some_and(|s| s.contains("You name coding sessions")) {
            let _ = events.send(ChatEvent::Text("Fake session".into()));
            return Ok(());
        }
        // Subagents (their system prompt is the explore prompt) answer right away.
        if req.system.as_deref().is_some_and(|s| s.contains("read-only search agent")) {
            self.seen.lock().unwrap().push(req);
            let _ = events.send(ChatEvent::Text("found it in src/lib.rs".into()));
            return Ok(());
        }
        let step = {
            let mut steps = self.steps.lock().unwrap();
            if steps.is_empty() { Vec::new() } else { steps.remove(0) }
        };
        self.seen.lock().unwrap().push(req);
        for (i, (name, input)) in step.into_iter().enumerate() {
            if name == "text" {
                let _ = events.send(ChatEvent::Text(input.as_str().unwrap().into()));
            } else {
                let id = format!("c{}{i}", uuid::Uuid::new_v4().simple());
                let _ = events.send(ChatEvent::ToolStart { id: id.clone(), name: name.clone() });
                let _ = events.send(ChatEvent::ToolCall(ToolCall { id, name, input }));
            }
        }
        let _ = events.send(ChatEvent::Usage(Usage { input_tokens: 1000, output_tokens: 50, ..Default::default() }));
        Ok(())
    }
}

fn temp_project() -> PathBuf {
    let d = std::env::temp_dir().join(format!("codeit-loop-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(d.join("src")).unwrap();
    std::fs::write(d.join("src/lib.rs"), "pub fn add(a: i32, b: i32) -> i32 {\n    a - b\n}\n").unwrap();
    std::fs::write(d.join("AGENTS.md"), "Run cargo test before finishing.\n").unwrap();
    d
}

/// Runs one turn, answering permission asks with `reply`; returns the events' kinds.
async fn turn(h: &Arc<Harness>, s: &Arc<Mutex<Session>>, input: Input, reply: Reply) -> Vec<String> {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let run = Run { harness: h.clone(), session: s.clone(), events: tx, cancel: CancellationToken::new(), depth: 0 };
    let task = tokio::spawn(async move { run.start(input).await });
    let mut kinds = Vec::new();
    while let Some(ev) = rx.recv().await {
        kinds.push(match ev {
            Event::Ask(Ask::Permission(p)) => {
                let k = format!("ask:{}:{}", p.permission, p.patterns.join(","));
                let _ = p.reply.send(reply.clone());
                k
            }
            Event::ToolDone { name, error, output, .. } => {
                format!("done:{name}:{}:{}", if error { "err" } else { "ok" }, output.lines().next().unwrap_or(""))
            }
            Event::ToolOutput { text, .. } => format!("out:{text}"),
            Event::Error(e) => format!("error:{e}"),
            Event::Done => "end".into(),
            _ => continue,
        });
    }
    task.await.unwrap();
    kinds
}

async fn setup(dir: &Path, provider: Arc<Scripted>) -> (Arc<Harness>, Arc<Mutex<Session>>) {
    // Keep the tests away from the user's real data and config (set once: tests run in parallel).
    static ENV: std::sync::Once = std::sync::Once::new();
    ENV.call_once(|| {
        let home = std::env::temp_dir().join(format!("codeit-loop-home-{}", uuid::Uuid::new_v4()));
        unsafe {
            std::env::set_var("XDG_DATA_HOME", home.join("data"));
            std::env::set_var("XDG_CONFIG_HOME", home.join("config"));
            std::env::set_var("XDG_CACHE_HOME", home.join("cache"));
        }
    });
    let h = Harness::new(dir, vec![provider], vec![]).await;
    let s = Arc::new(Mutex::new(Session::new(dir, "build", Some("fake/m".into()), None)));
    (h, s)
}

#[tokio::test]
async fn reads_edits_and_finishes() {
    let dir = temp_project();
    let provider = Scripted::new(vec![
        // Two reads in parallel, and a read of a file that doesn't exist.
        vec![
            ("read", json!({"path": "src/lib.rs"})),
            ("grep", json!({"pattern": "fn add"})),
            ("read", json!({"path": "src/nope.rs"})),
        ],
        // A fuzzy edit: the model forgot the indentation.
        vec![("edit", json!({"path": "src/lib.rs", "old_string": "a - b", "new_string": "a + b"}))],
        // Reading the same unchanged range again... the file changed, so it's read again.
        vec![("read", json!({"path": "src/lib.rs"}))],
        vec![("read", json!({"path": "src/lib.rs"}))],
        vec![("text", json!("Fixed add."))],
    ]);
    let (h, s) = setup(&dir, provider.clone()).await;
    let kinds = turn(&h, &s, Input::Prompt("fix add in @src/lib.rs".into()), Reply::Once).await;

    assert_eq!(
        std::fs::read_to_string(dir.join("src/lib.rs")).unwrap(),
        "pub fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n"
    );
    assert!(kinds.contains(&"done:read:ok:1: pub fn add(a: i32, b: i32) -> i32 {".to_string()), "{kinds:?}");
    assert!(kinds.contains(&"done:grep:ok:src/lib.rs".to_string()), "{kinds:?}");
    assert!(kinds.iter().any(|k| k.starts_with("done:read:err:src/nope.rs does not exist")), "{kinds:?}");
    assert!(kinds.iter().any(|k| k.starts_with("done:read:ok:src/lib.rs is unchanged")), "{kinds:?}");
    assert_eq!(kinds.last().unwrap(), "end");

    let seen = provider.seen.lock().unwrap();
    // The first request is the user's; follow-ups are agent-initiated.
    assert!(!seen[0].agent_initiated && seen[1].agent_initiated);
    let system = seen[0].system.as_deref().unwrap();
    assert!(system.contains("Run cargo test before finishing."), "AGENTS.md is in the system prompt");
    assert!(seen[0].tools.iter().any(|t| t.name == "edit") && !seen[0].tools.iter().any(|t| t.name == "apply_patch"));
    // The @mention attached the file.
    assert!(
        seen[0].messages[0]
            .parts
            .iter()
            .any(|p| matches!(p, Part::Text { text } if text.contains("<file path=\"src/lib.rs\">")))
    );
    // Tool results come back in the order of the calls.
    let results: Vec<bool> =
        seen[1].messages[2].parts.iter().map(|p| matches!(p, Part::ToolResult { error: true, .. })).collect();
    assert_eq!(results, [false, false, true]);
    drop(seen);

    // Undo puts the file back and removes the turn.
    let u = s.lock().unwrap().undo().unwrap();
    assert_eq!(u.prompt, "fix add in @src/lib.rs");
    assert_eq!(u.restored, vec![dir.join("src/lib.rs")]);
    assert!(std::fs::read_to_string(dir.join("src/lib.rs")).unwrap().contains("a - b"));
    assert!(s.lock().unwrap().entries.is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn asks_and_stops_when_rejected() {
    let dir = temp_project();
    let provider = Scripted::new(vec![
        vec![("bash", json!({"command": "rm -rf build && ls"})), ("write", json!({"path": "x.txt", "content": "x"}))],
        vec![("text", json!("never sent"))],
    ]);
    let (h, s) = setup(&dir, provider.clone()).await;
    s.lock().unwrap().approval = Approval::Ask;
    let kinds = turn(&h, &s, Input::Prompt("clean up".into()), Reply::Reject(Some("don't delete".into()))).await;
    // `ls` is read-only, so only `rm -rf build` is asked about.
    assert!(kinds.contains(&"ask:bash:rm -rf build".to_string()), "{kinds:?}");
    assert!(kinds.iter().any(|k| k.starts_with("done:write:err:Skipped")), "{kinds:?}");
    assert_eq!(provider.seen.lock().unwrap().len(), 1, "the turn stopped after the rejection");
    assert!(!dir.join("x.txt").exists());
    let entries = &s.lock().unwrap().entries;
    let Part::ToolResult { content, .. } = &entries.last().unwrap().message.parts[0] else { panic!() };
    assert!(content.contains("don't delete"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn runs_subagents_and_bash() {
    let dir = temp_project();
    let provider = Scripted::new(vec![
        vec![
            ("task", json!({"description": "find add", "prompt": "Where is add?", "subagent_type": "explore"})),
            ("bash", json!({"command": "printf 'a\\033[31mred\\033[0m\\n'; exit 3"})),
        ],
        vec![("todo", json!({"todos": [{"content": "fix", "status": "in_progress"}]}))],
        vec![("text", json!("ok"))],
    ]);
    let (h, s) = setup(&dir, provider.clone()).await;
    let kinds = turn(&h, &s, Input::Prompt("go".into()), Reply::Once).await;
    assert!(kinds.contains(&"done:task:ok:found it in src/lib.rs".to_string()), "{kinds:?}");
    assert!(kinds.contains(&"done:bash:ok:Exit code 3".to_string()), "{kinds:?}");
    let seen = provider.seen.lock().unwrap();
    let results = &seen.iter().find(|r| r.messages.len() == 3).unwrap().messages[2];
    let Part::ToolResult { content, .. } = &results.parts[1] else { panic!() };
    assert_eq!(content, "Exit code 3\nared", "terminal escapes are removed");
    let Part::ToolResult { content, .. } = &results.parts[0] else { panic!() };
    assert!(content.starts_with("task_id: "));
    // The subagent got its own session, linked to this one, and no task tool.
    let explore = seen.iter().find(|r| r.system.as_deref().unwrap().contains("read-only search agent")).unwrap();
    assert!(explore.agent_initiated);
    assert!(!explore.tools.iter().any(|t| t.name == "task" || t.name == "edit"));
    assert_eq!(s.lock().unwrap().todos.len(), 1);
    assert_eq!(seen.last().unwrap().messages.last().unwrap().role, Role::User);
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn todo_list_is_kept_current_and_bash_streams() {
    let dir = temp_project();
    let list = |a: &str, b: &str| json!({"todos": [{"content": "one", "status": a}, {"content": "two", "status": b}]});
    let bash = |n: u32| vec![("bash", json!({"command": format!("echo hi; sleep 0.{n}")}))];
    let provider = Scripted::new(vec![
        vec![("todo", list("in_progress", "pending"))],
        bash(3),
        bash(1),
        bash(2),
        vec![("text", json!("All done."))],
        vec![("todo", list("completed", "completed"))],
        vec![("text", json!("never asked"))],
    ]);
    let (h, s) = setup(&dir, provider.clone()).await;
    let kinds = turn(&h, &s, Input::Prompt("go".into()), Reply::Always).await;
    assert!(kinds.iter().any(|k| k == "out:hi\n"), "bash output is sent while it runs: {kinds:?}");
    let seen = provider.seen.lock().unwrap();
    let last_text = |r: &ChatRequest| match r.messages.last().unwrap().parts.last().unwrap() {
        Part::Text { text } => text.clone(),
        _ => String::new(),
    };
    // Three steps without an update: the list is shown again with a reminder.
    assert!(!last_text(&seen[3]).contains("hasn't changed"));
    assert!(last_text(&seen[4]).contains("hasn't changed") && last_text(&seen[4]).contains("- [pending] two"));
    // The turn ended with items open: asked once to close them, and done once they are.
    assert!(last_text(&seen[5]).contains("ended with todo items still open"));
    assert_eq!(seen.len(), 6, "no request after the closing todo call");
    assert!(s.lock().unwrap().todos.iter().all(|t| t.status == "completed"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn plan_mode_is_read_only_and_reminded() {
    let dir = temp_project();
    let provider = Scripted::new(vec![
        vec![("write", json!({"path": "src/new.rs", "content": "x"}))],
        vec![("text", json!("Plan: ..."))],
    ]);
    let (h, s) = setup(&dir, provider.clone()).await;
    s.lock().unwrap().agent = "plan".into();
    let kinds = turn(&h, &s, Input::Prompt("plan it".into()), Reply::Once).await;
    assert!(kinds.iter().any(|k| k.starts_with("done:write:err")), "{kinds:?}");
    assert!(!dir.join("src/new.rs").exists());
    let seen = provider.seen.lock().unwrap();
    let last_prompt = &seen[0].messages[0];
    assert!(last_prompt.parts.iter().any(|p| matches!(p, Part::Text { text } if text.contains("Plan mode is on"))));
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn condenses_long_command_output_keeping_errors() {
    let dir = temp_project();
    let provider = Scripted::new(vec![
        vec![
            (
                "bash",
                json!({"command": "for i in $(seq 1 3000); do echo \"compiling crate_$i\"; done; echo 'error[E0425]: cannot find value `x`'"}),
            ),
            ("bash", json!({"command": "echo short"})),
        ],
        vec![("text", json!("done"))],
    ]);
    let (h, s) = setup(&dir, provider.clone()).await;
    turn(&h, &s, Input::Prompt("build it".into()), Reply::Once).await;
    let seen = provider.seen.lock().unwrap();
    let condense = seen.iter().find(|r| r.tools.is_empty()).expect("a condense request was made");
    assert!(condense.agent_initiated, "helper calls are not premium requests");
    assert!(condense.messages[0].text().contains("build it"), "the helper knows the task");
    let results = &seen.last().unwrap().messages[2];
    let Part::ToolResult { content, .. } = &results.parts[0] else { panic!() };
    assert!(content.starts_with("[bash: 3001 lines of output, condensed by fake/m]"), "{content}");
    assert!(content.contains("Ran 3000 lines of build output"));
    assert!(content.contains("Error lines, verbatim:\nerror[E0425]: cannot find value `x`"));
    assert!(content.contains("Full output: "));
    assert!(content.len() < 2_000, "the context gets the digest, not the log");
    let Part::ToolResult { content, .. } = &results.parts[1] else { panic!() };
    assert_eq!(content, "short", "short output is left alone");
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn review_agent_comments_on_diff_lines_only() {
    let dir = temp_project();
    let git = |args: &[&str]| {
        std::process::Command::new("git")
            .args(args)
            .current_dir(&dir)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .output()
            .unwrap()
    };
    git(&["init", "-q"]);
    git(&["add", "-A"]);
    git(&["commit", "-qm", "init"]);
    std::fs::write(dir.join("src/lib.rs"), "pub fn add(a: i32, b: i32) -> i32 {\n    a * b\n}\n").unwrap();

    let provider = Scripted::new(vec![
        vec![
            ("review_comment", json!({"path": "src/lib.rs", "line": 2, "severity": "bug", "body": "multiplies"})),
            ("review_comment", json!({"path": "src/lib.rs", "line": 40, "severity": "nit", "body": "x"})),
            ("edit", json!({"path": "src/lib.rs", "old_string": "a * b", "new_string": "a + b"})),
        ],
        vec![("text", json!("One bug."))],
    ]);
    let (h, s) = setup(&dir, provider.clone()).await;
    let review = codeit_harness::review::Review::open(&h.cwd, codeit_harness::review::Target::Working).unwrap();
    assert_eq!(review.files.len(), 1);
    let request = review.request();
    let shared = Arc::new(Mutex::new(review));
    h.set_review(Some(shared.clone()));
    s.lock().unwrap().agent = "review".into();
    let kinds = turn(&h, &s, Input::Prompt(request), Reply::Once).await;

    assert!(kinds.iter().any(|k| k.starts_with("done:review_comment:ok:Comment")), "{kinds:?}");
    assert!(
        kinds.iter().any(|k| k.starts_with("done:review_comment:err:Line 40 of src/lib.rs is not in the diff")),
        "{kinds:?}"
    );
    assert!(kinds.iter().any(|k| k.starts_with("done:edit:err")), "the review agent can't edit: {kinds:?}");
    assert!(std::fs::read_to_string(dir.join("src/lib.rs")).unwrap().contains("a * b"));
    let r = shared.lock().unwrap();
    assert_eq!(r.comments.len(), 1);
    assert_eq!((r.comments[0].line, r.comments[0].author.as_str()), (2, "codeit"));
    // The tool is only offered while a review is open, and never to other agents.
    let seen = provider.seen.lock().unwrap();
    assert!(seen[0].tools.iter().any(|t| t.name == "review_comment"));
    assert!(seen[0].messages[0].text().contains("- src/lib.rs (M, +1 -1)"));
    drop(seen);
    drop(r);
    h.set_review(None);
    let build = h.agent("build").unwrap().clone();
    let rules = h.rules(&build, Approval::Auto);
    assert!(!h.tools("m", &rules, 0).iter().any(|t| t.name() == "review_comment"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn undo_reverts_what_commands_changed_in_a_git_project() {
    let dir = temp_project();
    std::process::Command::new("git").args(["init", "-q"]).current_dir(&dir).output().unwrap();
    std::fs::write(dir.join("notes.txt"), "keep me\n").unwrap();
    let provider = Scripted::new(vec![
        vec![("bash", json!({"command": "echo changed > notes.txt && echo new > made.txt && rm AGENTS.md"}))],
        vec![("text", json!("done"))],
    ]);
    let (h, s) = setup(&dir, provider).await;
    turn(&h, &s, Input::Prompt("shuffle files".into()), Reply::Once).await;
    assert!(dir.join("made.txt").exists() && !dir.join("AGENTS.md").exists());
    let u = s.lock().unwrap().undo().unwrap();
    assert_eq!(std::fs::read_to_string(dir.join("notes.txt")).unwrap(), "keep me\n");
    assert!(!dir.join("made.txt").exists(), "files a command created are removed");
    assert!(dir.join("AGENTS.md").exists(), "files a command deleted come back");
    assert_eq!(u.restored.len(), 3);
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn reads_and_attaches_images() {
    let dir = temp_project();
    // A 1x1 PNG.
    let png: [u8; 67] = [
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 0x0D, 0x49, 0x48, 0x44, 0x52, 0, 0, 0, 1, 0, 0, 0, 1,
        8, 6, 0, 0, 0, 0x1F, 0x15, 0xC4, 0x89, 0, 0, 0, 0x0A, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0, 1, 0, 0, 5,
        0, 1, 0x0D, 0x0A, 0x2D, 0xB4, 0, 0, 0, 0, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
    ];
    std::fs::write(dir.join("shot.png"), png).unwrap();
    let provider = Scripted::new(vec![vec![("read", json!({"path": "shot.png"}))], vec![("text", json!("A pixel."))]]);
    let (h, s) = setup(&dir, provider.clone()).await;
    turn(&h, &s, Input::Prompt("what's in @shot.png?".into()), Reply::Once).await;
    let seen = provider.seen.lock().unwrap();
    let images = |m: &codeit_providers::Message| m.images().count();
    assert_eq!(images(&seen[0].messages[0]), 1, "the @mention attached the image");
    let results = seen[1].messages.last().unwrap();
    assert!(matches!(results.parts[0], Part::ToolResult { .. }));
    assert_eq!(images(results), 1, "read returned the image after the result");
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn language_server_answers_and_reports_errors_after_edits() {
    // Uses clangd when it is installed; skipped otherwise.
    if std::process::Command::new("clangd").arg("--version").output().is_err() {
        eprintln!("clangd not installed: skipped");
        return;
    }
    let dir = temp_project();
    std::fs::write(dir.join("compile_flags.txt"), "-std=c11\n").unwrap();
    std::fs::write(dir.join("m.c"), "int add(int a, int b) { return a + b; }\nint main(void) { return add(1, 2); }\n")
        .unwrap();
    let provider = Scripted::new(vec![
        vec![("lsp", json!({"operation": "goToDefinition", "path": "m.c", "line": 2, "character": 25}))],
        vec![("read", json!({"path": "m.c"}))],
        vec![("edit", json!({"path": "m.c", "old_string": "return add(1, 2);", "new_string": "return sub(1, 2);"}))],
        vec![("text", json!("done"))],
    ]);
    let (h, s) = setup(&dir, provider.clone()).await;
    turn(&h, &s, Input::Prompt("go".into()), Reply::Once).await;
    let seen = provider.seen.lock().unwrap();
    let result = |i: usize| match &seen[i].messages.last().unwrap().parts[0] {
        Part::ToolResult { content, .. } => content.clone(),
        _ => String::new(),
    };
    assert_eq!(result(1), "m.c:1:5  int add(int a, int b) { return a + b; }");
    let after_edit = result(3);
    assert!(after_edit.contains("<diagnostics file=\"m.c\">") && after_edit.contains("sub"), "{after_edit}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn picks_up_commands_and_skills_written_after_startup() {
    let dir = temp_project();
    let (h, _) = setup(&dir, Scripted::new(vec![])).await;
    assert!(h.skills().iter().any(|s| s.name == "customize-codeit"));
    assert!(!h.commands().iter().any(|c| c.name == "hello"));
    std::fs::create_dir_all(dir.join(".codeit/commands")).unwrap();
    std::fs::write(dir.join(".codeit/commands/hello.md"), "---\ndescription: greet\n---\nSay hi to $1.\n").unwrap();
    std::fs::create_dir_all(dir.join(".codeit/skills/notes")).unwrap();
    std::fs::write(
        dir.join(".codeit/skills/notes/SKILL.md"),
        "---\nname: notes\ndescription: take notes\n---\nWrite.\n",
    )
    .unwrap();
    h.reload();
    let hello = h.commands().into_iter().find(|c| c.name == "hello").expect("command loaded");
    assert_eq!(hello.description, "greet");
    assert!(h.skills().iter().any(|s| s.name == "notes"));
    assert!(h.skills().iter().any(|s| s.name == "customize-codeit"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn the_model_names_the_session_once() {
    let dir = temp_project();
    let provider = Scripted::new(vec![vec![("text", json!("Hi."))], vec![("text", json!("Again."))]]);
    let (h, s) = setup(&dir, provider.clone()).await;
    turn(&h, &s, Input::Prompt("please look at the add function in lib".into()), Reply::Once).await;
    for _ in 0..50 {
        if !s.lock().unwrap().title.is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert_eq!(s.lock().unwrap().title, "Fake session");
    s.lock().unwrap().title = "Mine".into();
    turn(&h, &s, Input::Prompt("and now sub".into()), Reply::Once).await;
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert_eq!(s.lock().unwrap().title, "Mine");
}
