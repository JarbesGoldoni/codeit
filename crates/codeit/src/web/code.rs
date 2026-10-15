//! The code viewer's API (`POST /api/code`): the projects in the folder codeit started in, their
//! files (in the working tree or at any commit), branches, commits and diffs, and what a code
//! review of the current branch covers. Read-only: nothing here changes a file or a repository.
//!
//! Every request names a project by its path relative to that folder (`""` is the folder itself,
//! or its git root); paths outside it are refused.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;

use anyhow::{Context, Result, anyhow, bail};
use axum::Json;
use axum::extract::State as AxState;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use codeit_harness::{project, util};
use serde::Deserialize;
use serde_json::{Value, json};

use super::Server;

/// Files larger than this aren't shown.
const MAX_FILE: usize = 2 << 20;
/// Diffs are cut here.
const MAX_DIFF: usize = 6 << 20;
/// git's empty tree, the "parent" of a first commit.
const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Req {
    Projects,
    Tree {
        root: String,
        rev: Option<String>,
    },
    File {
        root: String,
        path: String,
        rev: Option<String>,
    },
    Branches {
        root: String,
    },
    Log {
        root: String,
        rev: Option<String>,
        #[serde(default)]
        skip: usize,
    },
    /// `to` is a commit, or `None` for the uncommitted changes; `from` defaults to `to`'s parent
    /// (or `HEAD` for the uncommitted changes).
    Diff {
        root: String,
        from: Option<String>,
        to: Option<String>,
    },
    /// The current branch's own commits and where its changes start.
    Review {
        root: String,
    },
    Find {
        root: String,
        rev: Option<String>,
        name: String,
    },
}

pub async fn api(AxState(s): AxState<Arc<Server>>, Json(req): Json<Req>) -> Response {
    let s2 = s.clone();
    match tokio::task::spawn_blocking(move || handle(&s2, req)).await {
        Ok(Ok(v)) => Json(v).into_response(),
        Ok(Err(e)) => (StatusCode::BAD_REQUEST, Json(json!({"error": format!("{e:#}")}))).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))).into_response(),
    }
}

fn handle(s: &Server, req: Req) -> Result<Value> {
    let h = &s.harness;
    Ok(match req {
        Req::Projects => json!({"projects": projects(&h.cwd, &h.root)}),
        Req::Tree { root, rev } => {
            let root = s.project_dir(&root)?;
            tree(&root, rev.as_deref())?
        }
        Req::File { root, path, rev } => {
            let root = s.project_dir(&root)?;
            file(&root, &path, rev.as_deref())?
        }
        Req::Branches { root } => branches(&s.project_dir(&root)?)?,
        Req::Log { root, rev, skip } => log(&s.project_dir(&root)?, rev.as_deref(), skip)?,
        Req::Diff { root, from, to } => diff(&s.project_dir(&root)?, from.as_deref(), to.as_deref())?,
        Req::Review { root } => review(&s.project_dir(&root)?)?,
        Req::Find { root, rev, name } => find(&s.project_dir(&root)?, rev.as_deref(), &name)?,
    })
}

impl Server {
    /// A project's folder from its path relative to where codeit started.
    pub(super) fn project_dir(&self, rel: &str) -> Result<PathBuf> {
        let h = &self.harness;
        if rel.is_empty() {
            return Ok(h.root.clone());
        }
        let dir = util::resolve(&h.cwd, rel);
        if !dir.starts_with(&h.cwd) && !dir.starts_with(&h.root) {
            bail!("{rel} is outside {}", h.cwd.display());
        }
        if !dir.is_dir() {
            bail!("{rel} isn't a folder");
        }
        Ok(dir)
    }

    /// A folder as the app names it: relative to where codeit started (`""` for the git root
    /// above it, or itself).
    pub(super) fn project_rel(&self, dir: &Path) -> String {
        let h = &self.harness;
        if dir == h.root || dir == h.cwd {
            return String::new();
        }
        dir.strip_prefix(&h.cwd).map(|r| r.to_string_lossy().into_owned()).unwrap_or_default()
    }
}

