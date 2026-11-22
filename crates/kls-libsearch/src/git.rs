//! GitHub without credentials: smart-HTTP ref advertisement (pkt-line,
//! `ls-remote` without git) and a bounded, filtered codeload tarball
//! reader.
//!
//! The reader keeps only regular files whose path (below the archive's top
//! folder) matches the keep set, plus `LICENSE*`, `plugin.json` and
//! `marketplace.json`. It skips links, devices and other specials, refuses
//! `..`, absolute and backslash paths, skips credential file names, and
//! stops at the caps (belai `agentimport`): 1000 files, 1 MiB a file,
//! 8 MiB kept, depth 6, 256 MiB expanded (a gzip bomb is cut there).

use anyhow::{Result, anyhow, bail};
use bytes::Bytes;
use std::collections::BTreeMap;
use std::io::Read;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    pub files: usize,
    pub file_bytes: u64,
    pub kept_bytes: u64,
    pub expanded_bytes: u64,
    pub depth: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            files: 1000,
            file_bytes: 1024 * 1024,
            kept_bytes: 8 * 1024 * 1024,
            expanded_bytes: 256 * 1024 * 1024,
            depth: 6,
        }
    }
}

/// The kept files of a repo at one commit.
#[derive(Clone, Debug, Default)]
pub struct Tree {
    /// Paths relative to the repo root.
    pub files: BTreeMap<String, Bytes>,
    /// `(path, why)` for entries left out that matched the keep set.
    pub skipped: Vec<(String, String)>,
    /// The tarball URL (`codeload…/tar.gz/<sha>`).
    pub url: String,
    /// A cap was hit; the tree is incomplete.
    pub truncated: bool,
}

pub fn is_sha(s: &str) -> bool {
    s.len() == 40
        && s.bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

/// Parses a `git-upload-pack` ref advertisement (pkt-line). `HEAD` comes
/// first when advertised; peeled tags (`^{}`) are dropped.
pub fn parse_refs(body: &[u8]) -> Result<Vec<(String, String)>> {
    let mut out = Vec::new();
    let mut i = 0;
    let mut saw_service = false;
    while i + 4 <= body.len() {
        let len = std::str::from_utf8(&body[i..i + 4])
            .ok()
            .and_then(|h| usize::from_str_radix(h, 16).ok())
            .ok_or_else(|| anyhow!("not a pkt-line ref advertisement"))?;
        if len == 0 {
            i += 4;
            continue;
        }
        if len < 4 || i + len > body.len() {
            bail!("truncated pkt-line");
        }
        let line = &body[i + 4..i + len];
        i += len;
        let line = String::from_utf8_lossy(line);
        let line = line.trim_end_matches('\n');
        if line.starts_with("# service=") {
            saw_service = true;
            continue;
        }
        // "<sha> <ref>\0caps" on the first ref line.
        let line = line.split('\0').next().unwrap_or_default();
        let Some((sha, name)) = line.split_once(' ') else {
            continue;
        };
        if !is_sha(sha) || name.ends_with("^{}") {
            continue;
        }
        out.push((name.to_string(), sha.to_string()));
    }
    if !saw_service && out.is_empty() {
        bail!("not a smart-HTTP ref advertisement");
    }
    out.sort_by_key(|(n, _)| n != "HEAD");
    Ok(out)
}

/// The commit for `want`: `None` is HEAD; a short name tries
/// `refs/heads/<n>` then `refs/tags/<n>`; a full ref matches exactly.
pub fn pick_ref(refs: &[(String, String)], want: Option<&str>) -> Option<String> {
    let find = |n: &str| refs.iter().find(|(r, _)| r == n).map(|(_, s)| s.clone());
    match want {
        None | Some("") | Some("HEAD") => find("HEAD")
            .or_else(|| find("refs/heads/main"))
            .or_else(|| find("refs/heads/master")),
        Some(w) if w.starts_with("refs/") => find(w),
        Some(w) => find(&format!("refs/heads/{w}")).or_else(|| find(&format!("refs/tags/{w}"))),
    }
}

/// Credential-shaped file names never kept.
pub fn credential_name(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    matches!(
        n.as_str(),
        "auth.json"
            | ".env"
            | ".npmrc"
            | ".pypirc"
            | ".netrc"
            | "credentials"
            | "credentials.json"
            | ".git-credentials"
            | "secrets.json"
            | ".dockercfg"
    ) || n.starts_with(".env.")
        || n.starts_with("id_rsa")
        || n.starts_with("id_ed25519")
        || n.starts_with("id_ecdsa")
        || n.starts_with("id_dsa")
        || n.ends_with(".pem")
        || n.ends_with(".key")
        || n.ends_with(".p12")
        || n.ends_with(".pfx")
}

/// Always kept when present (licence detection and manifests).
pub fn always_keep(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    let up = name.to_ascii_uppercase();
    up.starts_with("LICENSE")
        || up.starts_with("LICENCE")
        || up.starts_with("COPYING")
        || name == "plugin.json"
        || name == "marketplace.json"
        || name == "gemini-extension.json"
}

/// A safe relative path, or why not.
pub fn clean_path(p: &str) -> Result<String, &'static str> {
    if p.is_empty() {
        return Err("empty path");
    }
    if p.contains('\\') {
        return Err("backslash in path");
    }
    if p.starts_with('/') {
        return Err("absolute path");
    }
    if p.bytes().any(|b| b < 0x20) {
        return Err("control character in path");
    }
    let mut parts = Vec::new();
    for c in p.split('/') {
        match c {
            "" | "." => continue,
            ".." => return Err("path escape"),
            c => parts.push(c),
        }
    }
    if parts.is_empty() {
        return Err("empty path");
    }
    Ok(parts.join("/"))
}

