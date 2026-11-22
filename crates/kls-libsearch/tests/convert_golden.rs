//! Golden converter tests: one case per (type × ecosystem) row of
//! `docs/gaps/library-search.md` §3, under
//! `tests/fixtures/convert/<type>/<case>/{in/…, case.json?, out.doc,
//! out.notes.json, out.flags.json}`. `<case>` is the ecosystem unless
//! `case.json` names one. Re-bless with `KLS_BLESS=1`.
//!
//! Every converted document is also a fixed point: feeding it back through
//! `from_edited` gives the same bytes and hash.

use bytes::Bytes;
use kls_libsearch::convert::{Converted, Opts, convert, from_edited};
use kls_libsearch::{Ecosystem, Fetched, ItemType};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

fn walk(dir: &Path, root: &Path, out: &mut BTreeMap<String, Bytes>) {
    for e in std::fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            walk(&p, root, out);
        } else {
            let rel = p
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            out.insert(rel, Bytes::from(std::fs::read(&p).unwrap()));
        }
    }
}

struct Case {
    dir: PathBuf,
    item_type: ItemType,
    eco: Ecosystem,
    src: Fetched,
}

fn cases() -> Vec<Case> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/convert");
    let mut out = vec![];
    for t in std::fs::read_dir(&root).unwrap() {
        let t = t.unwrap().path();
        let item_type = ItemType::parse(t.file_name().unwrap().to_str().unwrap()).unwrap();
        for c in std::fs::read_dir(&t).unwrap() {
            let dir = c.unwrap().path();
            let case: serde_json::Value = std::fs::read(dir.join("case.json"))
                .ok()
                .and_then(|b| serde_json::from_slice(&b).ok())
                .unwrap_or_default();
            let name = dir.file_name().unwrap().to_str().unwrap().to_string();
            let eco = Ecosystem::parse(case["eco"].as_str().unwrap_or(&name))
                .unwrap_or_else(|| panic!("{name}: no ecosystem"));
            let mut files = BTreeMap::new();
            walk(&dir.join("in"), &dir.join("in"), &mut files);
            let src = Fetched {
                file_urls: files
                    .keys()
                    .map(|k| {
                        (
                            k.clone(),
                            format!(
                                "https://raw.githubusercontent.com/o/r/{}/{k}",
                                "a".repeat(40)
                            ),
                        )
                    })
                    .collect(),
                files,
                name_hint: case["name_hint"].as_str().unwrap_or_default().to_string(),
                primary: case["primary"].as_str().map(str::to_string),
                licence_hint: case["licence_hint"].as_str().map(str::to_string),
                ..Default::default()
            };
            out.push(Case {
                dir,
                item_type,
                eco,
                src,
            });
        }
    }
    out.sort_by(|a, b| a.dir.cmp(&b.dir));
    out
}

fn notes_json(c: &Converted) -> String {
    let v: Vec<serde_json::Value> = c
        .notes
        .iter()
        .map(|n| serde_json::json!({"kind": n.kind.as_str(), "field": n.field, "text": n.text}))
        .collect();
    serde_json::to_string_pretty(&v).unwrap() + "\n"
}

fn flags_json(c: &Converted) -> String {
    let v: Vec<String> = c.flags.iter().map(|f| f.key()).collect();
    serde_json::to_string_pretty(&v).unwrap() + "\n"
}

#[test]
fn golden_conversions() {
    let bless = std::env::var("KLS_BLESS").is_ok();
    let mut failures = vec![];
    let all = cases();
    assert!(all.len() >= 40, "only {} cases", all.len());
    for case in all {
        let label = case.dir.display().to_string();
        let c = convert(case.item_type, case.eco, &case.src, &Opts::default())
            .unwrap_or_else(|e| panic!("{label}: {e:#}"));
        let got = [
            ("out.doc", c.doc_text()),
            ("out.notes.json", notes_json(&c)),
            ("out.flags.json", flags_json(&c)),
        ];
        for (f, text) in got {
            let p = case.dir.join(f);
            if bless {
                std::fs::write(&p, &text).unwrap();
                continue;
            }
            let want = std::fs::read_to_string(&p).unwrap_or_default();
            if want != text {
                failures.push(format!("{label}/{f}:\n--- want\n{want}\n--- got\n{text}"));
            }
        }
        // Fixed point (refused items carry no valid document).
        if c.refused().is_none() {
            let again = from_edited(&c, &c.doc_text()).unwrap_or_else(|e| {
                panic!("{label}: canonical output does not re-validate: {e:#}")
            });
            assert_eq!(again.doc, c.doc, "{label}: not a fixed point");
            assert_eq!(again.sha256, c.sha256, "{label}");
        }
        assert_eq!(c.upstream, case.src.upstream);
        assert_eq!(c.source_files.len(), case.src.files.len(), "{label}");
    }
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}
