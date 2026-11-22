//! Canonical form and hash, byte for byte with belai `internal/libitem`
//! (`CanonicalMarkdown`, `CanonicalJSON`, `Hash`, `HashBundle`), which
//! follow vdb-site `belaiCanonicalMarkdown` / `belaiCanonicalJSON`.
//!
//! JSON is read by a small parser of our own (numbers kept as written, a
//! repeated key refused, nesting capped) and written the way Go's
//! `encoding/json` encoder writes it (Go 1.22+): compact, keys sorted
//! bytewise, `\b \f \n \r \t` short escapes, other C0 controls as
//! `\u00xx`, U+2028/U+2029 always escaped, HTML escaping only when asked.

use anyhow::{Result, bail};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

/// How deeply a JSON document may nest (belai `MaxJSONDepth`).
pub const MAX_JSON_DEPTH: usize = 8;

/// belai `CanonicalMarkdown`.
pub fn markdown(text: &str) -> Result<String> {
    markdown_bytes(text.as_bytes())
}

/// belai `CanonicalMarkdown` over raw bytes.
pub fn markdown_bytes(raw: &[u8]) -> Result<String> {
    if raw.is_empty() {
        bail!("the document is empty");
    }
    let Ok(s) = std::str::from_utf8(raw) else {
        bail!("the document is not valid UTF-8");
    };
    let text = s.replace("\r\n", "\n");
    for (i, c) in text.char_indices() {
        if c == '\n' || c == '\t' {
            continue;
        }
        if c == '\0' {
            bail!("the document holds a NUL byte");
        }
        if c.is_control() {
            bail!("the document holds a control character at byte {i}");
        }
    }
    let t = text.trim_end_matches(char::is_whitespace);
    Ok(format!("{t}\n"))
}

/// belai `CanonicalJSON`: the top level must be an object.
pub fn json(raw: &[u8], max_depth: usize) -> Result<Vec<u8>> {
    let v = parse_object(raw, max_depth)?;
    let mut out = String::new();
    write_json(&v, false, &mut out);
    out.push('\n');
    Ok(out.into_bytes())
}

/// Lower-case hex sha256.
pub fn hash(doc: &[u8]) -> String {
    hex::encode(Sha256::digest(doc))
}

/// belai `HashBundle`: the document's own hash with no files; else the
/// sha256 of the document, `\n--files--\n` and one `path sha256\n` line per
/// file in path order. `files` are `(path, sha256)`.
pub fn bundle_hash(doc: &[u8], files: &[(&str, &str)]) -> String {
    if files.is_empty() {
        return hash(doc);
    }
    let mut sorted = files.to_vec();
    sorted.sort_by(|a, b| a.0.cmp(b.0));
    let mut b = doc.to_vec();
    b.extend_from_slice(b"\n--files--\n");
    for (p, s) in sorted {
        b.extend_from_slice(p.as_bytes());
        b.push(b' ');
        b.extend_from_slice(s.as_bytes());
        b.push(b'\n');
    }
    hash(&b)
}

// ---- JSON value --------------------------------------------------------------

/// A JSON value with numbers kept as written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Json {
    Null,
    Bool(bool),
    Num(String),
    Str(String),
    Arr(Vec<Json>),
    Obj(BTreeMap<String, Json>),
}

