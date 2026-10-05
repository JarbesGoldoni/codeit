//! The system prompt: the agent's prompt, the environment, instructions, skills, MCP server
//! instructions and what extensions add. Stable within a session, so providers can cache it.

use crate::agent::Agent;
use crate::permission::Ruleset;
use crate::{Harness, instructions, skill};

pub const BASE: &str = include_str!("prompts/system.md");
pub const PLAN: &str = include_str!("prompts/plan.md");
pub const BUILD_SWITCH: &str = include_str!("prompts/build_switch.md");
pub const MAX_STEPS: &str = include_str!("prompts/max_steps.md");
pub const COMPACTION: &str = include_str!("prompts/compaction.md");
pub const COMPACTION_REQUEST: &str = include_str!("prompts/compaction_request.md");
pub const CONDENSE: &str = include_str!("prompts/condense.md");

pub fn system(h: &Harness, agent: &Agent, model_key: &str, rules: &Ruleset) -> String {
    let mut parts: Vec<String> = vec![agent.prompt.clone().unwrap_or_else(|| BASE.to_string())];

    let git = if h.root.join(".git").exists() {
        if h.root == h.cwd { "yes".to_string() } else { format!("yes, root {}", h.root.display()) }
    } else {
        "no".to_string()
    };
    parts.push(format!(
        "<env>\nWorking directory: {}\nGit repository: {git}\nPlatform: {}\nDate: {}\nModel: {model_key}\n</env>",
        h.cwd.display(),
        platform(),
        today(),
    ));
    if let Some(i) = instructions::prompt(&h.instructions()) {
        parts.push(i);
    }
    if !rules.disabled("skill")
        && let Some(s) = skill::prompt(&h.skills)
    {
        parts.push(s);
    }
    if let Some(m) = h.mcp.prompt() {
        parts.push(m);
    }
    for e in &h.extensions {
        e.system(&mut parts);
    }
    parts.iter().map(|p| p.trim()).filter(|p| !p.is_empty()).collect::<Vec<_>>().join("\n\n")
}

fn platform() -> String {
    let os = std::env::consts::OS;
    let wsl = std::fs::read_to_string("/proc/version").is_ok_and(|v| v.to_lowercase().contains("microsoft"));
    if wsl { format!("{os} (WSL)") } else { os.into() }
}

/// Today as YYYY-MM-DD (UTC), without a date library.
pub fn today() -> String {
    let days = (crate::util::now() / 86_400) as i64;
    let (y, m, d) = civil(days);
    format!("{y:04}-{m:02}-{d:02}")
}

/// Days since 1970-01-01 to (year, month, day); Howard Hinnant's algorithm.
fn civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    #[test]
    fn civil_dates() {
        assert_eq!(super::civil(0), (1970, 1, 1));
        assert_eq!(super::civil(20_362), (2025, 10, 1));
        assert_eq!(super::civil(19_782), (2024, 2, 29));
    }
}