fn projects(cwd: &Path, root: &Path) -> Value {
    if util::git_root(cwd).is_some() {
        let name = root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        return json!([{"path": "", "name": name, "branch": project::branch(root), "git": true}]);
    }
    let mut v: Vec<Value> = project::list(cwd)
        .into_iter()
        .map(|p| {
            json!({
                "path": util::display(cwd, &p.path),
                "name": p.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
                "branch": p.branch,
                "git": true,
            })
        })
        .collect();
    let name = cwd.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    v.insert(0, json!({"path": "", "name": name, "branch": null, "git": false}));
    json!(v)
}

fn git(dir: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let out = Command::new("git")
        .args(["-c", "core.quotepath=off", "-c", "color.ui=never"])
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .output()
        .context("couldn't run git")?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        bail!("git {}: {}", args.first().unwrap_or(&""), err.trim());
    }
    Ok(out.stdout)
}

fn git_text(dir: &Path, args: &[&str]) -> Result<String> {
    Ok(String::from_utf8_lossy(&git(dir, args)?).into_owned())
}

fn is_git(dir: &Path) -> bool {
    util::git_root(dir).is_some()
}

/// A revision from the app: a hash, branch or tag name, maybe with `^` or `~N`. Never an option.
fn check_rev(rev: &str) -> Result<()> {
    let ok = !rev.is_empty()
        && !rev.starts_with('-')
        && !rev.contains("..")
        && rev.chars().all(|c| c.is_ascii_alphanumeric() || "/._-~^@{}+".contains(c));
    if ok { Ok(()) } else { Err(anyhow!("bad revision `{rev}`")) }
}

/// A path inside the project, from the app.
fn check_path(path: &str) -> Result<()> {
    let p = Path::new(path);
    if path.is_empty() || p.is_absolute() || p.components().any(|c| !matches!(c, std::path::Component::Normal(_))) {
        bail!("bad path `{path}`");
    }
    Ok(())
}

fn tree(root: &Path, rev: Option<&str>) -> Result<Value> {
    if let Some(rev) = rev {
        check_rev(rev)?;
        let out = git(root, &["ls-tree", "-r", "-z", "--name-only", rev])?;
        let files: Vec<String> = split0(&out).collect();
        return Ok(json!({"files": files, "status": {}}));
    }
    if !is_git(root) {
        let mut files = Vec::new();
        walk(root, root, &mut files);
        files.sort();
        return Ok(json!({"files": files, "status": {}}));
    }
    let out = git(root, &["ls-files", "-z", "--cached", "--others", "--exclude-standard"])?;
    let mut files: Vec<String> = split0(&out).filter(|f| root.join(f).exists()).collect();
    files.sort();
    files.dedup();
    // What changed since the last commit, to mark in the tree.
    let mut status = BTreeMap::new();
    let out =
        git(root, &["status", "--porcelain=v1", "-z", "--untracked-files=all", "--no-renames"]).unwrap_or_default();
    for entry in split0(&out) {
        if entry.len() < 4 {
            continue;
        }
        let (code, path) = entry.split_at(3);
        let code = code.trim();
        let mark = match code {
            "??" => "added",
            c if c.contains('A') => "added",
            c if c.contains('D') => "deleted",
            _ => "modified",
        };
        status.insert(path.to_string(), mark);
    }
    Ok(json!({"files": files, "status": status}))
}

fn split0(out: &[u8]) -> impl Iterator<Item = String> + '_ {
    out.split(|b| *b == 0).filter(|s| !s.is_empty()).map(|s| String::from_utf8_lossy(s).into_owned())
}

