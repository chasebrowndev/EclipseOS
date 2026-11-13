// SPDX-License-Identifier: AGPL-3.0-only
//! Family / style / fallback resolution (T-06 §4). A [`FontSystem`] owns the system index, the
//! embedded fonts, the lazily loaded faces and a per-cluster resolution cache.
//!
//! Order for a cluster (one face covering *all* its code points): the style's family, then
//! `font.map`, then `font.fallback`, then the system coverage index (script aware), then the
//! embedded fonts, then the procedural tofu. Emoji presentation tries colour faces first.

use crate::config::{AxisSetting, FontConfig, Style};
use crate::discovery::{FontIndex, IndexedFace};
use crate::face::{Face, FaceId, FaceInstance};
use crate::metrics::CellMetrics;
use crate::text::{
    Presentation, is_default_ignorable, presentation, script_is_weak, script_of, script_probe,
};
use std::collections::HashMap;
use std::sync::Arc;

/// An in-memory font shipped with the program (the terminal fallback of every chain).
#[derive(Clone, Debug)]
pub struct EmbeddedFont {
    /// Short name for diagnostics.
    pub name: String,
    /// Font file bytes.
    pub data: Arc<[u8]>,
}

/// Position of a face in the system or embedded index.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FaceRef {
    /// True for the embedded index.
    pub embedded: bool,
    /// File position.
    pub file: u32,
    /// Face position within the file.
    pub face: u32,
}

/// Which rule of the fixed order produced a resolution.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rule {
    /// The style's own family (or the regular family it derives from).
    StyleFamily,
    /// The n-th `font.map` entry.
    Map(usize),
    /// The n-th `font.fallback` family.
    Fallback(usize),
    /// System coverage index.
    System,
    /// Embedded JetBrains Mono / Instrument Sans.
    Embedded,
    /// Nothing covers the base code point: procedural tofu with hex.
    Tofu,
}

/// Result of resolving one cluster.
#[derive(Clone, Debug)]
pub struct Resolved {
    /// The face to use; `None` means draw the tofu glyph.
    pub face: Option<Arc<FaceInstance>>,
    /// Rule that matched.
    pub rule: Rule,
    /// True if the face does not cover every code point of the cluster.
    pub partial: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ColorFilter {
    Only,
    Not,
    Any,
}

impl ColorFilter {
    fn accepts(self, f: &IndexedFace) -> bool {
        match self {
            Self::Only => f.color.drawable(),
            Self::Not => !f.color.drawable(),
            Self::Any => true,
        }
    }
}

struct Pick {
    r: FaceRef,
    synthetic_bold: bool,
    synthetic_italic: bool,
    auto_wght: Option<f32>,
}

/// Fonts for one configuration: discovery, loading, resolution caches.
pub struct FontSystem {
    sys: FontIndex,
    emb: FontIndex,
    emb_data: Vec<Arc<[u8]>>,
    config: FontConfig,
    families: HashMap<String, Vec<FaceRef>>,
    files: HashMap<(bool, u32), Option<Arc<[u8]>>>,
    loaded: HashMap<FaceRef, Option<Arc<Face>>>,
    instances: HashMap<(FaceRef, u8, bool), Option<Arc<FaceInstance>>>,
    cache: [HashMap<Box<str>, Resolved>; 4],
    next_id: u32,
    generation: u64,
}

impl std::fmt::Debug for FontSystem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FontSystem")
            .field("faces", &self.sys.face_count())
            .field("embedded", &self.emb.face_count())
            .field("generation", &self.generation)
            .finish_non_exhaustive()
    }
}

fn norm(name: &str) -> String {
    name.trim().to_lowercase()
}

impl FontSystem {
    /// Builds a system from an index, the embedded fonts and a configuration.
    pub fn new(sys: FontIndex, embedded: Vec<EmbeddedFont>, config: FontConfig) -> Self {
        let emb = FontIndex::from_memory(embedded.iter().map(|e| (e.name.as_str(), &e.data[..])));
        let emb_data = embedded.into_iter().map(|e| e.data).collect();
        let mut s = Self {
            sys,
            emb,
            emb_data,
            config,
            families: HashMap::new(),
            files: HashMap::new(),
            loaded: HashMap::new(),
            instances: HashMap::new(),
            cache: Default::default(),
            next_id: 0,
            generation: 1,
        };
        s.rebuild_families();
        s
    }

