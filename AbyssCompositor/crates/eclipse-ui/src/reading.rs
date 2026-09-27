// SPDX-License-Identifier: AGPL-3.0-only
//! A `source` widget's line (ADR 0065): a shipped source's reading, put
//! through the block's `format`.
//!
//! One function for the bar that draws the line and for Settings, which
//! previews it under the format field. Two copies drifted once already — the
//! preview dropped the `%` the bar keeps — and a preview that disagrees with
//! the bar is worse than none.

/// What the shipped sources read right now. `None` is a source with nothing
/// to say, which the bar shows as no line at all, never as zero.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Readings {
    /// Fractions, `1.0` = 100%.
    pub cpu: Option<f32>,
    pub mem: Option<f32>,
    pub gpu: Option<f32>,
    pub disk: Option<f32>,
    /// The default sink's level and whether it is muted.
    pub volume: Option<(f32, bool)>,
    pub title: Option<String>,
    pub artist: Option<String>,
}

impl Readings {
    /// Plausible stand-ins for a preview with no live data behind it.
    pub fn sample() -> Readings {
        Readings {
            cpu: Some(0.42),
            mem: Some(0.42),
            gpu: Some(0.42),
            disk: Some(0.42),
            volume: Some((0.62, false)),
            title: Some("Midnight City".into()),
            artist: Some("M83".into()),
        }
    }
}

/// A fraction as the bar prints it: `0.42` is `42%`.
pub fn percent(f: f32) -> String {
    format!("{:.0}%", f * 100.0)
}

/// `source`'s value, as text, before formatting.
pub fn value(source: &str, r: &Readings) -> Option<String> {
    match source {
        "usage.cpu" => r.cpu.map(percent),
        "usage.mem" => r.mem.map(percent),
        "usage.gpu" => r.gpu.map(percent),
        "usage.disk" => r.disk.map(percent),
        "audio.volume" => r
            .volume
            .map(|(v, muted)| if muted { "muted".to_owned() } else { percent(v) }),
        "media.title" => r.title.clone(),
        "media.artist" => r.artist.clone(),
        _ => None,
    }
}

/// The line a `source` block draws: `format` with every `{}` replaced by the
/// value. `None` while the source has nothing to say.
pub fn line(source: &str, format: &str, r: &Readings) -> Option<String> {
    value(source, r).map(|v| format.replace("{}", &v))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The value carries its own unit, so a format that adds text keeps it.
    #[test]
    fn a_percentage_keeps_its_sign_through_the_format() {
        let r = Readings {
            cpu: Some(0.19),
            ..Readings::default()
        };
        assert_eq!(line("usage.cpu", "C {}pct", &r).as_deref(), Some("C 19%pct"));
        assert_eq!(line("usage.mem", "{}", &r), None, "no reading, no line");
    }

    #[test]
    fn a_muted_sink_reads_muted() {
        let r = Readings {
            volume: Some((0.5, true)),
            ..Readings::default()
        };
        assert_eq!(line("audio.volume", "vol {}", &r).as_deref(), Some("vol muted"));
    }
}