/// Files under a folder that isn't a git repository (hidden folders and build output skipped).
fn walk(root: &Path, dir: &Path, out: &mut Vec<String>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        if out.len() >= 20_000 {
            return;
        }
        let name = e.file_name().to_string_lossy().into_owned();
        let Ok(t) = e.file_type() else { continue };
        if t.is_dir() {
            if !name.starts_with('.') && name != "node_modules" && name != "target" {
                walk(root, &e.path(), out);
            }
        } else if t.is_file() {
            out.push(util::display(root, &e.path()));
        }
    }
}

fn file(root: &Path, path: &str, rev: Option<&str>) -> Result<Value> {
    check_path(path)?;
    let bytes = match rev {
        Some(rev) => {
            check_rev(rev)?;
            git(root, &["show", &format!("{rev}:{path}")])?
        }
        None => {
            let p = root.join(path);
            // Through a link, still inside the project.
            let real = p.canonicalize().with_context(|| format!("{path} doesn't exist"))?;
            if !real.starts_with(root.canonicalize()?) {
                bail!("{path} points outside the project");
            }
            if std::fs::metadata(&real)?.len() as usize > MAX_FILE {
                return Ok(json!({"path": path, "text": null, "too_big": true}));
            }
            std::fs::read(&real)?
        }
    };
    if bytes.len() > MAX_FILE {
        return Ok(json!({"path": path, "text": null, "too_big": true}));
    }
    if bytes.iter().take(8192).any(|b| *b == 0) {
        return Ok(json!({"path": path, "text": null, "binary": true, "size": bytes.len()}));
    }
    Ok(json!({"path": path, "text": String::from_utf8_lossy(&bytes)}))
}

fn branches(root: &Path) -> Result<Value> {
    if !is_git(root) {
        return Ok(json!({"current": null, "branches": []}));
    }
    let current = git_text(root, &["rev-parse", "--abbrev-ref", "HEAD"]).map(|s| s.trim().to_string()).ok();
    let out = git_text(
        root,
        &[
            "for-each-ref",
            "--sort=-committerdate",
            "--count=300",
            "--format=%(refname)%1f%(refname:short)%1f%(objectname)%1f%(committerdate:unix)%1f%(subject)%1f%(upstream:short)%1f%(upstream:track)",
            "refs/heads",
            "refs/remotes",
        ],
    )?;
    let list: Vec<Value> = out
        .lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split('\x1f').collect();
            if f.len() < 7 || f[0].ends_with("/HEAD") {
                return None;
            }
            Some(json!({
                "name": f[1],
                "remote": f[0].starts_with("refs/remotes/"),
                "hash": f[2],
                "date": f[3].parse::<u64>().unwrap_or(0),
                "subject": f[4],
                "upstream": (!f[5].is_empty()).then_some(f[5]),
                "track": f[6].trim_matches(['[', ']']),
            }))
        })
        .collect();
    Ok(json!({"current": current, "branches": list}))
}

const PAGE: usize = 100;

fn log(root: &Path, rev: Option<&str>, skip: usize) -> Result<Value> {
    if !is_git(root) {
        return Ok(json!({"commits": [], "more": false, "uncommitted": 0}));
    }
    let rev = rev.unwrap_or("HEAD");
    check_rev(rev)?;
    let (commits, more) = commits(root, &[rev], skip, false);
    let uncommitted = if skip == 0 { uncommitted(root) } else { 0 };
    Ok(json!({"commits": commits, "more": more, "uncommitted": uncommitted}))
}