impl Json {
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Json::Str(s) => Some(s),
            _ => None,
        }
    }
    pub fn as_obj(&self) -> Option<&BTreeMap<String, Json>> {
        match self {
            Json::Obj(o) => Some(o),
            _ => None,
        }
    }
    pub fn as_arr(&self) -> Option<&Vec<Json>> {
        match self {
            Json::Arr(a) => Some(a),
            _ => None,
        }
    }
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Json::Bool(b) => Some(*b),
            _ => None,
        }
    }
    /// A whole number, when the value is one written without a fraction.
    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Json::Num(n) => n.parse().ok(),
            _ => None,
        }
    }
    pub fn get(&self, k: &str) -> Option<&Json> {
        self.as_obj().and_then(|o| o.get(k))
    }
    pub fn str_of(&self, k: &str) -> Option<&str> {
        self.get(k).and_then(Json::as_str)
    }
    pub fn obj() -> Self {
        Json::Obj(BTreeMap::new())
    }
    pub fn s(v: impl Into<String>) -> Self {
        Json::Str(v.into())
    }
    /// Sets a key on an object (no-op otherwise).
    pub fn set(&mut self, k: &str, v: Json) {
        if let Json::Obj(o) = self {
            o.insert(k.to_string(), v);
        }
    }

    /// Compact text, Go encoder rules, no HTML escaping.
    pub fn to_compact(&self) -> String {
        let mut s = String::new();
        write_json(self, false, &mut s);
        s
    }

    /// Canonical bytes of an object value (compact + `\n`).
    pub fn canonical(&self) -> Vec<u8> {
        let mut s = self.to_compact();
        s.push('\n');
        s.into_bytes()
    }

    /// From serde_json (numbers as serde writes them).
    pub fn from_serde(v: &serde_json::Value) -> Self {
        match v {
            serde_json::Value::Null => Json::Null,
            serde_json::Value::Bool(b) => Json::Bool(*b),
            serde_json::Value::Number(n) => Json::Num(n.to_string()),
            serde_json::Value::String(s) => Json::Str(s.clone()),
            serde_json::Value::Array(a) => Json::Arr(a.iter().map(Json::from_serde).collect()),
            serde_json::Value::Object(o) => Json::Obj(
                o.iter()
                    .map(|(k, v)| (k.clone(), Json::from_serde(v)))
                    .collect(),
            ),
        }
    }
}

/// Parses one JSON object document (belai's refusals and their order).
pub fn parse_object(raw: &[u8], max_depth: usize) -> Result<Json> {
    let vals = parse_all(raw, max_depth)?;
    if vals.len() > 1 {
        bail!("the document has data after the JSON object");
    }
    match vals.into_iter().next() {
        Some(v @ Json::Obj(_)) => Ok(v),
        _ => bail!("the document must be a JSON object"),
    }
}

/// Parses any JSON value (an array top level allowed); a lenient helper
/// for source files. Duplicate keys keep the last value.
pub fn parse_value(raw: &[u8]) -> Result<Json> {
    let s = std::str::from_utf8(raw).map_err(|_| anyhow::anyhow!("not valid UTF-8"))?;
    let s = s.strip_prefix('\u{feff}').unwrap_or(s);
    let mut p = Parser {
        b: s.as_bytes(),
        i: 0,
        max_depth: 64,
        depth: 0,
        lenient: true,
    };
    p.ws();
    let v = p.value()?;
    p.ws();
    if p.i != p.b.len() {
        bail!("the document is not valid JSON");
    }
    Ok(v)
}

fn parse_all(raw: &[u8], max_depth: usize) -> Result<Vec<Json>> {
    if raw.iter().all(|b| b.is_ascii_whitespace()) {
        bail!("the document is empty");
    }
    let Ok(s) = std::str::from_utf8(raw) else {
        bail!("the document is not valid UTF-8");
    };
    let mut p = Parser {
        b: s.as_bytes(),
        i: 0,
        max_depth,
        depth: 0,
        lenient: false,
    };
    let mut out = Vec::new();
    loop {
        p.ws();
        if p.i >= p.b.len() {
            break;
        }
        out.push(p.value()?);
    }
    Ok(out)
}

struct Parser<'a> {
    b: &'a [u8],
    i: usize,
    max_depth: usize,
    depth: usize,
    lenient: bool,
}

fn invalid<T>() -> Result<T> {
    bail!("the document is not valid JSON")
}

