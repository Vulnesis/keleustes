//! SPDX by normalised-text fingerprints: lower-case words joined by single
//! spaces, matched against phrases each licence text carries.

/// Lower-case alphanumeric words joined by one space.
pub fn normalise(text: &str) -> String {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// The SPDX id whose fingerprint the text carries.
pub fn detect(text: &str) -> Option<String> {
    let t = normalise(text);
    let has = |p: &str| t.contains(p);
    let id = if has("gnu affero general public license") {
        "AGPL-3.0"
    } else if has("gnu lesser general public license") || has("gnu library general public license")
    {
        if has("version 3") {
            "LGPL-3.0"
        } else {
            "LGPL-2.1"
        }
    } else if has("gnu general public license") {
        if has("version 3") {
            "GPL-3.0"
        } else {
            "GPL-2.0"
        }
    } else if has("mozilla public license version 2 0") || has("mozilla public license 2 0") {
        "MPL-2.0"
    } else if has("apache license")
        && (has("version 2 0") || has("apache org licenses license 2 0"))
    {
        "Apache-2.0"
    } else if has("permission is hereby granted free of charge to any person obtaining a copy")
        && has("the above copyright notice and this permission notice shall be included")
    {
        "MIT"
    } else if has(
        "permission to use copy modify and or distribute this software for any purpose with or without fee is hereby granted",
    ) {
        if has("permission notice appear in all copies")
            || has("the above copyright notice and this permission notice appear")
        {
            "ISC"
        } else {
            "0BSD"
        }
    } else if has(
        "redistribution and use in source and binary forms with or without modification are permitted",
    ) {
        if has("neither the name of") || has("names of its contributors may be used") {
            "BSD-3-Clause"
        } else {
            "BSD-2-Clause"
        }
    } else if has("cc0 1 0 universal") || has("creative commons legal code cc0") {
        "CC0-1.0"
    } else if has("this is free and unencumbered software released into the public domain") {
        "Unlicense"
    } else if has("attribution sharealike 4 0 international") {
        "CC-BY-SA-4.0"
    } else if has("attribution 4 0 international") && has("creative commons") {
        "CC-BY-4.0"
    } else if has("all rights reserved") && !has("permission is hereby granted")
        || has("proprietary")
    {
        "Proprietary"
    } else {
        return None;
    };
    Some(id.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprints() {
        let mit = "MIT License\n\nCopyright (c) 2025 X\n\nPermission is hereby granted, free of charge, to any person obtaining a copy\nof this software... The above copyright notice and this permission notice shall be included in all copies.";
        assert_eq!(detect(mit).as_deref(), Some("MIT"));
        assert_eq!(
            detect("Apache License\nVersion 2.0, January 2004\nhttp://www.apache.org/licenses/")
                .as_deref(),
            Some("Apache-2.0")
        );
        assert_eq!(detect("GNU LESSER GENERAL PUBLIC LICENSE Version 3, 29 June 2007 ... GNU General Public License").as_deref(), Some("LGPL-3.0"));
        assert_eq!(
            detect("GNU GENERAL PUBLIC LICENSE Version 2, June 1991").as_deref(),
            Some("GPL-2.0")
        );
        assert_eq!(
            detect("GNU AFFERO GENERAL PUBLIC LICENSE Version 3").as_deref(),
            Some("AGPL-3.0")
        );
        assert_eq!(detect("Redistribution and use in source and binary forms, with or without modification, are permitted provided... Neither the name of").as_deref(), Some("BSD-3-Clause"));
        assert_eq!(detect("Redistribution and use in source and binary forms, with or without modification, are permitted provided that").as_deref(), Some("BSD-2-Clause"));
        assert_eq!(detect("Permission to use, copy, modify, and/or distribute this software for any purpose with or without fee is hereby granted, provided that the above copyright notice and this permission notice appear in all copies.").as_deref(), Some("ISC"));
        assert_eq!(
            detect("Creative Commons Legal Code\n\nCC0 1.0 Universal").as_deref(),
            Some("CC0-1.0")
        );
        assert_eq!(
            detect("This is free and unencumbered software released into the public domain.")
                .as_deref(),
            Some("Unlicense")
        );
        assert_eq!(
            detect("Mozilla Public License Version 2.0").as_deref(),
            Some("MPL-2.0")
        );
        assert_eq!(
            detect("Attribution-ShareAlike 4.0 International").as_deref(),
            Some("CC-BY-SA-4.0")
        );
        assert_eq!(
            detect("Creative Commons Attribution 4.0 International Public License").as_deref(),
            Some("CC-BY-4.0")
        );
        assert_eq!(
            detect("© Anthropic. All rights reserved.").as_deref(),
            Some("Proprietary")
        );
        assert_eq!(detect("hello"), None);
    }
}