/// A page of `git log` for these revisions, newest first, with each commit's files if asked.
fn commits(root: &Path, revs: &[&str], skip: usize, files: bool) -> (Vec<Value>, bool) {
    let (n, s) = ((PAGE + 1).to_string(), skip.to_string());
    let mut args = vec![
        "log",
        "--max-count",
        &n,
        "--skip",
        &s,
        "--format=%x1e%H%x1f%P%x1f%an%x1f%ae%x1f%at%x1f%D%x1f%s%x1f%b%x1f",
    ];
    if files {
        args.push("--name-only");
    }
    args.extend(revs);
    args.push("--");
    let out = git_text(root, &args).unwrap_or_default();
    let mut list: Vec<Value> = out
        .split('\x1e')
        .filter_map(|rec| {
            let f: Vec<&str> = rec.splitn(9, '\x1f').collect();
            if f.len() < 9 {
                return None;
            }
            let mut c = json!({
                "hash": f[0].trim(),
                "parents": f[1].split_whitespace().collect::<Vec<_>>(),
                "author": f[2],
                "email": f[3],
                "date": f[4].parse::<u64>().unwrap_or(0),
                "refs": f[5].split(", ").filter(|r| !r.is_empty()).collect::<Vec<_>>(),
                "subject": f[6],
                "body": f[7].trim_end(),
            });
            if files {
                c["files"] = json!(f[8].lines().filter(|l| !l.is_empty()).collect::<Vec<_>>());
            }
            Some(c)
        })
        .collect();
    let more = list.len() > PAGE;
    list.truncate(PAGE);
    (list, more)
}

fn uncommitted(root: &Path) -> usize {
    git(root, &["status", "--porcelain=v1", "-z", "--untracked-files=all"])
        .map(|o| split0(&o).filter(|e| e.len() > 3).count())
        .unwrap_or(0)
}

/// What a code review of the current branch covers: the commits only it has (on no other branch,
/// local or remote, but its own upstream), newest first, and `base`, the commit its changes start
/// from (the newest one it shares with another branch). On the main branch it has none: the
/// review is of the uncommitted changes.
fn review(root: &Path) -> Result<Value> {
    if !is_git(root) {
        bail!("not a git repository");
    }
    let uncommitted = uncommitted(root);
    let branch = git_text(root, &["symbolic-ref", "--short", "-q", "HEAD"]).ok().map(|b| b.trim().to_string());
    let branch = branch.filter(|b| !b.is_empty());
    let none = |base: &str, main: bool| json!({"branch": branch, "main": main, "base": base, "parent": null, "commits": [], "more": false, "uncommitted": uncommitted});
    if git(root, &["rev-parse", "--verify", "-q", "HEAD"]).is_err() {
        return Ok(none(EMPTY_TREE, false));
    }
    let main = branch.as_deref().is_some_and(|b| {
        let origin = git_text(root, &["symbolic-ref", "--short", "-q", "refs/remotes/origin/HEAD"]).unwrap_or_default();
        b == "main" || b == "master" || origin.trim() == format!("origin/{b}")
    });
    if main {
        return Ok(none("HEAD", true));
    }
    // HEAD, but nothing another branch has.
    let mut others = vec!["--not".to_string()];
    if let Some(b) = &branch {
        others.push(format!("--exclude={b}"));
    }
    others.push("--branches".into());
    if let Some(b) = &branch {
        others.push(format!("--exclude=*/{b}"));
    }
    others.push("--remotes".into());
    let mut revs = vec!["HEAD"];
    revs.extend(others.iter().map(String::as_str));

    let mut args = vec!["rev-list", "--boundary"];
    args.extend(&revs);
    let listed = git_text(root, &args)?;
    let boundary: Vec<&str> = listed.lines().filter_map(|l| l.strip_prefix('-')).collect();
    if listed.lines().all(|l| l.starts_with('-')) {
        return Ok(none("HEAD", false));
    }
    let base = match boundary.as_slice() {
        // Its whole history: from the empty tree.
        [] => EMPTY_TREE.to_string(),
        [one] => one.to_string(),
        // Another branch merged in: from the newest commit shared, as a pull request would.
        many => {
            let mut args = vec!["log", "--no-walk=sorted", "--format=%H"];
            args.extend(many);
            git_text(root, &args)?.lines().next().unwrap_or_default().to_string()
        }
    };
    let (commits, more) = commits(root, &revs, 0, true);
    // The branch it came from, for the title.
    let mine = |n: &&str| branch.as_deref().is_some_and(|b| *n == b || n.ends_with(&format!("/{b}")));
    let parent = if base == EMPTY_TREE {
        None
    } else {
        let out = git_text(
            root,
            &["for-each-ref", "--contains", &base, "--format=%(refname:short)", "refs/heads", "refs/remotes"],
        )
        .unwrap_or_default();
        let names: Vec<&str> = out.lines().filter(|n| !n.ends_with("/HEAD") && !mine(n)).collect();
        let main = ["main", "master", "origin/main", "origin/master"];
        names.iter().find(|n| main.contains(n)).or(names.first()).map(|n| n.to_string())
    };
    Ok(json!({
        "branch": branch, "main": false, "base": base, "parent": parent,
        "commits": commits, "more": more, "uncommitted": uncommitted,
    }))
}

