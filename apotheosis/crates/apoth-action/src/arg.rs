// SPDX-License-Identifier: Apache-2.0
//! Typed argument values: the `ArgType` trait every Action field implements, the closed
//! `ParamKind` set the registry and settings UI read, and the small value types
//! (`Dur`, `Bytes`) that have a textual form.
//!
//! Every value parses from, and formats to, the same string form the textual action
//! syntax and the KDL node form use (T-11 §1, AUTOMATION-24). Round-trip is tested.

/// Byte budget for any free-text argument (T-18 §5: every string sink has a budget).
pub const MAX_TEXT_BYTES: usize = 64 * 1024;

/// Longest accepted duration (24 h).
pub const MAX_DURATION_MS: u64 = 24 * 60 * 60 * 1000;

/// What the settings bind editor and palette show for a parameter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParamKind {
    /// Free text.
    Text,
    /// Flag.
    Bool,
    /// Signed integer.
    Int,
    /// Unsigned integer.
    Uint,
    /// Finite float.
    Float,
    /// Duration with unit (`500ms`, `10s`, `2m`, `1h`).
    Duration,
    /// Argv array with typed placeholders (T-11 §2).
    Argv,
    /// Hex-encoded bytes.
    Hex,
    /// Space-separated key chords.
    Keys,
    /// A single character.
    Char,
    /// One of a closed set of names.
    Enum(&'static [&'static str]),
}

impl ParamKind {
    /// Short label for help text.
    pub const fn label(self) -> &'static str {
        match self {
            ParamKind::Text => "text",
            ParamKind::Bool => "bool",
            ParamKind::Int => "int",
            ParamKind::Uint => "uint",
            ParamKind::Float => "float",
            ParamKind::Duration => "duration",
            ParamKind::Argv => "argv",
            ParamKind::Hex => "hex",
            ParamKind::Keys => "keys",
            ParamKind::Char => "char",
            ParamKind::Enum(_) => "enum",
        }
    }
}

/// A value that can be an Action argument.
pub trait ArgType: Sized {
    /// Registry kind.
    const KIND: ParamKind;
    /// False when the argument may be omitted.
    const REQUIRED: bool = true;
    /// True when the argument may be given more than once.
    const REPEATED: bool = false;

    /// Parse one textual value.
    fn parse_one(s: &str) -> Result<Self, String>;
    /// Format one value.
    fn to_one(&self) -> String;

    /// Build from all the textual values given for the parameter (zero, one, or many).
    fn from_values(vals: &[String]) -> Result<Self, String> {
        match vals {
            [v] => Self::parse_one(v),
            [] => Err("missing required argument".to_string()),
            _ => Err("given more than once".to_string()),
        }
    }

    /// Textual values; empty means "omit".
    fn to_values(&self) -> Vec<String> {
        vec![self.to_one()]
    }
}

impl ArgType for String {
    const KIND: ParamKind = ParamKind::Text;
    fn parse_one(s: &str) -> Result<Self, String> {
        if s.len() > MAX_TEXT_BYTES {
            return Err(format!(
                "text is {} bytes, limit is {MAX_TEXT_BYTES}",
                s.len()
            ));
        }
        Ok(s.to_string())
    }
    fn to_one(&self) -> String {
        self.clone()
    }
}

impl ArgType for bool {
    const KIND: ParamKind = ParamKind::Bool;
    const REQUIRED: bool = false;
    fn parse_one(s: &str) -> Result<Self, String> {
        match s {
            "true" => Ok(true),
            "false" => Ok(false),
            _ => Err("expected `true` or `false`".to_string()),
        }
    }
    fn to_one(&self) -> String {
        (if *self { "true" } else { "false" }).to_string()
    }
    fn from_values(vals: &[String]) -> Result<Self, String> {
        match vals {
            [] => Ok(false),
            [v] => Self::parse_one(v),
            _ => Err("given more than once".to_string()),
        }
    }
    fn to_values(&self) -> Vec<String> {
        if *self {
            vec!["true".to_string()]
        } else {
            Vec::new()
        }
    }
}

impl ArgType for u32 {
    const KIND: ParamKind = ParamKind::Uint;
    fn parse_one(s: &str) -> Result<Self, String> {
        s.parse::<u32>()
            .map_err(|_| "expected an unsigned integer".to_string())
    }
    fn to_one(&self) -> String {
        self.to_string()
    }
}

impl ArgType for i32 {
    const KIND: ParamKind = ParamKind::Int;
    fn parse_one(s: &str) -> Result<Self, String> {
        s.parse::<i32>()
            .map_err(|_| "expected an integer".to_string())
    }
    fn to_one(&self) -> String {
        self.to_string()
    }
}

impl ArgType for f32 {
    const KIND: ParamKind = ParamKind::Float;
    fn parse_one(s: &str) -> Result<Self, String> {
        let v: f32 = s.parse().map_err(|_| "expected a number".to_string())?;
        if v.is_finite() {
            Ok(v)
        } else {
            Err("number must be finite".to_string())
        }
    }
    fn to_one(&self) -> String {
        self.to_string()
    }
}