impl Parser<'_> {
    fn ws(&mut self) {
        while self.i < self.b.len() && matches!(self.b[self.i], b' ' | b'\t' | b'\n' | b'\r') {
            self.i += 1;
        }
    }

    fn peek(&self) -> Option<u8> {
        self.b.get(self.i).copied()
    }

    fn lit(&mut self, w: &str, v: Json) -> Result<Json> {
        if self.b[self.i..].starts_with(w.as_bytes()) {
            self.i += w.len();
            Ok(v)
        } else {
            invalid()
        }
    }

    fn value(&mut self) -> Result<Json> {
        match self.peek() {
            Some(b'{') => self.object(),
            Some(b'[') => self.array(),
            Some(b'"') => Ok(Json::Str(self.string()?)),
            Some(b't') => self.lit("true", Json::Bool(true)),
            Some(b'f') => self.lit("false", Json::Bool(false)),
            Some(b'n') => self.lit("null", Json::Null),
            Some(b'-' | b'0'..=b'9') => self.number(),
            _ => invalid(),
        }
    }

    fn enter(&mut self) -> Result<()> {
        if self.depth >= self.max_depth {
            bail!("the document nests more than {} levels", self.max_depth);
        }
        self.depth += 1;
        Ok(())
    }

    fn object(&mut self) -> Result<Json> {
        self.enter()?;
        self.i += 1;
        let mut m = BTreeMap::new();
        self.ws();
        if self.peek() == Some(b'}') {
            self.i += 1;
            self.depth -= 1;
            return Ok(Json::Obj(m));
        }
        loop {
            self.ws();
            if self.peek() != Some(b'"') {
                return invalid();
            }
            let k = self.string()?;
            self.ws();
            if self.peek() != Some(b':') {
                return invalid();
            }
            self.i += 1;
            self.ws();
            let v = self.value()?;
            if m.contains_key(&k) && !self.lenient {
                let shown: String = k
                    .chars()
                    .map(|c| if c.is_control() { '?' } else { c })
                    .take(40)
                    .collect();
                bail!("the key {shown:?} appears twice in one object");
            }
            m.insert(k, v);
            self.ws();
            match self.peek() {
                Some(b',') => self.i += 1,
                Some(b'}') => {
                    self.i += 1;
                    self.depth -= 1;
                    return Ok(Json::Obj(m));
                }
                _ => return invalid(),
            }
        }
    }

    fn array(&mut self) -> Result<Json> {
        self.enter()?;
        self.i += 1;
        let mut a = Vec::new();
        self.ws();
        if self.peek() == Some(b']') {
            self.i += 1;
            self.depth -= 1;
            return Ok(Json::Arr(a));
        }
        loop {
            self.ws();
            a.push(self.value()?);
            self.ws();
            match self.peek() {
                Some(b',') => self.i += 1,
                Some(b']') => {
                    self.i += 1;
                    self.depth -= 1;
                    return Ok(Json::Arr(a));
                }
                _ => return invalid(),
            }
        }
    }

    fn number(&mut self) -> Result<Json> {
        let start = self.i;
        let b = self.b;
        let mut i = self.i;
        if b.get(i) == Some(&b'-') {
            i += 1;
        }
        match b.get(i) {
            Some(b'0') => i += 1,
            Some(b'1'..=b'9') => {
                while matches!(b.get(i), Some(b'0'..=b'9')) {
                    i += 1;
                }
            }
            _ => return invalid(),
        }
        if b.get(i) == Some(&b'.') {
            i += 1;
            if !matches!(b.get(i), Some(b'0'..=b'9')) {
                return invalid();
            }
            while matches!(b.get(i), Some(b'0'..=b'9')) {
                i += 1;
            }
        }
        if matches!(b.get(i), Some(b'e' | b'E')) {
            i += 1;
            if matches!(b.get(i), Some(b'+' | b'-')) {
                i += 1;
            }
            if !matches!(b.get(i), Some(b'0'..=b'9')) {
                return invalid();
            }
            while matches!(b.get(i), Some(b'0'..=b'9')) {
                i += 1;
            }
        }
        self.i = i;
        Ok(Json::Num(
            String::from_utf8_lossy(&b[start..i]).into_owned(),
        ))
    }

    fn hex4(&mut self) -> Result<u32> {
        let Some(h) = self.b.get(self.i..self.i + 4) else {
            return invalid();
        };
        let s = std::str::from_utf8(h).map_err(|_| anyhow::anyhow!("bad escape"))?;
        let v = u32::from_str_radix(s, 16).or_else(|_| invalid())?;
        self.i += 4;
        Ok(v)
    }

    fn string(&mut self) -> Result<String> {
        self.i += 1;
        let mut out = String::new();
        loop {
            let Some(c) = self.peek() else {
                return invalid();
            };
            match c {
                b'"' => {
                    self.i += 1;
                    return Ok(out);
                }
                b'\\' => {
                    self.i += 1;
                    let Some(e) = self.peek() else {
                        return invalid();
                    };
                    self.i += 1;
                    match e {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{8}'),
                        b'f' => out.push('\u{c}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => {
                            let u = self.hex4()?;
                            if (0xD800..0xDC00).contains(&u) {
                                // A pair, or U+FFFD for a lone half (as Go).
                                if self.b[self.i..].starts_with(b"\\u") {
                                    let save = self.i;
                                    self.i += 2;
                                    let lo = self.hex4()?;
                                    if (0xDC00..0xE000).contains(&lo) {
                                        let cp = 0x10000 + ((u - 0xD800) << 10) + (lo - 0xDC00);
                                        out.push(char::from_u32(cp).unwrap_or('\u{fffd}'));
                                    } else {
                                        out.push('\u{fffd}');
                                        self.i = save;
                                    }
                                } else {
                                    out.push('\u{fffd}');
                                }
                            } else {
                                out.push(char::from_u32(u).unwrap_or('\u{fffd}'));
                            }
                        }
                        _ => return invalid(),
                    }
                }
                0..=0x1f => return invalid(),
                _ => {
                    // A whole UTF-8 char (the input is valid UTF-8).
                    let rest = std::str::from_utf8(&self.b[self.i..]).unwrap_or_default();
                    let ch = rest.chars().next().unwrap_or('\u{fffd}');
                    out.push(ch);
                    self.i += ch.len_utf8().max(1);
                }
            }
        }
    }
}