    fn rebuild_families(&mut self) {
        self.families.clear();
        for (emb, idx) in [(false, &self.sys), (true, &self.emb)] {
            for (fi, f) in idx.files.iter().enumerate() {
                for (i, face) in f.faces.iter().enumerate() {
                    self.families
                        .entry(norm(&face.family))
                        .or_default()
                        .push(FaceRef {
                            embedded: emb,
                            file: fi as u32,
                            face: i as u32,
                        });
                }
            }
        }
    }

    /// Replaces the configuration. Bumps the font generation, which invalidates every cache
    /// keyed on it (shape cache, glyph caches).
    pub fn set_config(&mut self, config: FontConfig) {
        self.config = config;
        self.instances.clear();
        for c in &mut self.cache {
            c.clear();
        }
        self.generation += 1;
    }

    /// The font generation counter (PERFORMANCE-20/-21).
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Current configuration.
    pub fn config(&self) -> &FontConfig {
        &self.config
    }

    /// Index data of a face.
    pub fn info(&self, r: FaceRef) -> Option<&IndexedFace> {
        let idx = if r.embedded { &self.emb } else { &self.sys };
        idx.files.get(r.file as usize)?.faces.get(r.face as usize)
    }

    /// Path of a face's file.
    pub fn path(&self, r: FaceRef) -> Option<&std::path::Path> {
        let idx = if r.embedded { &self.emb } else { &self.sys };
        idx.files.get(r.file as usize).map(|f| f.path.as_path())
    }

    /// All faces of a family (case-insensitive).
    pub fn family_faces(&self, family: &str) -> &[FaceRef] {
        self.families
            .get(&norm(family))
            .map_or(&[], |v| v.as_slice())
    }

    // ------------------------------------------------------------------ loading

    fn file_bytes(&mut self, r: FaceRef) -> Option<Arc<[u8]>> {
        if r.embedded {
            return self.emb_data.get(r.file as usize).cloned();
        }
        let key = (false, r.file);
        if !self.files.contains_key(&key) {
            let bytes = self
                .sys
                .files
                .get(r.file as usize)
                .and_then(|f| std::fs::read(&f.path).ok())
                .map(Arc::<[u8]>::from);
            self.files.insert(key, bytes);
        }
        self.files.get(&key).cloned().flatten()
    }

    /// Loads (and caches) a face.
    pub fn load(&mut self, r: FaceRef) -> Option<Arc<Face>> {
        if let Some(f) = self.loaded.get(&r) {
            return f.clone();
        }
        let ttc = self.info(r).map(|i| i.ttc_index)?;
        let face = self.file_bytes(r).and_then(|data| {
            let id = FaceId(self.next_id);
            Face::from_bytes(id, data, ttc).ok().map(Arc::new)
        });
        if face.is_some() {
            self.next_id += 1;
        }
        self.loaded.insert(r, face.clone());
        face
    }

    fn instance(&mut self, p: &Pick, style: Style, primary: bool) -> Option<Arc<FaceInstance>> {
        let key = (p.r, style.index() as u8, primary);
        if let Some(i) = self.instances.get(&key) {
            return i.clone();
        }
        let face = self.load(p.r);
        let inst = face.map(|face| {
            let sc = self.config.style(style);
            let (inst_name, axes): (Option<&str>, &[AxisSetting]) = if primary {
                (sc.instance.as_deref(), &sc.axes)
            } else {
                (None, &[])
            };
            let explicit_weight = inst_name.is_some() || axes.iter().any(|a| &a.tag == b"wght");
            let auto: Vec<AxisSetting> = p
                .auto_wght
                .map(|v| AxisSetting {
                    tag: *b"wght",
                    value: v,
                })
                .into_iter()
                .collect();
            Arc::new(FaceInstance::new(
                face,
                inst_name,
                axes,
                &auto,
                p.synthetic_bold && !explicit_weight,
                p.synthetic_italic,
            ))
        });
        self.instances.insert(key, inst.clone());
        inst
    }

    // --------------------------------------------------------------- selection