impl ArgType for char {
    const KIND: ParamKind = ParamKind::Char;
    fn parse_one(s: &str) -> Result<Self, String> {
        let mut it = s.chars();
        match (it.next(), it.next()) {
            (Some(c), None) if !c.is_control() => Ok(c),
            _ => Err("expected exactly one printable character".to_string()),
        }
    }
    fn to_one(&self) -> String {
        self.to_string()
    }
}

impl<T: ArgType> ArgType for Option<T> {
    const KIND: ParamKind = T::KIND;
    const REQUIRED: bool = false;
    const REPEATED: bool = T::REPEATED;
    fn parse_one(s: &str) -> Result<Self, String> {
        T::parse_one(s).map(Some)
    }
    fn to_one(&self) -> String {
        match self {
            Some(v) => v.to_one(),
            None => String::new(),
        }
    }
    fn from_values(vals: &[String]) -> Result<Self, String> {
        if vals.is_empty() {
            Ok(None)
        } else {
            T::from_values(vals).map(Some)
        }
    }
    fn to_values(&self) -> Vec<String> {
        match self {
            // `Some(false)` must not collapse to `None`: bool's own list form is empty.
            Some(v) => {
                let vals = v.to_values();
                if vals.is_empty() && !T::REPEATED && T::KIND == ParamKind::Bool {
                    vec![v.to_one()]
                } else {
                    vals
                }
            }
            None => Vec::new(),
        }
    }
}

/// Most repeats of a repeatable argument.
pub const MAX_REPEATS: usize = 64;

impl<T: ArgType> ArgType for Vec<T> {
    const KIND: ParamKind = T::KIND;
    const REQUIRED: bool = false;
    const REPEATED: bool = true;
    fn parse_one(s: &str) -> Result<Self, String> {
        T::parse_one(s).map(|v| vec![v])
    }
    fn to_one(&self) -> String {
        String::new()
    }
    fn from_values(vals: &[String]) -> Result<Self, String> {
        if vals.len() > MAX_REPEATS {
            return Err(format!("more than {MAX_REPEATS} repeats"));
        }
        vals.iter().map(|v| T::parse_one(v)).collect()
    }
    fn to_values(&self) -> Vec<String> {
        self.iter().map(ArgType::to_one).collect()
    }
}

/// A duration in whole milliseconds with a unit suffix in text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Dur(pub u64);

impl Dur {
    /// Parse `500ms`, `10s`, `1.5s`, `2m`, `1h`.
    pub fn parse(s: &str) -> Result<Dur, String> {
        let idx = s
            .find(|c: char| !(c.is_ascii_digit() || c == '.'))
            .ok_or("missing unit (ms, s, m, h)")?;
        let (num, unit) = s.split_at(idx);
        if num.is_empty() {
            return Err("missing number".to_string());
        }
        let n: f64 = num.parse().map_err(|_| "bad number".to_string())?;
        let mult = match unit {
            "ms" => 1.0,
            "s" => 1000.0,
            "m" => 60_000.0,
            "h" => 3_600_000.0,
            _ => return Err(format!("unknown unit `{unit}` (ms, s, m, h)")),
        };
        let ms = (n * mult).round();
        if !ms.is_finite() || ms < 0.0 || ms > MAX_DURATION_MS as f64 {
            return Err("duration out of range (max 24h)".to_string());
        }
        Ok(Dur(ms as u64))
    }

    /// Format with the largest unit that divides evenly.
    pub fn format(self) -> String {
        let ms = self.0;
        if ms > 0 && ms.is_multiple_of(3_600_000) {
            format!("{}h", ms / 3_600_000)
        } else if ms > 0 && ms.is_multiple_of(60_000) {
            format!("{}m", ms / 60_000)
        } else if ms > 0 && ms.is_multiple_of(1000) {
            format!("{}s", ms / 1000)
        } else {
            format!("{ms}ms")
        }
    }
}

impl ArgType for Dur {
    const KIND: ParamKind = ParamKind::Duration;
    fn parse_one(s: &str) -> Result<Self, String> {
        Dur::parse(s)
    }
    fn to_one(&self) -> String {
        self.format()
    }
}

/// Raw bytes, hex in text. Used by `input.send-bytes`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bytes(pub Vec<u8>);

fn hex_val(b: u8) -> Result<u8, String> {
    match b {
        b'0'..=b'9' => Ok(b - b'0'),
        b'a'..=b'f' => Ok(b - b'a' + 10),
        b'A'..=b'F' => Ok(b - b'A' + 10),
        _ => Err("expected hexadecimal digits".to_string()),
    }
}

