//! Skills: `SKILL.md` files with a name and a description in their frontmatter. The system
//! prompt lists them; the skill tool loads one's instructions when a task matches.
//!
//! Found in `.codeit/skills`, `~/.config/codeit/skills`, the opencode, Claude and `.agents` skill
//! folders (project and global), and the paths in `skills.paths`.

use std::path::{Path, PathBuf};

use crate::util;

#[derive(Clone, Debug)]
pub struct Skill {
    pub name: String,
    pub description: String,
    /// The SKILL.md file.
    pub path: PathBuf,
}

impl Skill {
    pub fn dir(&self) -> &Path {
        self.path.parent().unwrap_or(Path::new("."))
    }

    /// The instructions (the file without its frontmatter).
    pub fn body(&self) -> String {
        std::fs::read_to_string(&self.path).map(|t| util::frontmatter(&t).1).unwrap_or_default()
    }
}

/// Folders that may hold skills, most specific last (a later skill with the same name wins).
pub fn roots(project_dirs: &[PathBuf], extra: &[String]) -> Vec<PathBuf> {
    let home = dirs::home_dir().unwrap_or_default();
    let config = crate::config::xdg_config();
    let mut out = vec![
        home.join(".claude/skills"),
        home.join(".agents/skills"),
        config.join("opencode/skill"),
        config.join("opencode/skills"),
        config.join("codeit/skills"),
    ];
    for d in project_dirs {
        for sub in [".claude/skills", ".agents/skills", ".opencode/skill", ".opencode/skills", ".codeit/skills"] {
            out.push(d.join(sub));
        }
    }
    let cwd = project_dirs.last().cloned().unwrap_or_default();
    out.extend(extra.iter().map(|p| util::resolve(&cwd, p)));
    out
}

pub fn discover(roots: &[PathBuf]) -> Vec<Skill> {
    let mut skills: Vec<Skill> = Vec::new();
    for root in roots {
        if !root.is_dir() {
            continue;
        }
        let walker = ignore::WalkBuilder::new(root).hidden(false).follow_links(true).max_depth(Some(6)).build();
        for entry in walker.flatten() {
            if entry.file_name() != "SKILL.md" {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(entry.path()) else { continue };
            let (fields, _) = util::frontmatter(&text);
            let dir_name = entry.path().parent().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned());
            let Some(name) = fields.get("name").cloned().or(dir_name) else { continue };
            let description = fields.get("description").cloned().unwrap_or_default();
            skills.retain(|s| s.name != name);
            skills.push(Skill { name, description, path: entry.path().to_path_buf() });
        }
    }
    skills.sort_by(|a, b| a.name.cmp(&b.name));
    skills
}

/// The skill list for the system prompt.
pub fn prompt(skills: &[Skill]) -> Option<String> {
    if skills.is_empty() {
        return None;
    }
    let mut s = String::from(
        "# Skills\nSkills hold instructions for specific tasks. When a task matches a skill's description, load it with the skill tool before starting.\n",
    );
    for k in skills {
        s.push_str(&format!("- {}: {}\n", k.name, k.description.replace('\n', " ")));
    }
    Some(s)
}
