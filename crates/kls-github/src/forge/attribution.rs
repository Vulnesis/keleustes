//! The attribution block every factory write carries (vdb-site
//! `internal/attribution`, website `attribution-trailers.ts`). A commit
//! message gets every line; a pull request body, an issue, a comment or a
//! review gets the links only (Session, Finding, Vulnerability, Advisory),
//! because `Co-authored-by` and `Signed-off-by` mean something to git and
//! nothing in Markdown.
//!
//! Pinned to a copy of the web's `attribution-cases.json` in `testdata/`.
//! On the desktop the signed-in person makes and signs the commit
//! (`commit_as`), so the bot is the co-author.

use serde::Deserialize;

pub const ORIGIN: &str = "https://vulnetix.com";
pub const ADVISORY_REPO_URL: &str = "https://github.com/Vulnetix/advisory-database";
pub const MAX_VULN_LINKS: usize = 10;

/// A person: name and email.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
pub struct Human {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub email: String,
}

/// Everything a block can name. Empty fields are left out.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Context {
    /// `pix` or `vulnetix` (default).
    pub bot: String,
    /// Who started the work; `None` for a cron, a webhook or a batch job.
    pub human: Option<Human>,
    /// The person whose account makes and signs the commit.
    pub commit_as: Option<Human>,
    pub session_id: String,
    pub finding_uuid: String,
    pub vuln_ids: Vec<String>,
    pub advisory_path: String,
    pub origin: String,
    /// The account the bot acts as on a forge other than github.com.
    pub bot_as: Option<Human>,
}

fn pix() -> Human {
    Human {
        name: "Pix [bot]".into(),
        email: "security@vulnetix.com".into(),
    }
}

fn vulnetix() -> Human {
    Human {
        name: "vulnetix[bot]".into(),
        email: "176591487+vulnetix[bot]@users.noreply.github.com".into(),
    }
}

fn bot_named(bot: &str) -> Human {
    if bot == "pix" { pix() } else { vulnetix() }
}

fn email_shape(e: &str) -> bool {
    let bad = |c: char| c.is_whitespace() || c == '<' || c == '>' || c == '@';
    match e.split_once('@') {
        Some((a, b)) => {
            !a.is_empty() && !b.is_empty() && !a.chars().any(bad) && !b.chars().any(bad)
        }
        None => false,
    }
}

fn clean_human(h: Option<&Human>) -> Option<Human> {
    let h = h?;
    let stripped: String = h
        .name
        .chars()
        .map(|c| {
            if matches!(c, '<' | '>' | '\r' | '\n') {
                ' '
            } else {
                c
            }
        })
        .collect();
    let name = stripped.split_whitespace().collect::<Vec<_>>().join(" ");
    let email = h.email.trim().to_string();
    (!name.is_empty() && email_shape(&email)).then_some(Human { name, email })
}

fn person(h: &Human) -> String {
    format!("{} <{}>", h.name, h.email)
}

fn all(s: &str, ok: impl Fn(char) -> bool, min: usize, max: usize) -> bool {
    let n = s.chars().count();
    n >= min && n <= max && s.chars().all(ok)
}

fn digits(s: &str, min: usize, max: usize) -> bool {
    all(s, |c| c.is_ascii_digit(), min, max)
}

/// The vulnerability id shapes of vdb-site `facts.vulnShapes`, whole string.
fn vuln_shape(s: &str) -> bool {
    let year_num = |rest: &str, min: usize, max: usize| {
        rest.split_once('-')
            .is_some_and(|(y, n)| digits(y, 4, 4) && digits(n, min, max))
    };
    let year_colon = |rest: &str| {
        rest.split_once(':')
            .is_some_and(|(y, n)| digits(y, 4, 4) && digits(n, 4, 6))
    };
    let advisory = |rest: &str, min: usize, max: usize| match rest.split_once('-') {
        Some((a, b)) => digits(a, min, max) && digits(b, 1, 2),
        None => digits(rest, min, max),
    };
    let Some((prefix, rest)) = s.split_once('-') else {
        return false;
    };
    match prefix {
        "CVE" => year_num(rest, 4, 12),
        "GHSA" => {
            let parts: Vec<&str> = rest.split('-').collect();
            parts.len() == 3
                && parts
                    .iter()
                    .all(|p| all(p, |c| "23456789cfghjmpqrvwx".contains(c), 4, 4))
        }
        "OSV" | "PYSEC" | "MAL" => year_num(rest, 1, 7),
        "RUSTSEC" => year_num(rest, 4, 4),
        "GO" | "GSD" => year_num(rest, 4, 7),
        "EUVD" => year_num(rest, 4, 8),
        "VND" => year_num(rest, 1, 8),
        "DSA" | "DLA" => advisory(rest, 3, 5),
        "USN" => advisory(rest, 1, 5),
        "RHSA" | "RHBA" | "RHEA" | "ALSA" | "RLSA" => year_colon(rest),
        _ => false,
    }
}