    fn pick_in_family(&self, family: &str, style: Style) -> Vec<Pick> {
        let want_bold = style.is_bold();
        let want_italic = style.is_italic();
        let target = if want_bold { 700.0 } else { 400.0 };
        let mut scored: Vec<(f32, FaceRef, &IndexedFace)> = self
            .family_faces(family)
            .iter()
            .filter_map(|&r| self.info(r).map(|i| (r, i)))
            .map(|(r, i)| {
                let mut d = if i.italic != want_italic { 1000.0 } else { 0.0 };
                d += match i.wght_axis {
                    Some((lo, hi)) if (lo..=hi).contains(&target) => 0.0,
                    Some((lo, hi)) => (target - target.clamp(lo, hi)).abs(),
                    None => (f32::from(i.weight) - target).abs(),
                };
                (d, r, i)
            })
            .collect();
        scored.sort_by(|a, b| {
            a.0.total_cmp(&b.0)
                .then_with(|| (a.1.file, a.1.face).cmp(&(b.1.file, b.1.face)))
        });
        scored
            .into_iter()
            .map(|(_, r, i)| {
                let var_covers = i
                    .wght_axis
                    .is_some_and(|(lo, hi)| (lo..=hi).contains(&target));
                let auto_wght = (var_covers && f32::from(i.weight) != target).then_some(target);
                let weight_now = if var_covers {
                    target
                } else {
                    f32::from(i.weight)
                };
                Pick {
                    r,
                    synthetic_bold: want_bold && weight_now < 600.0 && self.config.synthetic.bold,
                    synthetic_italic: want_italic && !i.italic && self.config.synthetic.italic,
                    auto_wght,
                }
            })
            .collect()
    }

    /// The face chain entry for a family at a style that covers `required`, with `filter`.
    fn family_cover(
        &self,
        family: &str,
        style: Style,
        required: &[u32],
        filter: ColorFilter,
    ) -> Option<Pick> {
        self.pick_in_family(family, style).into_iter().find(|p| {
            self.info(p.r)
                .is_some_and(|i| filter.accepts(i) && i.ranges.covers_all(required.iter().copied()))
        })
    }

    fn style_family(&self, style: Style) -> Option<&str> {
        self.config.style(style).family.as_deref().or(self
            .config
            .style(Style::Regular)
            .family
            .as_deref())
    }

    /// The primary instance of a style (its family, ignoring coverage).
    pub fn primary(&mut self, style: Style) -> Option<Arc<FaceInstance>> {
        if let Some(fam) = self.style_family(style).map(str::to_owned)
            && let Some(p) = self.pick_in_family(&fam, style).into_iter().next()
            && let Some(i) = self.instance(&p, style, true)
        {
            return Some(i);
        }
        // No (usable) configured family: the first embedded face is the primary.
        let r = FaceRef {
            embedded: true,
            file: 0,
            face: 0,
        };
        let p = self.pick_embedded_default(style).unwrap_or(Pick {
            r,
            synthetic_bold: false,
            synthetic_italic: false,
            auto_wght: None,
        });
        self.instance(&p, style, false)
    }

    fn pick_embedded_default(&self, style: Style) -> Option<Pick> {
        let mut best: Option<(f32, Pick)> = None;
        for (fi, f) in self.emb.files.iter().enumerate() {
            for (i, face) in f.faces.iter().enumerate() {
                if !face.monospace {
                    continue;
                }
                let target = if style.is_bold() { 700.0 } else { 400.0 };
                let d = (f32::from(face.weight) - target).abs()
                    + if face.italic != style.is_italic() {
                        1000.0
                    } else {
                        0.0
                    };
                let r = FaceRef {
                    embedded: true,
                    file: fi as u32,
                    face: i as u32,
                };
                if best.as_ref().is_none_or(|b| d < b.0) {
                    best = Some((
                        d,
                        Pick {
                            r,
                            synthetic_bold: style.is_bold()
                                && face.weight < 600
                                && self.config.synthetic.bold,
                            synthetic_italic: style.is_italic()
                                && !face.italic
                                && self.config.synthetic.italic,
                            auto_wght: face
                                .wght_axis
                                .is_some_and(|(lo, hi)| (lo..=hi).contains(&target))
                                .then_some(target),
                        },
                    ));
                }
            }
        }
        best.map(|b| b.1)
    }

    /// Cell metrics of the regular primary face at `px_size`, with the configured adjustments.
    pub fn cell_metrics(&mut self, px_size: f32) -> Option<CellMetrics> {
        let inst = self.primary(Style::Regular)?;
        let adjust = self.config.adjust;
        CellMetrics::compute(&inst, px_size, &adjust)
    }

    // -------------------------------------------------------------- resolution

    /// Resolves a cluster at a style. Cached: a hit does not allocate, and a miss (including a
    /// total miss, which yields the tofu) is cached once.
    pub fn resolve(&mut self, style: Style, cluster: &str) -> Resolved {
        if let Some(r) = self.cache[style.index()].get(cluster) {
            return r.clone();
        }
        let r = self.resolve_uncached(style, cluster);
        self.cache[style.index()].insert(cluster.into(), r.clone());
        r
    }