fn diff(root: &Path, from: Option<&str>, to: Option<&str>) -> Result<Value> {
    if !is_git(root) {
        bail!("not a git repository");
    }
    for r in [from, to].into_iter().flatten() {
        check_rev(r)?;
    }
    let has_head = git(root, &["rev-parse", "--verify", "-q", "HEAD"]).is_ok();
    let base = match (from, to) {
        (Some(f), _) => f.to_string(),
        (None, Some(t)) => {
            let parent = format!("{t}^");
            if git(root, &["rev-parse", "--verify", "-q", &parent]).is_ok() { parent } else { EMPTY_TREE.into() }
        }
        (None, None) => (if has_head { "HEAD" } else { EMPTY_TREE }).into(),
    };
    let mut args = vec!["diff", "--no-color", "--no-ext-diff", "-M", "-U3", base.as_str()];
    if let Some(t) = to {
        args.push(t);
    }
    args.push("--");
    let mut patch = git(root, &args)?;
    if to.is_none() {
        // New files git doesn't know yet, as additions.
        let out = git(root, &["ls-files", "-z", "--others", "--exclude-standard"])?;
        for f in split0(&out) {
            if patch.len() > MAX_DIFF {
                break;
            }
            patch.extend(new_file_patch(root, &f));
        }
    }
    let cut = patch.len() > MAX_DIFF;
    patch.truncate(MAX_DIFF);
    let mut stat = String::new();
    if let Some(t) = to {
        stat = git_text(root, &["log", "-1", "--format=%H%x1f%an%x1f%at%x1f%s%x1f%b", t])?;
    }
    let commit = stat.splitn(5, '\x1f').collect::<Vec<_>>();
    Ok(json!({
        "patch": String::from_utf8_lossy(&patch),
        "cut": cut,
        "base": base,
        "commit": (commit.len() == 5).then(|| json!({
            "hash": commit[0], "author": commit[1], "date": commit[2].parse::<u64>().unwrap_or(0),
            "subject": commit[3], "body": commit[4].trim_end(),
        })),
    }))
}

/// A patch adding an untracked file (like `git diff --no-index /dev/null file`).
fn new_file_patch(root: &Path, path: &str) -> Vec<u8> {
    let Ok(bytes) = std::fs::read(root.join(path)) else { return Vec::new() };
    let mut p = format!("diff --git a/{path} b/{path}\nnew file mode 100644\n");
    if bytes.len() > MAX_FILE || bytes.iter().take(8192).any(|b| *b == 0) {
        p.push_str(&format!("Binary files /dev/null and b/{path} differ\n"));
        return p.into_bytes();
    }
    let text = String::from_utf8_lossy(&bytes);
    let lines: Vec<&str> = text.lines().collect();
    if lines.is_empty() {
        return p.into_bytes();
    }
    p.push_str(&format!("--- /dev/null\n+++ b/{path}\n@@ -0,0 +1,{} @@\n", lines.len()));
    for l in lines {
        p.push('+');
        p.push_str(l);
        p.push('\n');
    }
    p.into_bytes()
}