/// A reader that errors once more than `cap` bytes came through (the gzip
/// bomb cut).
struct Capped<R> {
    inner: R,
    left: u64,
}

impl<R: Read> Read for Capped<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        if n as u64 > self.left {
            return Err(std::io::Error::other("expanded size cap reached"));
        }
        self.left -= n as u64;
        Ok(n)
    }
}

/// Reads a codeload `tar.gz`: strips the top folder (`repo-sha/`), keeps
/// what `keep` matches (and [`always_keep`]) within `lim`.
pub fn extract(gz: &[u8], keep: &globset::GlobSet, lim: Limits) -> Result<Tree> {
    let dec = flate2::read::GzDecoder::new(gz);
    let capped = Capped {
        inner: dec,
        left: lim.expanded_bytes,
    };
    let mut ar = tar::Archive::new(capped);
    let mut t = Tree::default();
    let mut kept = 0u64;
    let entries = ar
        .entries()
        .map_err(|e| anyhow!("not a tar archive: {e}"))?;
    for e in entries {
        let mut e = match e {
            Ok(e) => e,
            Err(err) => {
                if err.to_string().contains("cap reached") {
                    t.truncated = true;
                    t.skipped.push(("*".into(), "expanded size cap".into()));
                    break;
                }
                bail!("bad tar entry: {err}");
            }
        };
        let raw = String::from_utf8_lossy(&e.path_bytes()).into_owned();
        // Top folder off: "repo-sha/a/b" -> "a/b".
        let rel = match raw.split_once('/') {
            Some((_, r)) => r.to_string(),
            None => continue,
        };
        if rel.is_empty() || rel.ends_with('/') {
            continue;
        }
        let kind = e.header().entry_type();
        let wanted_raw = keep.is_match(&rel) || always_keep(&rel);
        let path = match clean_path(&rel) {
            Ok(p) => p,
            Err(why) => {
                if wanted_raw || rel.contains("..") || rel.starts_with('/') {
                    t.skipped.push((rel.clone(), why.into()));
                }
                continue;
            }
        };
        let wanted = keep.is_match(&path) || always_keep(&path);
        if !wanted {
            continue;
        }
        if !kind.is_file() {
            let why = if kind.is_symlink() {
                "symlink"
            } else if kind.is_hard_link() {
                "hard link"
            } else {
                "not a regular file"
            };
            t.skipped.push((path, why.into()));
            continue;
        }
        let name = path.rsplit('/').next().unwrap_or(&path);
        if credential_name(name) {
            t.skipped.push((path, "credential file".into()));
            continue;
        }
        if path.split('/').count() > lim.depth + 1 {
            t.skipped.push((path, "too deep".into()));
            continue;
        }
        let size = e.header().size().unwrap_or(u64::MAX);
        if size > lim.file_bytes {
            t.skipped.push((path, "file over the size cap".into()));
            continue;
        }
        if t.files.len() >= lim.files {
            t.truncated = true;
            t.skipped.push((path, "file count cap".into()));
            break;
        }
        if kept + size > lim.kept_bytes {
            t.truncated = true;
            t.skipped.push((path, "kept size cap".into()));
            break;
        }
        let mut buf = Vec::with_capacity(size as usize);
        if let Err(err) = (&mut e).take(lim.file_bytes + 1).read_to_end(&mut buf) {
            if err.to_string().contains("cap reached") {
                t.truncated = true;
                t.skipped.push((path, "expanded size cap".into()));
                break;
            }
            bail!("read {path}: {err}");
        }
        kept += buf.len() as u64;
        t.files.insert(path, Bytes::from(buf));
    }
    Ok(t)
}