/// vdb-site `facts.CanonicalVulnID`: the canonical form of a whole id.
pub fn canonical_vuln_id(s: &str) -> Option<String> {
    let s = s.trim();
    if s.is_empty() || s.len() > 40 || !s.is_ascii() {
        return None;
    }
    let c = if s.len() >= 5 && s[..5].eq_ignore_ascii_case("GHSA-") {
        format!("GHSA-{}", s[5..].to_ascii_lowercase())
    } else {
        s.to_ascii_uppercase()
    };
    vuln_shape(&c).then_some(c)
}

fn is_vuln_id(id: &str) -> bool {
    canonical_vuln_id(id).as_deref() == Some(id)
}

fn gcve_id(s: &str) -> bool {
    let Some(rest) = s.strip_prefix("GCVE-") else {
        return false;
    };
    rest.split_once('-').is_some_and(|(n, tail)| {
        digits(n, 1, 6) && all(tail, |c| c.is_ascii_alphanumeric() || c == '-', 1, 64)
    })
}

/// The link for a published GCVE record, or the repository.
pub fn advisory_url(path: &str) -> String {
    let p = path.trim_start_matches('/');
    let shape = all(
        p,
        |c| c.is_ascii_alphanumeric() || "._/-".contains(c),
        1,
        300,
    );
    if shape && !p.split('/').any(|seg| seg == "..") {
        return format!("{ADVISORY_REPO_URL}/blob/main/{p}");
    }
    ADVISORY_REPO_URL.into()
}