/// Where a name is defined (lines that look like a definition in most languages) and where it is
/// used, by `git grep`.
fn find(root: &Path, rev: Option<&str>, name: &str) -> Result<Value> {
    let ident = !name.is_empty()
        && !name.starts_with(|c: char| c.is_ascii_digit())
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$');
    if !ident || !is_git(root) {
        return Ok(json!({"defs": [], "refs": []}));
    }
    if let Some(r) = rev {
        check_rev(r)?;
    }
    let n = regex_escape(name);
    let end = "([^[:alnum:]_$]|$)";
    let keywords = "(fn|func|function|def|class|struct|enum|trait|type|interface|impl|mod|module|namespace|const|let|var|val|static|macro_rules!|record|object)";
    let patterns = [
        format!("{keywords}[[:space:]]+(\\*[[:space:]]*)?{n}{end}"),
        // Go methods: `func (r *T) Name(`.
        format!("func[[:space:]]*\\([^)]*\\)[[:space:]]*{n}[[:space:]]*\\("),
        // `name = function`, `name = (…) =>`, `name: function`.
        format!(
            "(^|[^[:alnum:]_.$]){n}[[:space:]]*[:=][[:space:]]*(async[[:space:]]+)?(function|\\([^)]*\\)[[:space:]]*=>|[[:alnum:]_$]+[[:space:]]*=>)"
        ),
        // C-like `Type name(…) {` at the start of a line.
        format!("^[[:alnum:]_<>:*& ]+[[:space:]*&]{n}[[:space:]]*\\([^;]*$"),
    ];
    let grep = |extra: &[&str]| -> Vec<Value> {
        let mut args: Vec<&str> = vec!["grep", "-n", "-I", "--null", "--no-color", "--max-count", "20"];
        if rev.is_none() {
            args.push("--untracked");
        }
        args.extend(extra);
        if let Some(r) = rev {
            args.push(r);
        }
        args.push("--");
        let Ok(out) = git(root, &args) else { return Vec::new() };
        String::from_utf8_lossy(&out)
            .lines()
            .take(200)
            .filter_map(|l| {
                let (path, rest) = l.split_once('\0')?;
                let path = match rev {
                    Some(r) => path.strip_prefix(&format!("{r}:")).unwrap_or(path),
                    None => path,
                };
                let (line, text) = rest.split_once('\0').or_else(|| rest.split_once(':'))?;
                let text: String = text.trim().chars().take(240).collect();
                Some(json!({"path": path, "line": line.parse::<u64>().ok()?, "text": text}))
            })
            .collect()
    };
    let mut def_args = vec!["-E"];
    for p in &patterns {
        def_args.push("-e");
        def_args.push(p);
    }
    let defs = grep(&def_args);
    let refs = grep(&["-w", "-F", "-e", name]);
    Ok(json!({"defs": defs, "refs": refs}))
}

