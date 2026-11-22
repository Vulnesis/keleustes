//! Front matter: reading foreign Markdown definitions (lenient YAML, with a
//! line-based fallback like belai's `parseLegacy`) and writing belai's
//! strict form (one `key: value` per line).

use serde_norway::Value as Yaml;
use std::collections::BTreeMap;

/// A parsed source document: front matter fields in order, and the body.
#[derive(Clone, Debug, Default)]
pub struct Doc {
    pub fields: Vec<(String, Yaml)>,
    pub body: String,
    /// The document had a front matter block.
    pub had_front: bool,
}

impl Doc {
    pub fn get(&self, k: &str) -> Option<&Yaml> {
        self.fields.iter().find(|(f, _)| f == k).map(|(_, v)| v)
    }

    /// A scalar as text (numbers and bools written out).
    pub fn str(&self, k: &str) -> Option<String> {
        self.get(k).and_then(scalar).filter(|s| !s.is_empty())
    }

    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.fields.iter().map(|(k, _)| k.as_str())
    }
}

/// The text of a scalar.
pub fn scalar(v: &Yaml) -> Option<String> {
    match v {
        Yaml::String(s) => Some(s.trim().to_string()),
        Yaml::Number(n) => Some(n.to_string()),
        Yaml::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

/// A list of strings from a sequence, or a comma/space separated string.
pub fn list(v: &Yaml) -> Vec<String> {
    match v {
        Yaml::Sequence(s) => s
            .iter()
            .filter_map(scalar)
            .filter(|s| !s.is_empty())
            .collect(),
        Yaml::String(s) => s
            .split([',', ' ', '\t', '\n'])
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(str::to_string)
            .collect(),
        _ => vec![],
    }
}

/// A one-line JSON rendering of any YAML value (for `metadata` notes).
pub fn to_json_line(v: &Yaml) -> String {
    serde_json::to_string(&yaml_to_json(v)).unwrap_or_default()
}

pub fn yaml_to_json(v: &Yaml) -> serde_json::Value {
    serde_json::to_value(v).unwrap_or(serde_json::Value::Null)
}

/// Splits `---\n<front>\n---\n<body>`. No front matter: everything is the body.
pub fn split(text: &str) -> (Option<String>, String) {
    let t = text.strip_prefix('\u{feff}').unwrap_or(text);
    let t = t.replace("\r\n", "\n");
    let Some(rest) = t.strip_prefix("---\n") else {
        return (None, t);
    };
    // The closing line is `---` (trailing spaces allowed).
    let mut pos = 0;
    for line in rest.split_inclusive('\n') {
        if line.trim_end() == "---" {
            let front = rest[..pos].to_string();
            let body = rest[pos + line.len()..].to_string();
            return (Some(front), body);
        }
        pos += line.len();
    }
    (None, t)
}

/// Parses a front matter block: YAML when it is a mapping, else
/// `key: value` lines.
pub fn parse_front(front: &str) -> Vec<(String, Yaml)> {
    if front.trim().is_empty() {
        return vec![];
    }
    if let Ok(Yaml::Mapping(m)) = serde_norway::from_str::<Yaml>(front) {
        return m
            .into_iter()
            .filter_map(|(k, v)| scalar(&k).map(|k| (k, v)))
            .collect();
    }
    let mut out: Vec<(String, Yaml)> = Vec::new();
    for line in front.lines() {
        if line.starts_with([' ', '\t']) || line.trim_start().starts_with('#') {
            continue;
        }
        if let Some((k, v)) = line.split_once(':') {
            let k = k.trim().to_string();
            if k.is_empty() || out.iter().any(|(x, _)| *x == k) {
                continue;
            }
            let v = v.trim();
            let v = v
                .strip_prefix('"')
                .and_then(|x| x.strip_suffix('"'))
                .or_else(|| v.strip_prefix('\'').and_then(|x| x.strip_suffix('\'')))
                .unwrap_or(v);
            out.push((k, Yaml::String(v.to_string())));
        }
    }
    out
}

/// Parses a Markdown document with optional front matter.
pub fn parse(text: &str) -> Doc {
    let (front, body) = split(text);
    match front {
        Some(f) => Doc {
            fields: parse_front(&f),
            body,
            had_front: true,
        },
        None => Doc {
            fields: vec![],
            body,
            had_front: false,
        },
    }
}

/// A value safe as a belai plain scalar line, quoted when YAML would read
/// it otherwise.
pub fn yaml_scalar(s: &str) -> String {
    let needs = s.is_empty()
        || s.contains(": ")
        || s.contains(" #")
        || s.ends_with(':')
        || s.starts_with([
            '"', '\'', '[', ']', '{', '}', '#', '&', '*', '!', '|', '>', '%', '@', '`', '-', '?',
            ',', ' ',
        ])
        || s.ends_with(' ')
        || matches!(
            s.to_ascii_lowercase().as_str(),
            "true" | "false" | "yes" | "no" | "null" | "~" | "on" | "off"
        )
        || s.parse::<f64>().is_ok();
    if !needs {
        return s.to_string();
    }
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Composes `---\n<k: v lines>---\n\n<body>`; empty values are left out.
pub fn compose(fields: &[(&str, String)], body: &str) -> String {
    let mut s = String::from("---\n");
    for (k, v) in fields {
        if v.is_empty() {
            continue;
        }
        s.push_str(k);
        s.push_str(": ");
        s.push_str(v);
        s.push('\n');
    }
    s.push_str("---\n\n");
    s.push_str(body.trim_start_matches('\n'));
    s
}

/// Parses belai's strict front matter (the output form) into a map of the
/// raw values as YAML reads them; `None` when it is not a mapping.
pub fn strict_fields(front: &str) -> Result<BTreeMap<String, Yaml>, String> {
    if front.trim().is_empty() {
        return Ok(BTreeMap::new());
    }
    match serde_norway::from_str::<Yaml>(front) {
        Ok(Yaml::Mapping(m)) => {
            let mut out = BTreeMap::new();
            for (k, v) in m {
                let Some(k) = scalar(&k) else {
                    return Err("front-matter keys must be plain words".into());
                };
                if out.insert(k.clone(), v).is_some() {
                    return Err(format!("front-matter field {k:?} appears twice"));
                }
            }
            Ok(out)
        }
        Ok(Yaml::Null) => Ok(BTreeMap::new()),
        Ok(_) => Err("the front matter is not a mapping".into()),
        Err(e) => {
            let msg = e.to_string();
            if msg.contains("duplicate") {
                Err("a front-matter field appears twice".into())
            } else {
                // belai falls back to `key: value` lines.
                let mut out = BTreeMap::new();
                for line in front.lines() {
                    let l = line.trim();
                    if l.is_empty() || l.starts_with('#') {
                        continue;
                    }
                    let Some((k, v)) = l.split_once(':') else {
                        return Err(format!("malformed front matter line {l:?}"));
                    };
                    if out
                        .insert(k.trim().to_string(), Yaml::String(v.trim().to_string()))
                        .is_some()
                    {
                        return Err(format!("front-matter field {:?} appears twice", k.trim()));
                    }
                }
                Ok(out)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_and_parse() {
        let d = parse("---\nname: x\ntools: [Read, Grep]\n---\nbody\n");
        assert_eq!(d.str("name").as_deref(), Some("x"));
        assert_eq!(list(d.get("tools").unwrap()), vec!["Read", "Grep"]);
        assert_eq!(d.body, "body\n");
        let d = parse("no front");
        assert!(!d.had_front);
        // Not YAML: line fallback.
        let d = parse("---\nname: a: b: c\ndescription: [oops\n---\nb");
        assert_eq!(d.str("description").as_deref(), Some("[oops"));
    }

    #[test]
    fn scalars_quote_when_needed() {
        assert_eq!(yaml_scalar("plain words"), "plain words");
        assert_eq!(yaml_scalar("a: b"), "\"a: b\"");
        assert_eq!(yaml_scalar("true"), "\"true\"");
        assert_eq!(yaml_scalar("- x"), "\"- x\"");
        let f = strict_fields("description: \"a: b\"").unwrap();
        assert_eq!(scalar(&f["description"]).unwrap(), "a: b");
    }
}