/// A glob set from patterns (`skills/*/SKILL.md`, `agents/**/*.md`).
/// `*` does not cross `/`; `**` does.
pub fn globs(patterns: &[&str]) -> Result<globset::GlobSet> {
    let mut b = globset::GlobSetBuilder::new();
    for p in patterns {
        b.add(
            globset::GlobBuilder::new(p)
                .literal_separator(true)
                .build()
                .map_err(|e| anyhow!("bad glob {p}: {e}"))?,
        );
    }
    Ok(b.build()?)
}

/// Test helper: a codeload-shaped `tar.gz` (top folder `repo-sha/`) from
/// `(path, bytes)` pairs.
pub fn build_tarball(top: &str, files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut b = tar::Builder::new(flate2::write::GzEncoder::new(
        Vec::new(),
        flate2::Compression::fast(),
    ));
    for (p, data) in files {
        let mut h = tar::Header::new_gnu();
        h.set_size(data.len() as u64);
        h.set_mode(0o644);
        h.set_entry_type(tar::EntryType::Regular);
        h.set_cksum();
        b.append_data(&mut h, format!("{top}/{p}"), *data).unwrap();
    }
    b.into_inner().unwrap().finish().unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pkt(s: &str) -> String {
        format!("{:04x}{s}", s.len() + 4)
    }

    const SHA1: &str = "a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2";
    const SHA2: &str = "b7e0000000000000000000000000000000000001";

    #[test]
    fn refs_parse_and_pick() {
        let body = format!(
            "{}0000{}{}{}{}0000",
            pkt("# service=git-upload-pack\n"),
            pkt(&format!(
                "{SHA1} HEAD\0multi_ack symref=HEAD:refs/heads/main\n"
            )),
            pkt(&format!("{SHA1} refs/heads/main\n")),
            pkt(&format!("{SHA2} refs/tags/v1\n")),
            pkt(&format!("{SHA1} refs/tags/v1^{{}}\n")),
        );
        let refs = parse_refs(body.as_bytes()).unwrap();
        assert_eq!(refs[0], ("HEAD".into(), SHA1.into()));
        assert_eq!(refs.len(), 3);
        assert_eq!(pick_ref(&refs, None).as_deref(), Some(SHA1));
        assert_eq!(pick_ref(&refs, Some("v1")).as_deref(), Some(SHA2));
        assert_eq!(pick_ref(&refs, Some("main")).as_deref(), Some(SHA1));
        assert!(pick_ref(&refs, Some("nope")).is_none());
        assert!(parse_refs(b"<html>").is_err());
        assert!(is_sha(SHA1) && !is_sha("main") && !is_sha(&SHA1.to_uppercase()));
    }

    fn raw_tar(entries: &[(&str, tar::EntryType, &[u8], Option<&str>)]) -> Vec<u8> {
        let mut b = tar::Builder::new(flate2::write::GzEncoder::new(
            Vec::new(),
            flate2::Compression::fast(),
        ));
        for (p, kind, data, link) in entries {
            let mut h = tar::Header::new_gnu();
            h.set_size(data.len() as u64);
            h.set_mode(0o644);
            h.set_entry_type(*kind);
            if let Some(l) = link {
                h.set_link_name(l).unwrap();
            }
            // Write the name bytes directly so "../x" and "/abs" survive.
            {
                let gnu = h.as_gnu_mut().unwrap();
                let n = p.as_bytes();
                gnu.name[..n.len()].copy_from_slice(n);
            }
            h.set_cksum();
            b.append(&h, *data).unwrap();
        }
        b.into_inner().unwrap().finish().unwrap()
    }

    #[test]
    fn extract_keeps_matches_and_refuses_the_bad() {
        use tar::EntryType as E;
        let gz = raw_tar(&[
            (
                "r-sha/skills/pdf/SKILL.md",
                E::Regular,
                b"---\nname: pdf\n---\nx",
                None,
            ),
            ("r-sha/skills/pdf/notes.txt", E::Regular, b"skip", None),
            ("r-sha/LICENSE", E::Regular, b"MIT License", None),
            ("r-sha/skills/../../etc/SKILL.md", E::Regular, b"evil", None),
            (
                "r-sha/skills/ln/SKILL.md",
                E::Symlink,
                b"",
                Some("/etc/passwd"),
            ),
            (
                "r-sha/skills/hl/SKILL.md",
                E::Link,
                b"",
                Some("r-sha/LICENSE"),
            ),
            ("r-sha/skills/dev/SKILL.md", E::Char, b"", None),
            ("r-sha/skills/env/.env", E::Regular, b"TOKEN=x", None),
        ]);
        let keep = globs(&["skills/*/SKILL.md", "skills/**/.env"]).unwrap();
        let t = extract(&gz, &keep, Limits::default()).unwrap();
        let names: Vec<_> = t.files.keys().cloned().collect();
        assert_eq!(names, vec!["LICENSE", "skills/pdf/SKILL.md"]);
        let why: Vec<_> = t.skipped.iter().map(|(_, w)| w.as_str()).collect();
        assert!(why.contains(&"path escape"), "{why:?}");
        assert!(why.contains(&"symlink"));
        assert!(why.contains(&"hard link"));
        assert!(why.contains(&"not a regular file"));
        assert!(why.contains(&"credential file"));
    }

    #[test]
    fn extract_caps() {
        let keep = globs(&["**"]).unwrap();
        // 1001 files: the count cap.
        let many: Vec<(String, Vec<u8>)> = (0..1001)
            .map(|i| (format!("f/{i}.md"), b"x".to_vec()))
            .collect();
        let refs: Vec<(&str, &[u8])> = many
            .iter()
            .map(|(p, d)| (p.as_str(), d.as_slice()))
            .collect();
        let t = extract(&build_tarball("r-sha", &refs), &keep, Limits::default()).unwrap();
        assert_eq!(t.files.len(), 1000);
        assert!(t.truncated);
        // Too deep.
        let t = extract(
            &build_tarball("r-sha", &[("a/b/c/d/e/f/g/h.md", b"x")]),
            &keep,
            Limits::default(),
        )
        .unwrap();
        assert!(t.files.is_empty());
        assert_eq!(t.skipped[0].1, "too deep");
        // A file over 1 MiB.
        let big = vec![b'a'; 1024 * 1024 + 1];
        let t = extract(
            &build_tarball("r-sha", &[("big.md", &big)]),
            &keep,
            Limits::default(),
        )
        .unwrap();
        assert!(t.files.is_empty());
    }

    #[test]
    fn gzip_bomb_is_cut() {
        // 300 MiB of zeros compresses to ~300 KiB; the expanded cap stops it.
        let keep = globs(&["**"]).unwrap();
        let mut b = tar::Builder::new(flate2::write::GzEncoder::new(
            Vec::new(),
            flate2::Compression::fast(),
        ));
        let size: u64 = 300 * 1024 * 1024;
        let mut h = tar::Header::new_gnu();
        h.set_size(size);
        h.set_mode(0o644);
        h.set_entry_type(tar::EntryType::Regular);
        h.set_cksum();
        b.append_data(&mut h, "r-sha/zero.bin", std::io::repeat(0).take(size))
            .unwrap();
        h.set_size(1);
        h.set_cksum();
        b.append_data(&mut h, "r-sha/after.md", &b"x"[..]).unwrap();
        let gz = b.into_inner().unwrap().finish().unwrap();
        assert!(gz.len() < 2 * 1024 * 1024);
        let lim = Limits {
            expanded_bytes: 256 * 1024 * 1024,
            ..Default::default()
        };
        let t = extract(&gz, &keep, lim).unwrap();
        assert!(t.truncated);
        assert!(!t.files.contains_key("after.md"));
    }

    #[test]
    fn clean_paths() {
        assert_eq!(clean_path("a/./b").unwrap(), "a/b");
        assert!(clean_path("a/../b").is_err());
        assert!(clean_path("/a").is_err());
        assert!(clean_path("a\\b").is_err());
        assert!(credential_name("id_rsa.pub") && credential_name(".env.local"));
        assert!(!credential_name("SKILL.md"));
    }
}