fn query_escape(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// The trailer lines in order. With no usable human the other bot is the
/// co-author, so the line is always there.
pub fn trailers(c: &Context) -> Vec<String> {
    let origin = match c.origin.trim_end_matches('/') {
        "" => ORIGIN,
        o => o,
    };
    let other = if c.bot == "pix" { vulnetix() } else { pix() };
    let co = clean_human(c.human.as_ref()).unwrap_or(other);
    let bot = clean_human(c.bot_as.as_ref()).unwrap_or_else(|| bot_named(&c.bot));
    let mut lines = match clean_human(c.commit_as.as_ref()) {
        Some(signer) => vec![
            format!("Co-authored-by: {}", person(&bot)),
            format!("Signed-off-by: {}", person(&signer)),
        ],
        None => vec![
            format!("Co-authored-by: {}", person(&co)),
            format!("Signed-off-by: {}", person(&bot)),
        ],
    };
    if !c.session_id.is_empty()
        && all(
            &c.session_id,
            |ch| ch.is_ascii_alphanumeric() || "._:-".contains(ch),
            1,
            128,
        )
    {
        lines.push(format!(
            "Session: {origin}/resolve/factory-session?id={}",
            query_escape(&c.session_id)
        ));
    }
    if !c.finding_uuid.is_empty()
        && all(
            &c.finding_uuid,
            |ch| ch.is_ascii_hexdigit() || ch == '-',
            8,
            64,
        )
    {
        lines.push(format!(
            "Finding: {origin}/resolve/scanner-results/finding/{}",
            c.finding_uuid
        ));
    }
    let mut seen = std::collections::BTreeSet::new();
    let mut gcve = false;
    let mut n = 0;
    for id in &c.vuln_ids {
        if seen.contains(id) || (!is_vuln_id(id) && !gcve_id(id)) {
            continue;
        }
        seen.insert(id.clone());
        if n == MAX_VULN_LINKS {
            break;
        }
        n += 1;
        lines.push(format!("Vulnerability: {origin}/vdb/{}", query_escape(id)));
        gcve |= gcve_id(id);
    }
    if gcve {
        lines.push(format!("Advisory: {}", advisory_url(&c.advisory_path)));
    }
    lines
}

/// [`trailers`] without `Co-authored-by` and `Signed-off-by`.
pub fn links(c: &Context) -> Vec<String> {
    trailers(c)
        .into_iter()
        .filter(|l| !l.starts_with("Co-authored-by: ") && !l.starts_with("Signed-off-by: "))
        .collect()
}

fn trailer_line(l: &str) -> bool {
    let Some((k, v)) = l.split_once(": ") else {
        return false;
    };
    k.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
        && k.chars().all(|c| c.is_ascii_alphabetic() || c == '-')
        && v.chars().next().is_some_and(|c| !c.is_whitespace())
}

fn marker_line(l: &str) -> bool {
    l.starts_with("<!--") && l.ends_with("-->") && l.len() >= 7
}

/// Splits on blank lines (`\n\s*\n`).
fn paragraphs(body: &str) -> Vec<String> {
    let mut out = vec![String::new()];
    let lines: Vec<&str> = body.split('\n').collect();
    for (i, l) in lines.iter().enumerate() {
        if l.trim().is_empty() && i > 0 && i + 1 < lines.len() {
            if !out.last().is_some_and(String::is_empty) {
                out.push(String::new());
            }
            continue;
        }
        let cur = out.last_mut().expect("one paragraph");
        if !cur.is_empty() {
            cur.push('\n');
        }
        cur.push_str(l);
    }
    out
}

/// Adds the block to the end of `text`; safe to run twice. In a commit the
/// lines join a last paragraph that is already all trailers. With
/// `markdown` the block (links only) goes before a trailing hidden marker,
/// after a blank line.
pub fn append(text: &str, c: &Context, markdown: bool) -> String {
    let all_lines: Vec<&str> = text.split('\n').collect();
    let mut end = all_lines.len();
    if markdown {
        while end > 0 {
            let l = all_lines[end - 1].trim();
            if !l.is_empty() && !marker_line(l) {
                break;
            }
            end -= 1;
        }
    }
    let body = all_lines[..end].join("\n").trim_end().to_string();
    let tail = if markdown {
        all_lines[end..].join("\n").trim_start().to_string()
    } else {
        String::new()
    };
    let present: std::collections::BTreeSet<&str> = body.split('\n').map(str::trim).collect();
    let lines = if markdown { links(c) } else { trailers(c) };
    let missing: Vec<String> = lines
        .into_iter()
        .filter(|l| !present.contains(l.as_str()))
        .collect();
    if missing.is_empty() {
        return text.to_string();
    }
    let paras = paragraphs(&body);
    let joins = !markdown
        && paras.len() > 1
        && paras
            .last()
            .is_some_and(|p| p.split('\n').all(trailer_line));
    let sep = match (body.is_empty(), joins) {
        (true, _) => "",
        (false, true) => "\n",
        (false, false) => "\n\n",
    };
    let out = format!("{body}{sep}{}", missing.join("\n"));
    if !tail.is_empty() {
        return format!("{out}\n\n{tail}");
    }
    if text.ends_with('\n') {
        return format!("{out}\n");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    #[test]
    fn replays_the_web_cases() {
        let cases: Value =
            serde_json::from_str(include_str!("../../testdata/attribution-cases.json")).unwrap();
        for case in cases["trailers"].as_array().unwrap() {
            let ctx: Context = serde_json::from_value(case["ctx"].clone()).unwrap();
            let want: Vec<String> = serde_json::from_value(case["lines"].clone()).unwrap();
            assert_eq!(trailers(&ctx), want, "{}", case["name"]);
        }
        for case in cases["appends"].as_array().unwrap() {
            let ctx: Context = serde_json::from_value(case["ctx"].clone()).unwrap();
            let got = append(
                case["text"].as_str().unwrap(),
                &ctx,
                case["markdown"].as_bool().unwrap(),
            );
            assert_eq!(got, case["want"].as_str().unwrap(), "{}", case["name"]);
        }
    }

    #[test]
    fn vuln_ids_are_canonical_only() {
        assert_eq!(
            canonical_vuln_id("ghsa-JFH8-c2jp-5v3q").as_deref(),
            Some("GHSA-jfh8-c2jp-5v3q")
        );
        assert!(is_vuln_id("RHSA-2024:12345"));
        assert!(!is_vuln_id("cve-2024-0001"));
        assert!(!is_vuln_id("CVE-24-1"));
    }
}