fn regex_escape(s: &str) -> String {
    s.chars().map(|c| if c == '$' { "\\$".to_string() } else { c.to_string() }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo() -> PathBuf {
        let d = std::env::temp_dir().join(format!("codeit-code-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(d.join("src")).unwrap();
        let run = |args: &[&str]| {
            let ok = Command::new("git")
                .args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false"])
                .args(args)
                .current_dir(&d)
                .output()
                .unwrap()
                .status
                .success();
            assert!(ok, "git {args:?}");
        };
        run(&["init", "-q", "-b", "main"]);
        std::fs::write(d.join("src/lib.rs"), "pub fn add(a: i32) -> i32 {\n    a + 1\n}\n").unwrap();
        run(&["add", "."]);
        run(&["commit", "-q", "-m", "first"]);
        std::fs::write(d.join("src/lib.rs"), "pub fn add(a: i32) -> i32 {\n    a + 2\n}\n\nfn use_it() { add(1); }\n")
            .unwrap();
        run(&["commit", "-q", "-am", "second"]);
        std::fs::write(d.join("new.txt"), "hello\n").unwrap();
        d
    }

    #[test]
    fn reads_trees_logs_diffs_and_definitions() {
        let d = repo();
        let t = tree(&d, None).unwrap();
        assert_eq!(t["files"], json!(["new.txt", "src/lib.rs"]));
        assert_eq!(t["status"]["new.txt"], "added");
        assert_eq!(tree(&d, Some("HEAD~1")).unwrap()["files"], json!(["src/lib.rs"]));

        let l = log(&d, None, 0).unwrap();
        let commits = l["commits"].as_array().unwrap();
        assert_eq!(commits.iter().map(|c| c["subject"].as_str().unwrap()).collect::<Vec<_>>(), ["second", "first"]);
        assert_eq!(l["uncommitted"], 1);

        let first = commits[1]["hash"].as_str().unwrap();
        let p = diff(&d, None, Some(first)).unwrap();
        assert!(p["patch"].as_str().unwrap().contains("+pub fn add"), "{p}");
        let w = diff(&d, None, None).unwrap();
        assert!(w["patch"].as_str().unwrap().contains("+++ b/new.txt\n@@ -0,0 +1,1 @@\n+hello"), "{w}");

        assert_eq!(file(&d, "src/lib.rs", Some("HEAD~1")).unwrap()["text"].as_str().unwrap().lines().count(), 3);
        assert!(file(&d, "../x", None).is_err());
        assert!(check_rev("--output=/tmp/x").is_err());

        let f = find(&d, None, "add").unwrap();
        assert_eq!(f["defs"], json!([{"path": "src/lib.rs", "line": 1, "text": "pub fn add(a: i32) -> i32 {"}]));
        assert_eq!(f["refs"].as_array().unwrap().len(), 2);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn reviews_only_the_branchs_own_commits() {
        let d = repo();
        let run = |args: &[&str]| {
            let ok = Command::new("git")
                .args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false"])
                .args(args)
                .current_dir(&d)
                .status()
                .unwrap()
                .success();
            assert!(ok, "git {args:?}");
        };
        // On main: only the uncommitted changes.
        let r = review(&d).unwrap();
        assert_eq!(
            (r["main"].clone(), r["base"].clone(), r["uncommitted"].clone()),
            (json!(true), json!("HEAD"), json!(1))
        );

        let main_head = git_text(&d, &["rev-parse", "HEAD"]).unwrap().trim().to_string();
        run(&["checkout", "-q", "-b", "feature"]);
        let fresh = review(&d).unwrap();
        assert_eq!((fresh["base"].clone(), fresh["commits"].clone()), (json!("HEAD"), json!([])));

        run(&["add", "new.txt"]);
        run(&["commit", "-q", "-m", "on feature"]);
        let feature_head = git_text(&d, &["rev-parse", "HEAD"]).unwrap().trim().to_string();
        // A branch from the feature branch: only what it adds, whichever branch it came from.
        run(&["checkout", "-q", "-b", "stacked"]);
        std::fs::write(d.join("more.txt"), "more\n").unwrap();
        run(&["add", "more.txt"]);
        run(&["commit", "-q", "-m", "on stacked"]);
        let r = review(&d).unwrap();
        let subjects: Vec<&str> =
            r["commits"].as_array().unwrap().iter().map(|c| c["subject"].as_str().unwrap()).collect();
        assert_eq!(subjects, ["on stacked"]);
        assert_eq!(r["commits"][0]["files"], json!(["more.txt"]));
        assert_eq!((r["base"].as_str().unwrap(), r["parent"].as_str().unwrap()), (feature_head.as_str(), "feature"));
        assert_ne!(feature_head, main_head);
        let _ = std::fs::remove_dir_all(&d);
    }
}
