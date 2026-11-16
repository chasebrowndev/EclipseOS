// SPDX-License-Identifier: Apache-2.0
//! Argv arrays with typed placeholders (T-11 §2).
//!
//! An argv is a list of elements; each is a literal string or exactly one placeholder.
//! A placeholder is only recognised as a *whole* element (`{url}`), never inside a larger
//! string, and it expands to exactly one argv element: it is never re-split or
//! interpreted. There is no `sh -c` string action; a user who wants a shell writes
//! `"sh" "-c" "script" "_" {url}` and the placeholder arrives as `$1`.
//!
//! Source form: a literal element writes `{` as `{{` and `}` as `}}`; the element
//! `{url}` (unescaped, whole) is the placeholder; `{{url}}` is the literal `{url}`.

use crate::arg::{ArgType, ParamKind};

/// Most elements in an argv.
pub const MAX_ARGV_ELEMS: usize = 256;
/// Longest literal element in bytes.
pub const MAX_ARGV_ELEM_BYTES: usize = 16 * 1024;

crate::string_enum! {
    /// The closed set of argv placeholders.
    Placeholder {
        Selection = "selection",
        Url = "url",
        Match = "match",
        Cwd = "cwd",
        File = "file",
        Line = "line",
        PaneId = "pane-id",
    }
}

impl Placeholder {
    /// The source token, e.g. `{url}`.
    pub fn token(self) -> String {
        format!("{{{}}}", self.as_str())
    }

    /// True when the value is pane text (needs `read-content`).
    pub const fn carries_content(self) -> bool {
        matches!(
            self,
            Placeholder::Selection | Placeholder::Match | Placeholder::Url
        )
    }
}

/// One argv element.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArgvElem {
    /// Passed verbatim.
    Literal(String),
    /// Replaced by exactly one element at dispatch.
    Placeholder(Placeholder),
}

/// Values for placeholders at dispatch time.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PlaceholderValues {
    /// `{selection}`
    pub selection: Option<String>,
    /// `{url}`
    pub url: Option<String>,
    /// `{match}`
    pub matched: Option<String>,
    /// `{cwd}`
    pub cwd: Option<String>,
    /// `{file}`
    pub file: Option<String>,
    /// `{line}`
    pub line: Option<String>,
    /// `{pane-id}`
    pub pane_id: Option<String>,
}

impl PlaceholderValues {
    fn get(&self, p: Placeholder) -> Option<&String> {
        match p {
            Placeholder::Selection => self.selection.as_ref(),
            Placeholder::Url => self.url.as_ref(),
            Placeholder::Match => self.matched.as_ref(),
            Placeholder::Cwd => self.cwd.as_ref(),
            Placeholder::File => self.file.as_ref(),
            Placeholder::Line => self.line.as_ref(),
            Placeholder::PaneId => self.pane_id.as_ref(),
        }
    }
}

/// A placeholder had no value at expansion time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExpandError(pub Placeholder);

impl std::fmt::Display for ExpandError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "no value for placeholder {}", self.0.token())
    }
}

impl std::error::Error for ExpandError {}

/// A validated, non-empty argv whose first element is a literal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Argv(Vec<ArgvElem>);

fn parse_elem(s: &str) -> Result<ArgvElem, String> {
    if let Some(inner) = s.strip_prefix('{').and_then(|r| r.strip_suffix('}'))
        && let Some(p) = Placeholder::parse(inner)
    {
        return Ok(ArgvElem::Placeholder(p));
    }
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        match c {
            '{' | '}' => {
                if it.peek() == Some(&c) {
                    it.next();
                    out.push(c);
                } else {
                    return Err("a brace in an argv element must be doubled; a placeholder \
                                must be a whole element ({selection} {url} {match} {cwd} \
                                {file} {line} {pane-id}) and is never interpolated"
                        .to_string());
                }
            }
            c => out.push(c),
        }
    }
    Ok(ArgvElem::Literal(out))
}

fn format_elem(e: &ArgvElem) -> String {
    match e {
        ArgvElem::Placeholder(p) => p.token(),
        ArgvElem::Literal(s) => s.replace('{', "{{").replace('}', "}}"),
    }
}

impl Argv {
    /// Validate and build.
    pub fn new(elems: Vec<ArgvElem>) -> Result<Argv, String> {
        if elems.is_empty() {
            return Err("argv must not be empty".to_string());
        }
        if elems.len() > MAX_ARGV_ELEMS {
            return Err(format!("argv has more than {MAX_ARGV_ELEMS} elements"));
        }
        if !matches!(elems[0], ArgvElem::Literal(_)) {
            return Err("the program (first element) cannot be a placeholder".to_string());
        }
        for e in &elems {
            if let ArgvElem::Literal(s) = e {
                if s.len() > MAX_ARGV_ELEM_BYTES {
                    return Err(format!(
                        "argv element is {} bytes, limit is {MAX_ARGV_ELEM_BYTES}",
                        s.len()
                    ));
                }
                if s.contains('\0') {
                    return Err("argv element contains NUL".to_string());
                }
            }
        }
        Ok(Argv(elems))
    }

    /// An all-literal argv.
    pub fn literal(args: &[&str]) -> Result<Argv, String> {
        Argv::new(
            args.iter()
                .map(|a| ArgvElem::Literal((*a).to_string()))
                .collect(),
        )
    }

