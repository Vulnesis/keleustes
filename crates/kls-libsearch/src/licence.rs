//! Licence detection (SPDX by normalised-text fingerprints) and policy.
//!
//! Detection order (first match wins): item front matter `license`;
//! `LICENSE*` in the item dir; `plugin.json` / `gemini-extension.json` /
//! marketplace entry `license`; index metadata; repo root `LICENSE`; with
//! `gh`, the licence API.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum LicenceClass {
    /// MIT, Apache-2.0, BSD, ISC, CC0, Unlicense, MPL-2.0 (weak), CC-BY.
    Permissive,
    /// GPL, LGPL, AGPL, CC-BY-SA: acknowledge (amber).
    Copyleft,
    /// Proprietary, NOASSERTION, "all rights reserved": acknowledge (red).
    Restricted,
    /// None found: acknowledge (red).
    #[default]
    Unknown,
}

/// A detected licence.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Licence {
    /// SPDX id as found or matched (`MIT`, `Apache-2.0`), or the raw value
    /// (`Proprietary`), or empty when unknown.
    pub spdx: String,
    pub class: LicenceClass,
    /// Where it came from: `frontmatter`, `LICENSE`, `plugin.json`,
    /// `index`, `repo`, `gh`.
    pub from: String,
}

impl Licence {
    pub fn unknown() -> Self {
        Self::default()
    }

    /// From an SPDX-ish string (`mit`, `Apache-2.0`, `Proprietary`).
    pub fn from_id(id: &str, from: &str) -> Self {
        let id = id.trim();
        if id.is_empty() {
            return Self::unknown();
        }
        let spdx = normalise_id(id);
        Self {
            class: classify(&spdx),
            spdx,
            from: from.to_string(),
        }
    }

    pub fn known(&self) -> bool {
        self.class != LicenceClass::Unknown
    }

    pub fn permissive(&self) -> bool {
        self.class == LicenceClass::Permissive
    }

    /// The review step must acknowledge anything not permissive.
    pub fn needs_ack(&self) -> bool {
        !self.permissive()
    }