/// Writes a string the way Go's encoder does.
pub fn write_str(s: &str, html: bool, out: &mut String) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            '<' | '>' | '&' if html => {
                let b = c as u8;
                out.push_str("\\u00");
                out.push(HEX[(b >> 4) as usize] as char);
                out.push(HEX[(b & 0xf) as usize] as char);
            }
            c if (c as u32) < 0x20 => {
                let b = c as u8;
                out.push_str("\\u00");
                out.push(HEX[(b >> 4) as usize] as char);
                out.push(HEX[(b & 0xf) as usize] as char);
            }
            '\u{2028}' => out.push_str("\\u2028"),
            '\u{2029}' => out.push_str("\\u2029"),
            c => out.push(c),
        }
    }
    out.push('"');
}

/// Compact JSON, Go encoder rules.
pub fn write_json(v: &Json, html: bool, out: &mut String) {
    match v {
        Json::Null => out.push_str("null"),
        Json::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Json::Num(n) => out.push_str(n),
        Json::Str(s) => write_str(s, html, out),
        Json::Arr(a) => {
            out.push('[');
            for (i, x) in a.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_json(x, html, out);
            }
            out.push(']');
        }
        Json::Obj(o) => {
            out.push('{');
            for (i, (k, x)) in o.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_str(k, html, out);
                out.push(':');
                write_json(x, html, out);
            }
            out.push('}');
        }
    }
}

// ---- text sanitising ------------------------------------------------------------

/// Bidi controls, zero-width characters and tag characters.
pub fn hidden_char(c: char) -> bool {
    matches!(c,
        '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}' | '\u{200E}' | '\u{200F}' | '\u{061C}'
        | '\u{200B}'..='\u{200D}' | '\u{2060}'..='\u{2064}' | '\u{FEFF}' | '\u{180E}'
        | '\u{E0000}'..='\u{E007F}')
}

/// CRLF and lone CR become LF; hidden characters and controls other than
/// tab and LF are removed. The flag says whether anything was removed.
pub fn clean_text(s: &str) -> (String, bool) {
    let s = s.replace("\r\n", "\n").replace('\r', "\n");
    let mut hidden = false;
    let out: String = s
        .chars()
        .filter(|c| {
            let drop = hidden_char(*c) || (c.is_control() && *c != '\n' && *c != '\t');
            hidden |= drop;
            !drop
        })
        .collect();
    (out, hidden)
}

