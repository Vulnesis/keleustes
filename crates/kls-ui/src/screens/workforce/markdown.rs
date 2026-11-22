//! Builder extras: an agent as Markdown (front matter, the prompt as the
//! body, the typed spec in a fenced JSON block), and the tool groups the
//! builder offers as one click.

use kls_core::vault::AgentProfile;

/// Tool groups the builder adds in one click.
pub const TOOL_GROUPS: [(&str, &[&str]); 5] = [
    ("Read", &["Read", "Grep", "Glob"]),
    ("Edit", &["Read", "Grep", "Glob", "Edit", "Write"]),
    ("Shell", &["Bash", "Git"]),
    ("Web", &["WebFetch", "WebSearch"]),
    ("Forge", &["Git", "GH"]),
];

const SPEC_FENCE: &str = "```json belai-spec";

/// Adds a group's tools to a comma list, keeping order and no repeats.
pub fn add_group(tools: &str, group: &[&str]) -> String {
    let mut v: Vec<String> = tools
        .split([',', '\n'])
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .collect();
    for t in group {
        if !v.iter().any(|x| x == t) {
            v.push(t.to_string());
        }
    }
    v.join(", ")
}

fn quote(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_default()
}

/// The agent as Markdown.
pub fn to_markdown(p: &AgentProfile, tools: &str, meta_json: &str) -> String {
    let mut out = String::from("---\n");
    for (k, v) in [
        ("name", &p.name),
        ("display_name", &p.display_name),
        ("description", &p.description),
        ("provider", &p.provider),
        ("model", &p.model),
    ] {
        if !v.is_empty() {
            out.push_str(&format!("{k}: {}\n", quote(v)));
        }
    }
    if !tools.trim().is_empty() {
        out.push_str(&format!("tools: {}\n", quote(tools.trim())));
    }
    out.push_str("---\n\n");
    out.push_str(p.prompt.trim());
    out.push('\n');
    let meta = meta_json.trim();
    if !meta.is_empty() && meta != "{}" {
        let pretty = serde_json::from_str::<serde_json::Value>(meta)
            .and_then(|v| serde_json::to_string_pretty(&v))
            .unwrap_or_else(|_| meta.to_string());
        out.push_str(&format!("\n{SPEC_FENCE}\n{pretty}\n```\n"));
    }
    out
}

/// Reads [`to_markdown`]'s form back: (profile, tools, meta JSON). `None`
/// when there is no front matter or no name.
pub fn from_markdown(text: &str) -> Option<(AgentProfile, String, String)> {
    let text = text.trim_start_matches('\u{feff}').trim_start();
    let rest = text.strip_prefix("---")?;
    let end = rest.find("\n---")?;
    let (front, body) = (&rest[..end], &rest[end + 4..]);
    let mut p = AgentProfile::default();
    let mut tools = String::new();
    for line in front.lines() {
        let Some((k, v)) = line.split_once(':') else {
            continue;
        };
        let v = v.trim();
        let v =
            serde_json::from_str::<String>(v).unwrap_or_else(|_| v.trim_matches('"').to_string());
        match k.trim() {
            "name" => p.name = v,
            "display_name" | "displayName" => p.display_name = v,
            "description" => p.description = v,
            "provider" => p.provider = v,
            "model" => p.model = v,
            "tools" => tools = v,
            _ => {}
        }
    }
    let (prompt, meta) = match body.find(SPEC_FENCE) {
        Some(i) => {
            let after = &body[i + SPEC_FENCE.len()..];
            let close = after.find("\n```").unwrap_or(after.len());
            (&body[..i], after[..close].trim().to_string())
        }
        None => (body, String::new()),
    };
    if !meta.is_empty() && serde_json::from_str::<serde_json::Value>(&meta).is_err() {
        return None;
    }
    p.prompt = prompt.trim().to_string();
    if p.name.trim().is_empty() {
        return None;
    }
    Some((p, tools, meta))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markdown_round_trips() {
        let p = AgentProfile {
            name: "patcher".into(),
            display_name: "Patch: \"Bot\"".into(),
            description: "Fixes deps".into(),
            prompt: "Fix the dependency.\n\nThen test.".into(),
            provider: "anthropic".into(),
            ..Default::default()
        };
        let md = to_markdown(&p, "Read, Edit", r#"{"mode":"worker"}"#);
        let (q, tools, meta) = from_markdown(&md).unwrap();
        assert_eq!(q.name, p.name);
        assert_eq!(q.display_name, p.display_name);
        assert_eq!(q.prompt, p.prompt);
        assert_eq!(tools, "Read, Edit");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&meta).unwrap()["mode"],
            "worker"
        );
        assert!(from_markdown("no front matter").is_none());
        assert!(from_markdown("---\nname: \"\"\n---\nx").is_none());
    }

    #[test]
    fn groups_add_without_repeats() {
        assert_eq!(
            add_group("Read, Bash", TOOL_GROUPS[0].1),
            "Read, Bash, Grep, Glob"
        );
    }
}