    /// From source-form elements (see module docs).
    pub fn from_source(elems: &[String]) -> Result<Argv, String> {
        let mut v = Vec::with_capacity(elems.len().min(MAX_ARGV_ELEMS + 1));
        for e in elems.iter().take(MAX_ARGV_ELEMS + 1) {
            v.push(parse_elem(e)?);
        }
        Argv::new(v)
    }

    /// To source-form elements.
    pub fn to_source(&self) -> Vec<String> {
        self.0.iter().map(format_elem).collect()
    }

    /// The elements.
    pub fn elems(&self) -> &[ArgvElem] {
        &self.0
    }

    /// Placeholders used, in order.
    pub fn placeholders(&self) -> impl Iterator<Item = Placeholder> + '_ {
        self.0.iter().filter_map(|e| match e {
            ArgvElem::Placeholder(p) => Some(*p),
            ArgvElem::Literal(_) => None,
        })
    }

    /// Expand to the concrete argv. One placeholder becomes exactly one element,
    /// byte-for-byte, however many spaces or metacharacters it holds.
    pub fn expand(&self, vals: &PlaceholderValues) -> Result<Vec<String>, ExpandError> {
        self.0
            .iter()
            .map(|e| match e {
                ArgvElem::Literal(s) => Ok(s.clone()),
                ArgvElem::Placeholder(p) => vals.get(*p).cloned().ok_or(ExpandError(*p)),
            })
            .collect()
    }
}

impl ArgType for Argv {
    const KIND: ParamKind = ParamKind::Argv;
    fn parse_one(s: &str) -> Result<Self, String> {
        Argv::from_source(&[s.to_string()])
    }
    fn to_one(&self) -> String {
        self.to_source().join(" ")
    }
    fn from_values(vals: &[String]) -> Result<Self, String> {
        Argv::from_source(vals)
    }
    fn to_values(&self) -> Vec<String> {
        self.to_source()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn src(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn whole_element_placeholder() {
        let a = Argv::from_source(&src(&["xdg-open", "{url}"])).unwrap();
        assert_eq!(a.elems()[1], ArgvElem::Placeholder(Placeholder::Url));
        assert_eq!(a.to_source(), src(&["xdg-open", "{url}"]));
    }

    #[test]
    fn embedded_placeholder_is_rejected() {
        assert!(Argv::from_source(&src(&["sh", "-c", "echo {url}"])).is_err());
        assert!(Argv::from_source(&src(&["x", "--url={url}"])).is_err());
        assert!(Argv::from_source(&src(&["x", "{unknown}"])).is_err());
        assert!(Argv::from_source(&src(&["x", "a}"])).is_err());
    }

    #[test]
    fn doubled_braces_are_literal() {
        let a = Argv::from_source(&src(&["sh", "-c", "echo ${{HOME}}", "{{url}}"])).unwrap();
        assert_eq!(a.elems()[2], ArgvElem::Literal("echo ${HOME}".into()));
        assert_eq!(a.elems()[3], ArgvElem::Literal("{url}".into()));
        assert_eq!(
            a.to_source(),
            src(&["sh", "-c", "echo ${{HOME}}", "{{url}}"])
        );
    }

    #[test]
    fn program_cannot_be_placeholder() {
        assert!(Argv::from_source(&src(&["{selection}"])).is_err());
        assert!(Argv::from_source(&[]).is_err());
    }

    #[test]
    fn placeholder_never_resplit() {
        let a = Argv::from_source(&src(&["prog", "-x", "{selection}", "{url}"])).unwrap();
        let v = PlaceholderValues {
            selection: Some("a b; rm -rf / $(x) `y` \"q\"\n--flag".into()),
            url: Some("http://e/ x".into()),
            ..Default::default()
        };
        let out = a.expand(&v).unwrap();
        assert_eq!(out.len(), 4);
        assert_eq!(out[2], "a b; rm -rf / $(x) `y` \"q\"\n--flag");
        assert_eq!(out[3], "http://e/ x");
    }

    #[test]
    fn sh_c_gets_positional_params() {
        let a =
            Argv::from_source(&src(&["sh", "-c", "printf %s \"$1\"", "_", "{selection}"])).unwrap();
        let v = PlaceholderValues {
            selection: Some("$(evil)".into()),
            ..Default::default()
        };
        let out = a.expand(&v).unwrap();
        assert_eq!(out[2], "printf %s \"$1\"");
        assert_eq!(out[4], "$(evil)");
    }

    #[test]
    fn missing_value_errors() {
        let a = Argv::from_source(&src(&["p", "{file}"])).unwrap();
        assert_eq!(
            a.expand(&PlaceholderValues::default()),
            Err(ExpandError(Placeholder::File))
        );
    }

    #[test]
    fn limits() {
        let big = "x".repeat(MAX_ARGV_ELEM_BYTES + 1);
        assert!(Argv::from_source(&[big]).is_err());
        let many: Vec<String> = (0..=MAX_ARGV_ELEMS).map(|_| "a".to_string()).collect();
        assert!(Argv::from_source(&many).is_err());
        assert!(Argv::from_source(&src(&["a\0b"])).is_err());
    }

    #[test]
    fn roundtrip_all_placeholders() {
        let mut v = vec!["prog".to_string()];
        v.extend(Placeholder::ALL.iter().map(|p| p.token()));
        v.push("a{{b}}c".into());
        let a = Argv::from_source(&v).unwrap();
        assert_eq!(a.to_source(), v);
        assert_eq!(a.placeholders().count(), Placeholder::ALL.len());
    }
}