/// One line: whitespace runs folded to one space, trimmed.
pub fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Cuts `s` to at most `max` bytes at a char boundary.
pub fn clip(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut cut = max;
    while cut > 0 && !s.is_char_boundary(cut) {
        cut -= 1;
    }
    &s[..cut]
}

/// A belai item name (`^[a-z0-9][a-z0-9._-]{0,63}$`) from free text.
pub fn slug(s: &str) -> String {
    let mut out = String::new();
    for c in s.trim().chars().flat_map(char::to_lowercase) {
        if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
            out.push(c);
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    let t = out.trim_matches(|c| matches!(c, '-' | '.' | '_'));
    let t = clip(t, 64);
    t.trim_end_matches(['-', '.', '_']).to_string()
}

/// An MCP name (`^[A-Za-z0-9_-]{1,32}$`) from free text.
pub fn mcp_slug(s: &str) -> String {
    let mut out = String::new();
    for c in s.trim().chars() {
        if c.is_ascii_alphanumeric() || matches!(c, '_' | '-') {
            out.push(c);
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    let t = out.trim_matches('-');
    clip(t, 32).trim_end_matches('-').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markdown_vectors() {
        let ok = [
            ("a\nb\n", "a\nb\n"),
            ("a\r\nb\r\n", "a\nb\n"),
            ("a\n\n  \t\n\n", "a\n"),
            ("a", "a\n"),
            ("a  \n\n  b", "a  \n\n  b\n"),
            ("  a", "  a\n"),
            ("a\tb", "a\tb\n"),
            ("a\u{a0}\u{a0}", "a\n"),
            (" \r\n\t\n", "\n"),
            ("café ✓\n", "café ✓\n"),
        ];
        for (i, w) in ok {
            assert_eq!(markdown(i).unwrap(), w, "{i:?}");
            assert_eq!(markdown(w).unwrap(), w);
        }
        for (i, e) in [
            ("a\rb", "control character"),
            ("a\x1b[0mb", "control character"),
            ("a\x7fb", "control character"),
            ("a\u{85}b", "control character"),
            ("", "empty"),
            ("a\0b", "NUL"),
        ] {
            assert!(markdown(i).unwrap_err().to_string().contains(e), "{i:?}");
        }
        assert!(
            markdown_bytes(b"a\xffb")
                .unwrap_err()
                .to_string()
                .contains("UTF-8")
        );
        let first = markdown("x: 1\n\nbody").unwrap();
        for s in [
            "x: 1\r\n\r\nbody\r\n",
            "x: 1\n\nbody\n\n\n",
            "x: 1\n\nbody \t\n",
        ] {
            assert_eq!(markdown(s).unwrap(), first);
        }
    }

    #[test]
    fn json_vectors() {
        let ok: &[(&str, &str)] = &[
            ("{ \"b\": 1,\n \"a\": [1, 2] }", r#"{"a":[1,2],"b":1}"#),
            (
                r#"{"z":{"b":1,"a":2},"a":{}}"#,
                r#"{"a":{},"z":{"a":2,"b":1}}"#,
            ),
            (r#"{"a":"<b>&</b>"}"#, r#"{"a":"<b>&</b>"}"#),
            (
                r#"{"a":1.50,"b":1e3,"c":12345678901234567890}"#,
                r#"{"a":1.50,"b":1e3,"c":12345678901234567890}"#,
            ),
            (r#"{"a":"café"}"#, r#"{"a":"café"}"#),
            (r#"{"a":null,"b":true}"#, r#"{"a":null,"b":true}"#),
            (
                r#"{"a":[{"y":1,"x":2},{"x":3}]}"#,
                r#"{"a":[{"x":2,"y":1},{"x":3}]}"#,
            ),
            ("{}", "{}"),
            (
                r#"{"x":{"a":1},"y":{"a":2}}"#,
                r#"{"x":{"a":1},"y":{"a":2}}"#,
            ),
            (
                r#"{"a":{"b":{"c":{"d":{"e":{"f":{"g":{"h":1}}}}}}}}"#,
                r#"{"a":{"b":{"c":{"d":{"e":{"f":{"g":{"h":1}}}}}}}}"#,
            ),
            (
                "{\"a\":\"x\u{2028}y\"}",
                concat!(r#"{"a":"x"#, "\\", r#"u2028y"}"#),
            ),
            (r#"{"a":"\u0001\b\f\u001f"}"#, r#"{"a":"\u0001\b\f\u001f"}"#),
            (r#"{"a":"\/"}"#, r#"{"a":"/"}"#),
        ];
        for (i, w) in ok {
            let got = String::from_utf8(json(i.as_bytes(), MAX_JSON_DEPTH).unwrap()).unwrap();
            assert_eq!(got, format!("{w}\n"), "{i}");
            let again = json(got.as_bytes(), MAX_JSON_DEPTH).unwrap();
            assert_eq!(again, got.as_bytes());
        }
        let bad: &[(&[u8], &str)] = &[
            (b"[1]", "object"),
            (b"\"x\"", "object"),
            (b"1", "object"),
            (b"{} {}", "after"),
            (b"{}x", "not valid JSON"),
            (br#"{"a":1,"a":2}"#, r#"the key "a" appears twice"#),
            (br#"{"x":{"b":1,"b":1}}"#, r#"the key "b" appears twice"#),
            (
                br#"{"a":{"b":{"c":{"d":{"e":{"f":{"g":{"h":{"i":1}}}}}}}}}"#,
                "nests more than 8 levels",
            ),
            (br#"{"a":[[[[[[[[1]]]]]]]]}"#, "nests more than 8 levels"),
            (br#"{"a":"#, "JSON"),
            (b"", "empty"),
            (b" \n", "empty"),
            (b"{\"a\":\"\xff\"}", "UTF-8"),
        ];
        for (i, e) in bad {
            let err = json(i, MAX_JSON_DEPTH).unwrap_err().to_string();
            assert!(err.contains(e), "{:?}: {err}", String::from_utf8_lossy(i));
        }
    }

    /// belai `TestHashVectors` and `TestHashBundleGolden`.
    #[test]
    fn hash_vectors() {
        assert_eq!(
            hash(b"abc\n"),
            "edeaaff3f1774ad2888673770c6d64097e391bc362d7d6fb34982ddf0efd18cb"
        );
        let c = json(br#"{"b":2,"a":1}"#, MAX_JSON_DEPTH).unwrap();
        assert_eq!(
            hash(&c),
            "e8d38819d39f705646bfb643368eca78f7db476c16471dbc33b941b27326410d"
        );
        let doc = b"{\"hooks\":{\"Stop\":[{\"hooks\":[{\"command\":\"a.sh\",\"type\":\"command\"}]}]},\"name\":\"x\"}\n";
        assert_eq!(bundle_hash(doc, &[]), hash(doc));
        let a = "a".repeat(64);
        let b = "b".repeat(64);
        let files = [("scripts/b.sh", b.as_str()), ("a.sh", a.as_str())];
        assert_eq!(
            bundle_hash(doc, &files),
            "341011b2221be4c90e63b86c5f7078e03daf4ed74230f576a9e8251840e57202"
        );
        assert_eq!(
            bundle_hash(doc, &[files[1], files[0]]),
            bundle_hash(doc, &files)
        );
    }

    #[test]
    fn hash_ignores_formatting() {
        let a = json(br#"{"name":"x","args":["a","b"]}"#, 8).unwrap();
        let b = json(b"{\n  \"args\": [\"a\", \"b\"],\n  \"name\": \"x\"\n}\n", 8).unwrap();
        assert_eq!(hash(&a), hash(&b));
    }

    #[test]
    fn text_helpers() {
        let (t, h) = clean_text("a\u{200B}b\u{202E}c\r\nd\u{E0041}");
        assert_eq!(t, "abc\nd");
        assert!(h);
        assert_eq!(slug("Code Reviewer!"), "code-reviewer");
        assert_eq!(slug("--My_Skill.."), "my_skill");
        assert_eq!(
            mcp_slug("io.github.x/GitHub Server"),
            "io-github-x-GitHub-Server"
        );
        let mut s = String::new();
        write_str("<a>&", true, &mut s);
        assert_eq!(s, ["\"", "\\u003ca", "\\u003e", "\\u0026", "\""].concat());
    }
}