    fn resolve_uncached(&mut self, style: Style, cluster: &str) -> Resolved {
        let cps: Vec<u32> = cluster.chars().map(|c| c as u32).collect();
        let required: Vec<u32> = cps
            .iter()
            .copied()
            .filter(|&c| !is_default_ignorable(c))
            .collect();
        let Some(&base) = required.first() else {
            // Only ignorables: any face does; use the primary.
            return match self.primary(style) {
                Some(f) => Resolved {
                    face: Some(f),
                    rule: Rule::StyleFamily,
                    partial: false,
                },
                None => Resolved {
                    face: None,
                    rule: Rule::Tofu,
                    partial: false,
                },
            };
        };
        let passes: &[ColorFilter] = match presentation(&cps) {
            Presentation::Emoji => &[ColorFilter::Only, ColorFilter::Any],
            _ => &[ColorFilter::Not, ColorFilter::Any],
        };
        let script = cps
            .iter()
            .map(|&c| script_of(c))
            .find(|&s| !script_is_weak(s));

        for &filter in passes {
            if let Some(r) = self.resolve_pass(style, &required, base, script, filter) {
                return r;
            }
        }
        self.resolve_partial(style, &required, base)
    }

    fn resolve_pass(
        &mut self,
        style: Style,
        required: &[u32],
        base: u32,
        script: Option<[u8; 4]>,
        filter: ColorFilter,
    ) -> Option<Resolved> {
        // 1. the style's family.
        if let Some(fam) = self.style_family(style).map(str::to_owned)
            && let Some(p) = self.family_cover(&fam, style, required, filter)
            && let Some(i) = self.instance(&p, style, true)
        {
            return Some(Resolved {
                face: Some(i),
                rule: Rule::StyleFamily,
                partial: false,
            });
        }
        // 2. font.map
        let maps: Vec<(usize, String)> = self
            .config
            .map
            .iter()
            .enumerate()
            .filter(|(_, (r, _))| r.contains(base))
            .map(|(i, (_, f))| (i, f.clone()))
            .collect();
        for (n, fam) in maps {
            if let Some(p) = self.family_cover(&fam, style, required, filter)
                && let Some(i) = self.instance(&p, style, false)
            {
                return Some(Resolved {
                    face: Some(i),
                    rule: Rule::Map(n),
                    partial: false,
                });
            }
        }
        // 3. font.fallback
        let fallbacks = self.config.fallback.clone();
        for (n, fam) in fallbacks.iter().enumerate() {
            if let Some(p) = self.family_cover(fam, style, required, filter)
                && let Some(i) = self.instance(&p, style, false)
            {
                return Some(Resolved {
                    face: Some(i),
                    rule: Rule::Fallback(n),
                    partial: false,
                });
            }
        }
        // 4. system coverage index, script aware.
        if let Some(p) = self.system_cover(style, required, base, script, filter)
            && let Some(i) = self.instance(&p, style, false)
        {
            return Some(Resolved {
                face: Some(i),
                rule: Rule::System,
                partial: false,
            });
        }
        // 5. embedded fonts.
        let emb_fams: Vec<String> = {
            let mut v: Vec<String> = Vec::new();
            for f in &self.emb.files {
                for face in &f.faces {
                    if !v.contains(&face.family) {
                        v.push(face.family.clone());
                    }
                }
            }
            v
        };
        for fam in emb_fams {
            let hit = self.pick_in_family(&fam, style).into_iter().find(|p| {
                p.r.embedded
                    && self.info(p.r).is_some_and(|i| {
                        filter.accepts(i) && i.ranges.covers_all(required.iter().copied())
                    })
            });
            if let Some(p) = hit
                && let Some(i) = self.instance(&p, style, false)
            {
                return Some(Resolved {
                    face: Some(i),
                    rule: Rule::Embedded,
                    partial: false,
                });
            }
        }
        None
    }