impl ArgType for Bytes {
    const KIND: ParamKind = ParamKind::Hex;
    fn parse_one(s: &str) -> Result<Self, String> {
        if s.is_empty() || !s.len().is_multiple_of(2) {
            return Err("expected an even, non-zero number of hex digits".to_string());
        }
        if s.len() / 2 > MAX_TEXT_BYTES {
            return Err(format!("more than {MAX_TEXT_BYTES} bytes"));
        }
        let mut out = Vec::with_capacity(s.len() / 2);
        for pair in s.as_bytes().chunks(2) {
            out.push((hex_val(pair[0])? << 4) | hex_val(pair[1])?);
        }
        Ok(Bytes(out))
    }
    fn to_one(&self) -> String {
        use std::fmt::Write;
        let mut s = String::with_capacity(self.0.len() * 2);
        for b in &self.0 {
            let _ = write!(s, "{b:02x}");
        }
        s
    }
}

/// Define a closed enum of kebab-case names that is also an `ArgType`.
#[macro_export]
macro_rules! string_enum {
    ($(#[$m:meta])* $name:ident { $($(#[$vm:meta])* $var:ident = $s:literal),+ $(,)? }) => {
        $(#[$m])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum $name { $($(#[$vm])* $var),+ }

        impl $name {
            /// Every name, in declaration order.
            pub const NAMES: &'static [&'static str] = &[$($s),+];
            /// Every value, in declaration order.
            pub const ALL: &'static [$name] = &[$($name::$var),+];
            /// The text form.
            pub const fn as_str(self) -> &'static str {
                match self { $($name::$var => $s),+ }
            }
            /// Parse the text form.
            pub fn parse(s: &str) -> Option<Self> {
                match s { $($s => Some($name::$var),)+ _ => None }
            }
        }

        impl $crate::arg::ArgType for $name {
            const KIND: $crate::arg::ParamKind = $crate::arg::ParamKind::Enum(Self::NAMES);
            fn parse_one(s: &str) -> Result<Self, String> {
                Self::parse(s).ok_or_else(|| format!("expected one of: {}", Self::NAMES.join(", ")))
            }
            fn to_one(&self) -> String {
                self.as_str().to_string()
            }
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dur_roundtrip() {
        for ms in [
            0u64, 1, 999, 1000, 1500, 60_000, 90_000, 3_600_000, 7_200_000, 86_400_000,
        ] {
            let t = Dur(ms).format();
            assert_eq!(Dur::parse(&t), Ok(Dur(ms)), "{t}");
        }
        assert_eq!(Dur::parse("1.5s"), Ok(Dur(1500)));
        assert!(Dur::parse("10").is_err());
        assert!(Dur::parse("s").is_err());
        assert!(Dur::parse("10x").is_err());
        assert!(Dur::parse("25h").is_err());
        assert!(Dur::parse("1.2.3s").is_err());
    }

    #[test]
    fn bytes_roundtrip() {
        let b = Bytes::parse_one("1B5b41").unwrap();
        assert_eq!(b.0, vec![0x1b, 0x5b, 0x41]);
        assert_eq!(b.to_one(), "1b5b41");
        assert!(Bytes::parse_one("1").is_err());
        assert!(Bytes::parse_one("zz").is_err());
        assert!(Bytes::parse_one("").is_err());
        assert!(Bytes::parse_one("é").is_err());
    }

    #[test]
    fn scalars() {
        assert_eq!(u32::parse_one("7"), Ok(7));
        assert!(u32::parse_one("-1").is_err());
        assert_eq!(i32::parse_one("-1"), Ok(-1));
        assert!(f32::parse_one("inf").is_err());
        assert!(f32::parse_one("NaN").is_err());
        assert_eq!(f32::parse_one("1.5"), Ok(1.5));
        assert_eq!(char::parse_one("x"), Ok('x'));
        assert!(char::parse_one("xy").is_err());
        assert!(char::parse_one("").is_err());
        assert!(char::parse_one("\n").is_err());
    }

    #[test]
    fn bool_flag_semantics() {
        assert_eq!(bool::from_values(&[]), Ok(false));
        assert_eq!(bool::from_values(&["true".into()]), Ok(true));
        assert!(true.to_values() == vec!["true".to_string()]);
        assert!(false.to_values().is_empty());
        assert!(bool::from_values(&["yes".into()]).is_err());
    }

    #[test]
    fn option_and_vec() {
        assert_eq!(Option::<u32>::from_values(&[]), Ok(None));
        assert_eq!(Option::<u32>::from_values(&["3".into()]), Ok(Some(3)));
        assert_eq!(
            Vec::<u32>::from_values(&["1".into(), "2".into()]),
            Ok(vec![1, 2])
        );
        assert_eq!(Some(3u32).to_values(), vec!["3".to_string()]);
        assert!(None::<u32>.to_values().is_empty());
        assert!(u32::from_values(&[]).is_err());
        assert!(u32::from_values(&["1".into(), "2".into()]).is_err());
    }

    #[test]
    fn text_budget() {
        let big = "x".repeat(MAX_TEXT_BYTES + 1);
        assert!(String::parse_one(&big).is_err());
        assert!(String::parse_one(&big[..MAX_TEXT_BYTES]).is_ok());
    }
}
