// SPDX-License-Identifier: AGPL-3.0-only
//! A file's mime type from its extension, and the icon names that go with it.
//!
//! The tree has no mime helper and `shared-mime-info`'s database is a
//! dependency's worth of parsing for what an icon needs, so this is a small
//! table of the types a desktop actually holds. Anything else is `None` and
//! draws the generic file icon; `xdg-open` does the real detection on open.

/// Extension (lowercase) to mime type.
const TABLE: &[(&str, &str)] = &[
    ("txt", "text/plain"),
    ("md", "text/markdown"),
    ("log", "text/plain"),
    ("csv", "text/csv"),
    ("html", "text/html"),
    ("htm", "text/html"),
    ("css", "text/css"),
    ("rs", "text/rust"),
    ("py", "text/x-python"),
    ("sh", "application/x-shellscript"),
    ("json", "application/json"),
    ("xml", "application/xml"),
    ("toml", "application/toml"),
    ("pdf", "application/pdf"),
    ("zip", "application/zip"),
    ("gz", "application/gzip"),
    ("xz", "application/x-xz"),
    ("zst", "application/zstd"),
    ("tar", "application/x-tar"),
    ("7z", "application/x-7z-compressed"),
    ("iso", "application/x-cd-image"),
    ("odt", "application/vnd.oasis.opendocument.text"),
    ("ods", "application/vnd.oasis.opendocument.spreadsheet"),
    ("doc", "application/msword"),
    (
        "docx",
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
    ),
    (
        "xlsx",
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
    ),
    ("png", "image/png"),
    ("jpg", "image/jpeg"),
    ("jpeg", "image/jpeg"),
    ("gif", "image/gif"),
    ("webp", "image/webp"),
    ("svg", "image/svg+xml"),
    ("bmp", "image/bmp"),
    ("mp3", "audio/mpeg"),
    ("flac", "audio/flac"),
    ("ogg", "audio/ogg"),
    ("wav", "audio/x-wav"),
    ("mp4", "video/mp4"),
    ("mkv", "video/x-matroska"),
    ("webm", "video/webm"),
    ("avi", "video/x-msvideo"),
    ("desktop", "application/x-desktop"),
];

/// The mime type for `name`'s extension, if the table knows it.
pub fn of(name: &str) -> Option<&'static str> {
    let (stem, ext) = name.rsplit_once('.')?;
    if stem.is_empty() {
        return None;
    }
    let ext = ext.to_ascii_lowercase();
    TABLE.iter().find(|(e, _)| *e == ext).map(|(_, m)| *m)
}

/// Icon names to try, most specific first, for a mime type: the type itself
/// (`image/png` -> `image-png`), then its media family (`image-x-generic`),
/// then the generic file.
pub fn icon_names(mime: Option<&str>) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(m) = mime {
        out.push(m.replace('/', "-"));
        if let Some((family, _)) = m.split_once('/') {
            match family {
                "text" => out.push("text-x-generic".to_owned()),
                "image" | "audio" | "video" => out.push(format!("{family}-x-generic")),
                _ => out.push("application-x-generic".to_owned()),
            }
        }
    }
    out.push("text-x-generic".to_owned());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_extensions_ignore_case_and_unknown_are_none() {
        assert_eq!(of("a.PNG"), Some("image/png"));
        assert_eq!(of("archive.tar.gz"), Some("application/gzip"));
        assert_eq!(of("noext"), None);
        assert_eq!(of(".bashrc"), None);
        assert_eq!(of("x.unknownext"), None);
    }

    #[test]
    fn icon_names_end_at_the_generic_file() {
        let n = icon_names(Some("image/png"));
        assert_eq!(n, ["image-png", "image-x-generic", "text-x-generic"]);
        assert_eq!(icon_names(None), ["text-x-generic"]);
    }
}