    fn system_cover(
        &self,
        style: Style,
        required: &[u32],
        base: u32,
        script: Option<[u8; 4]>,
        filter: ColorFilter,
    ) -> Option<Pick> {
        let want_bold = style.is_bold();
        let want_italic = style.is_italic();
        let probe = script.map_or((0, 0), script_probe);
        let target = if want_bold { 700.0f32 } else { 400.0 };
        // Score: lower is better. (style distance, non-mono penalty, script coverage deficit).
        let mut best: Option<((u32, u32, u64), FaceRef)> = None;
        for (fi, file) in self.sys.files.iter().enumerate() {
            for (i, face) in file.faces.iter().enumerate() {
                if !filter.accepts(face) || !face.ranges.contains(base) {
                    continue;
                }
                if !face.ranges.covers_all(required.iter().copied()) {
                    continue;
                }
                let style_d = (if face.italic != want_italic { 1000 } else { 0 })
                    + (f32::from(face.weight) - target).abs() as u32;
                let mono_pen = u32::from(!face.monospace);
                let deficit = if probe.1 > 0 {
                    // Fewer covered code points of the script's block = worse.
                    let covered: u64 = face
                        .ranges
                        .ranges()
                        .iter()
                        .map(|&(a, b)| {
                            let lo = a.max(probe.0);
                            let hi = b.min(probe.1);
                            if lo <= hi { u64::from(hi - lo) + 1 } else { 0 }
                        })
                        .sum();
                    u64::from(probe.1 - probe.0 + 1).saturating_sub(covered)
                } else {
                    0
                };
                let score = (style_d, mono_pen, deficit);
                let better = best.as_ref().is_none_or(|b| score < b.0);
                if better {
                    best = Some((
                        score,
                        FaceRef {
                            embedded: false,
                            file: fi as u32,
                            face: i as u32,
                        },
                    ));
                }
            }
        }
        let (_, r) = best?;
        let info = self.info(r)?;
        let var_covers = info
            .wght_axis
            .is_some_and(|(lo, hi)| (lo..=hi).contains(&target));
        let weight_now = if var_covers {
            target
        } else {
            f32::from(info.weight)
        };
        Some(Pick {
            r,
            synthetic_bold: want_bold && weight_now < 600.0 && self.config.synthetic.bold,
            synthetic_italic: want_italic && !info.italic && self.config.synthetic.italic,
            auto_wght: (var_covers && f32::from(info.weight) != target).then_some(target),
        })
    }

