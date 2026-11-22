//! The fixed tool table (belai `agentimport/build.go` `toolTable`): the
//! names other harnesses give their tools, mapped to belai's. An unknown
//! tool is dropped; nothing known left means the read-only set.

/// What an imported definition gets when it names no tool belai knows.
pub const READ_ONLY: [&str; 3] = ["Read", "Grep", "Glob"];

/// Tools that change things (noted on import).
pub const MUTATING: [&str; 4] = ["Write", "Edit", "Bash", "Task"];

/// Lower-cased, letters and digits only.
pub fn key(s: &str) -> String {
    s.chars()
        .flat_map(char::to_lowercase)
        .filter(|c| c.is_ascii_alphanumeric())
        .collect()
}

/// belai's name for a foreign tool name, if it has one.
pub fn map(name: &str) -> Option<&'static str> {
    Some(match key(name).as_str() {
        "read" | "readfile" | "view" | "cat" | "open" => "Read",
        "grep" | "search" | "searchfiles" | "ripgrep" | "rg" | "findinfiles" => "Grep",
        "glob" | "find" | "list" | "listfiles" | "listdirectory" => "Glob",
        "ls" => "LS",
        "write" | "writefile" | "create" | "createfile" => "Write",
        "edit" | "strreplace" | "strreplaceeditor" | "patch" | "applypatch" | "replace"
        | "multiedit" => "Edit",
        "bash" | "shell" | "exec" | "run" | "runcommand" | "terminal" | "execute" | "command"
        | "sh" => "Bash",
        "webfetch" | "fetch" | "httpget" | "openurl" => "WebFetch",
        "websearch" => "WebSearch",
        "todo" | "todowrite" | "updateplan" | "plan" => "Todo",
        "task" | "subagent" => "Task",
        _ => return None,
    })
}

/// Maps a list: `(kept, dropped)`, kept deduplicated in source order.
/// `Task(name)` (a binding to a named agent) is dropped: no A2A path.
pub fn map_all(names: &[String]) -> (Vec<String>, Vec<String>) {
    let mut kept: Vec<String> = Vec::new();
    let mut dropped = Vec::new();
    for n in names {
        let n = n.trim();
        if n.is_empty() {
            continue;
        }
        if n.contains('(') {
            dropped.push(n.to_string());
            continue;
        }
        match map(n) {
            Some(t) if !kept.iter().any(|k| k == t) => kept.push(t.to_string()),
            Some(_) => {}
            None => dropped.push(n.to_string()),
        }
    }
    (kept, dropped)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table() {
        let names: Vec<String> = [
            "Read",
            "Grep",
            "Bash",
            "mcp__x__y",
            "read_file",
            "Task(reviewer)",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let (k, d) = map_all(&names);
        assert_eq!(k, vec!["Read", "Grep", "Bash"]);
        assert_eq!(d, vec!["mcp__x__y", "Task(reviewer)"]);
        assert_eq!(map("str_replace_editor"), Some("Edit"));
    }
}