    /// `MIT`, or `?` when unknown.
    pub fn short(&self) -> &str {
        if self.spdx.is_empty() {
            "?"
        } else {
            &self.spdx
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum LicencePolicy {
    #[default]
    Any,
    Permissive,
    KnownOnly,
}

impl LicencePolicy {
    pub fn admits(self, l: &Licence) -> bool {
        match self {
            LicencePolicy::Any => true,
            LicencePolicy::Permissive => l.permissive(),
            LicencePolicy::KnownOnly => l.known(),
        }
    }
}

/// Canonical spelling for common ids (`mit` → `MIT`, `apache 2.0` →
/// `Apache-2.0`); anything else is kept as given.
pub fn normalise_id(id: &str) -> String {
    let k: String = id
        .trim()
        .to_ascii_lowercase()
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    let k = k
        .trim_end_matches(".txt")
        .trim_end_matches(".md")
        .to_string();
    let m = match k.as_str() {
        "mit" | "mitlicense" | "expat" => "MIT",
        "apache-2.0" | "apache2.0" | "apache2" | "apache-2" | "apachelicense2.0" | "apache" => {
            "Apache-2.0"
        }
        "bsd-2-clause" | "bsd2" | "bsd-2" => "BSD-2-Clause",
        "bsd-3-clause" | "bsd3" | "bsd-3" | "bsd" => "BSD-3-Clause",
        "isc" => "ISC",
        "cc0-1.0" | "cc0" | "cc0-1" => "CC0-1.0",
        "unlicense" | "theunlicense" => "Unlicense",
        "mpl-2.0" | "mpl2.0" | "mpl" => "MPL-2.0",
        "gpl-2.0" | "gpl-2.0-only" | "gpl-2.0-or-later" | "gpl2" | "gplv2" => "GPL-2.0",
        "gpl-3.0" | "gpl-3.0-only" | "gpl-3.0-or-later" | "gpl3" | "gplv3" | "gpl" => "GPL-3.0",
        "lgpl-2.1" | "lgpl-2.1-only" | "lgpl-2.1-or-later" | "lgpl" => "LGPL-2.1",
        "lgpl-3.0" | "lgpl-3.0-only" | "lgpl-3.0-or-later" => "LGPL-3.0",
        "agpl-3.0" | "agpl-3.0-only" | "agpl-3.0-or-later" | "agpl" | "agplv3" => "AGPL-3.0",
        "cc-by-4.0" | "ccby4.0" | "cc-by" => "CC-BY-4.0",
        "cc-by-sa-4.0" | "ccbysa4.0" | "cc-by-sa" => "CC-BY-SA-4.0",
        "noassertion" => "NOASSERTION",
        "proprietary" => "Proprietary",
        _ => return id.trim().to_string(),
    };
    m.to_string()
}

/// The policy class of an SPDX id.
pub fn classify(spdx: &str) -> LicenceClass {
    let s = spdx.to_ascii_lowercase();
    if s.is_empty() {
        return LicenceClass::Unknown;
    }
    if s.contains("sa-") || s.starts_with("gpl") || s.starts_with("lgpl") || s.starts_with("agpl") {
        return LicenceClass::Copyleft;
    }
    const PERMISSIVE: [&str; 12] = [
        "mit",
        "apache-2.0",
        "bsd-2-clause",
        "bsd-3-clause",
        "isc",
        "cc0-1.0",
        "unlicense",
        "mpl-2.0",
        "cc-by-4.0",
        "0bsd",
        "zlib",
        "bsl-1.0",
    ];
    if PERMISSIVE.contains(&s.as_str()) {
        return LicenceClass::Permissive;
    }
    LicenceClass::Restricted
}

/// Matches licence text against fingerprints; the SPDX id when one fits.
pub fn detect_text(text: &str) -> Option<String> {
    crate::licence_text::detect(text)
}

/// Detects an item's licence; the first match wins:
/// 1. `frontmatter` (the item's own `license` field);
/// 2. `LICENSE*` in the item dir (top-level files of `f.files`);
/// 3. `plugin.json` / `gemini-extension.json` / `marketplace.json`
///    `license` in `f.files`;
/// 4. index metadata (`f.licence_hint`);
/// 5. `LICENSE*` up the tree (`f.licence_files`, repo root last).
///
/// A front-matter value that only points at a file ("LICENSE.txt has the
/// terms") falls through to the file.
pub fn detect(f: &crate::source::Fetched, frontmatter: Option<&str>) -> Licence {
    if let Some(v) = frontmatter.map(str::trim).filter(|v| !v.is_empty()) {
        let lower = v.to_ascii_lowercase();
        if lower.starts_with("proprietary") {
            return Licence::from_id("Proprietary", "frontmatter");
        }
        let l = Licence::from_id(v, "frontmatter");
        let single_word = !v.contains(' ');
        if l.class != LicenceClass::Restricted || single_word {
            if l.class == LicenceClass::Restricted
                && let Some(id) = detect_text(v)
            {
                return Licence::from_id(&id, "frontmatter");
            }
            return l;
        }
        if let Some(id) = detect_text(v) {
            return Licence::from_id(&id, "frontmatter");
        }
    }
    let is_licence = |p: &str| {
        let up = p.rsplit('/').next().unwrap_or(p).to_ascii_uppercase();
        up.starts_with("LICENSE") || up.starts_with("LICENCE") || up.starts_with("COPYING")
    };
    // 2: the item dir (shallowest licence file).
    let mut item_lic: Vec<&String> = f.files.keys().filter(|k| is_licence(k)).collect();
    item_lic.sort_by_key(|k| k.matches('/').count());
    for k in item_lic {
        if let Some(t) = f.text(k)
            && let Some(id) = detect_text(t)
        {
            return Licence::from_id(&id, "LICENSE");
        }
    }
    // 3: manifests.
    for (k, b) in &f.files {
        let base = k.rsplit('/').next().unwrap_or(k);
        if matches!(
            base,
            "plugin.json" | "gemini-extension.json" | "marketplace.json" | "package.json"
        ) && let Ok(v) = serde_json::from_slice::<serde_json::Value>(b)
            && let Some(l) = v.get("license").and_then(|x| x.as_str())
            && !l.trim().is_empty()
        {
            return Licence::from_id(l, base);
        }
    }
    // 4: index metadata.
    if let Some(h) = f.licence_hint.as_deref().filter(|h| !h.trim().is_empty()) {
        return Licence::from_id(h, "index");
    }
    // 5: up the tree, nearest first.
    for (p, b) in &f.licence_files {
        if let Ok(t) = std::str::from_utf8(b)
            && let Some(id) = detect_text(t)
        {
            let from = if p.contains('/') { "LICENSE" } else { "repo" };
            return Licence::from_id(&id, from);
        }
    }
    Licence::unknown()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::Fetched;
    use bytes::Bytes;

    const MIT: &str = "Permission is hereby granted, free of charge, to any person obtaining a copy of this software. The above copyright notice and this permission notice shall be included in all copies.";

    #[test]
    fn detection_order() {
        let mut f = Fetched::default();
        assert_eq!(detect(&f, None).class, LicenceClass::Unknown);
        f.licence_files.push(("LICENSE".into(), Bytes::from(MIT)));
        assert_eq!(detect(&f, None).spdx, "MIT");
        f.licence_hint = Some("apache-2.0".into());
        assert_eq!(detect(&f, None).spdx, "Apache-2.0");
        f.files
            .insert("plugin.json".into(), Bytes::from(r#"{"license":"ISC"}"#));
        assert_eq!(detect(&f, None).spdx, "ISC");
        f.files
            .insert("LICENSE.txt".into(), Bytes::from("All rights reserved."));
        assert_eq!(detect(&f, None).class, LicenceClass::Restricted);
        assert_eq!(detect(&f, Some("mit")).spdx, "MIT");
        let p = detect(&f, Some("Proprietary. LICENSE.txt has complete terms"));
        assert_eq!(
            (p.spdx.as_str(), p.class),
            ("Proprietary", LicenceClass::Restricted)
        );
        // A pointer to the file falls through to the file.
        assert_eq!(
            detect(&f, Some("Complete terms in LICENSE.txt")).class,
            LicenceClass::Restricted
        );
        assert_eq!(classify("GPL-3.0"), LicenceClass::Copyleft);
        assert!(Licence::from_id("CC-BY-SA-4.0", "x").needs_ack());
    }
}