    /// No face covers the whole cluster: take the face covering the most code points, as long
    /// as it covers the base. Otherwise the tofu.
    fn resolve_partial(&mut self, style: Style, required: &[u32], base: u32) -> Resolved {
        let mut best: Option<(usize, FaceRef, Rule)> = None;
        let consider = |sys: &FontSystem,
                        r: FaceRef,
                        rule: Rule,
                        best: &mut Option<(usize, FaceRef, Rule)>| {
            if let Some(i) = sys.info(r)
                && i.ranges.contains(base)
            {
                let n = i.ranges.count_covered(required.iter().copied());
                if best.as_ref().is_none_or(|b| n > b.0) {
                    *best = Some((n, r, rule));
                }
            }
        };
        if let Some(fam) = self.style_family(style) {
            for p in self.pick_in_family(fam, style) {
                consider(self, p.r, Rule::StyleFamily, &mut best);
            }
        }
        for (n, fam) in self.config.fallback.iter().enumerate() {
            for p in self.pick_in_family(fam, style) {
                consider(self, p.r, Rule::Fallback(n), &mut best);
            }
        }
        for (fi, file) in self.sys.files.iter().enumerate() {
            for i in 0..file.faces.len() {
                let r = FaceRef {
                    embedded: false,
                    file: fi as u32,
                    face: i as u32,
                };
                consider(self, r, Rule::System, &mut best);
            }
        }
        for (fi, file) in self.emb.files.iter().enumerate() {
            for i in 0..file.faces.len() {
                let r = FaceRef {
                    embedded: true,
                    file: fi as u32,
                    face: i as u32,
                };
                consider(self, r, Rule::Embedded, &mut best);
            }
        }
        let Some((_, r, rule)) = best else {
            return Resolved {
                face: None,
                rule: Rule::Tofu,
                partial: false,
            };
        };
        let pick = Pick {
            r,
            synthetic_bold: false,
            synthetic_italic: false,
            auto_wght: None,
        };
        match self.instance(&pick, style, false) {
            Some(f) => Resolved {
                face: Some(f),
                rule,
                partial: true,
            },
            None => Resolved {
                face: None,
                rule: Rule::Tofu,
                partial: false,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ranges::CodepointRanges;

    fn face(
        family: &str,
        ranges: &[(u32, u32)],
        weight: u16,
        italic: bool,
        color: bool,
    ) -> IndexedFace {
        IndexedFace {
            ttc_index: 0,
            family: family.into(),
            weight,
            italic,
            monospace: true,
            wght_axis: None,
            color: crate::face::ColorSources {
                colr: color,
                ..Default::default()
            },
            ranges: CodepointRanges::from_ranges(ranges.iter().copied()),
        }
    }

    /// A system with synthetic index entries only. `load` fails (no real file), so these
    /// tests exercise selection through `info`/`pick_in_family`, not instances.
    fn fake(faces: Vec<IndexedFace>) -> FontSystem {
        let files = faces
            .into_iter()
            .enumerate()
            .map(|(i, f)| crate::discovery::FileEntry {
                path: format!("/nonexistent/{i}.ttf").into(),
                mtime_ns: 0,
                size: 0,
                faces: vec![f],
            })
            .collect();
        FontSystem::new(FontIndex { files }, Vec::new(), FontConfig::default())
    }

    #[test]
    fn family_pick_prefers_matching_weight_and_slant() {
        let s = fake(vec![
            face("Mono", &[(0x20, 0x7E)], 400, false, false),
            face("Mono", &[(0x20, 0x7E)], 700, false, false),
            face("Mono", &[(0x20, 0x7E)], 400, true, false),
        ]);
        let reg = s.pick_in_family("mono", Style::Regular);
        assert_eq!(s.info(reg[0].r).unwrap().weight, 400);
        assert!(!s.info(reg[0].r).unwrap().italic);
        let bold = s.pick_in_family("MONO", Style::Bold);
        assert_eq!(s.info(bold[0].r).unwrap().weight, 700);
        assert!(!bold[0].synthetic_bold);
        let it = s.pick_in_family("Mono", Style::Italic);
        assert!(s.info(it[0].r).unwrap().italic);
        assert!(!it[0].synthetic_italic);
    }

    #[test]
    fn synthetic_only_when_missing_and_allowed() {
        let mut s = fake(vec![face("Mono", &[(0x20, 0x7E)], 400, false, false)]);
        let bi = s.pick_in_family("Mono", Style::BoldItalic);
        assert!(bi[0].synthetic_bold && bi[0].synthetic_italic);
        let mut c = FontConfig::default();
        c.synthetic.bold = false;
        s.set_config(c);
        let bi = s.pick_in_family("Mono", Style::BoldItalic);
        assert!(!bi[0].synthetic_bold && bi[0].synthetic_italic);
    }

    #[test]
    fn system_cover_is_script_aware_and_color_filtered() {
        let s = fake(vec![
            face("Latin Only", &[(0x20, 0x7E)], 400, false, false),
            face("Han Small", &[(0x4E00, 0x4E10)], 400, false, false),
            face("Han Big", &[(0x4E00, 0x9FFF)], 400, false, false),
            face(
                "Emoji",
                &[(0x1F600, 0x1F64F), (0x20, 0x7E)],
                400,
                false,
                true,
            ),
        ]);
        let han = [0x4E2D];
        let p = s
            .system_cover(
                Style::Regular,
                &han,
                0x4E2D,
                Some(*b"Hani"),
                ColorFilter::Not,
            )
            .unwrap();
        assert_eq!(s.info(p.r).unwrap().family, "Han Big");
        let e = [0x1F600];
        assert!(
            s.system_cover(Style::Regular, &e, 0x1F600, None, ColorFilter::Not)
                .is_none()
        );
        let p = s
            .system_cover(Style::Regular, &e, 0x1F600, None, ColorFilter::Only)
            .unwrap();
        assert_eq!(s.info(p.r).unwrap().family, "Emoji");
        assert!(
            s.system_cover(Style::Regular, &[0x3042], 0x3042, None, ColorFilter::Any)
                .is_none()
        );
    }

    #[test]
    fn missing_everywhere_resolves_to_tofu_once() {
        let mut s = fake(vec![face("Latin Only", &[(0x20, 0x7E)], 400, false, false)]);
        let r = s.resolve(Style::Regular, "\u{3042}");
        assert!(r.face.is_none());
        assert_eq!(r.rule, Rule::Tofu);
        // Cached: the second call returns the same without re-running resolution.
        assert_eq!(s.cache[0].len(), 1);
        let _ = s.resolve(Style::Regular, "\u{3042}");
        assert_eq!(s.cache[0].len(), 1);
    }

    #[test]
    fn generation_bumps_on_config_change() {
        let mut s = fake(Vec::new());
        let g = s.generation();
        s.set_config(FontConfig::default());
        assert_eq!(s.generation(), g + 1);
    }
}
