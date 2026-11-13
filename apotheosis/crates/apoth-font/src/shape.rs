// SPDX-License-Identifier: AGPL-3.0-only
//! Row shaping (T-06 §5). Runs never cross rows; a run breaks on face, style and script, and
//! at the cursor cell under the `not-under-cursor` ligature policy. Simple text skips the
//! shaper; everything else goes through `harfrust` with a cached [`harfrust::ShapePlan`] and
//! a position-independent, fixed-size N-way associative shape cache.

use crate::builtin;
use crate::config::{FontFeature, Ligatures, Style};
use crate::face::FaceInstance;
use crate::system::{FontSystem, Rule};
use crate::text::{script_is_weak, script_of};
use harfrust::{Direction, Feature, Script, ShapeOptions, ShapePlan, UnicodeBuffer};
use skrifa::raw::types::Tag;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

/// One cluster of a row as the grid sees it.
#[derive(Clone, Copy, Debug)]
pub struct RowCluster<'a> {
    /// First column of the cluster.
    pub col: u16,
    /// Columns occupied (1, or 2 for wide clusters; `font.cell-width-override` is applied by
    /// the caller when it computes this).
    pub cells: u8,
    /// The cluster text (never empty; blank cells pass `" "`).
    pub text: &'a str,
    /// Text style.
    pub style: Style,
}

/// What a placed glyph is drawn from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GlyphSource {
    /// A glyph of the run's face.
    Glyph(u32),
    /// A built-in sprite (box drawing, blocks, braille, Powerline, legacy computing).
    Sprite(u32),
    /// Procedural tofu with the hex of this code point.
    Tofu(u32),
}

/// A shaped glyph mapped back to cells.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlacedGlyph {
    /// What to draw.
    pub source: GlyphSource,
    /// First column.
    pub col: u16,
    /// Columns spanned. Greater than 1 for ligatures and wide clusters: rasterise once and draw
    /// `span` cell-wide slices (see [`PlacedGlyph::slice_source`]).
    pub span: u16,
    /// Horizontal offset from the cell origin in physical pixels (integer: grid glyphs use
    /// integer origins).
    pub x_offset: i32,
    /// Vertical offset in physical pixels, positive up.
    pub y_offset: i32,
}

impl PlacedGlyph {
    /// Ligatures wider than this many cells are "infinite" (a repeated run such as `=====`):
    /// only the first, one middle and the last slice are rasterised (RENDERING-23).
    pub const INFINITE_SPAN: u16 = 9;

    /// Which slice of the once-rasterised glyph cell `i` (0..span) draws. For long repeats the
    /// middle slice (1) is reused for every interior cell.
    pub fn slice_source(&self, i: u16) -> u16 {
        let i = i.min(self.span.saturating_sub(1));
        if self.span > Self::INFINITE_SPAN && i != 0 && i != self.span - 1 {
            1
        } else {
            i
        }
    }
}

/// One run: consecutive clusters resolved to the same face and style.
#[derive(Clone, Debug)]
pub struct ShapedRun {
    /// The face; `None` for tofu or sprite runs.
    pub face: Option<Arc<FaceInstance>>,
    /// Style of the run.
    pub style: Style,
    /// Rule that resolved the face.
    pub rule: Rule,
    /// First column.
    pub first_col: u16,
    /// One past the last column.
    pub end_col: u16,
    /// Glyphs of the run: `glyphs[range.0..range.1]` of the owning [`ShapedRow`].
    pub range: (usize, usize),
    /// True if the simple-text fast path produced this run.
    pub fast_path: bool,
}

/// The shaping result of one row. Reuse across rows to avoid reallocation.
#[derive(Clone, Debug, Default)]
pub struct ShapedRow {
    /// Runs in column order.
    pub runs: Vec<ShapedRun>,
    /// Glyphs of all runs.
    pub glyphs: Vec<PlacedGlyph>,
}

impl ShapedRow {
    /// Empties the row, keeping capacity.
    pub fn clear(&mut self) {
        self.runs.clear();
        self.glyphs.clear();
    }

    /// Glyphs of a run.
    pub fn run_glyphs(&self, run: &ShapedRun) -> &[PlacedGlyph] {
        &self.glyphs[run.range.0..run.range.1]
    }
}

/// A shaped glyph in font units, position-independent (cluster is a byte offset into the run
/// text).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ShapedGlyph {
    /// Glyph id.
    pub glyph_id: u32,
    /// Byte offset of the cluster in the run text.
    pub cluster: u32,
    /// Advance in font units.
    pub x_advance: i32,
    /// Offset in font units.
    pub x_offset: i32,
    /// Offset in font units.
    pub y_offset: i32,
}

// ------------------------------------------------------------------ shape cache

struct Entry {
    hash: u64,
    text: Box<str>,
    face_key: u64,
    features: u64,
    size_bits: u32,
    script: [u8; 4],
    glyphs: Arc<[ShapedGlyph]>,
    stamp: u64,
}

/// Borrowed lookup key: building one allocates nothing.
#[derive(Clone, Copy, Debug)]
pub struct ShapeKey<'a> {
    /// Run text.
    pub text: &'a str,
    /// [`FaceInstance::key`].
    pub face_key: u64,
    /// Hash of the feature list.
    pub features: u64,
    /// Pixel size bits.
    pub size_bits: u32,
    /// Script tag.
    pub script: [u8; 4],
}

impl ShapeKey<'_> {
    fn hash(&self) -> u64 {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.text.hash(&mut h);
        self.face_key.hash(&mut h);
        self.features.hash(&mut h);
        self.size_bits.hash(&mut h);
        self.script.hash(&mut h);
        h.finish()
    }
}

/// Fixed-size N-way set-associative cache of shaped runs. Lookup hashes a borrowed key and
/// compares in place; a hit returns an `Arc` clone (no allocation). Replacement is LRU inside
/// a set.
#[derive(Debug)]
pub struct ShapeCache {
    sets: Vec<Vec<Entry>>,
    ways: usize,
    mask: usize,
    tick: u64,
    /// Lookups that hit.
    pub hits: u64,
    /// Lookups that missed.
    pub misses: u64,
}

impl std::fmt::Debug for Entry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Entry")
            .field("len", &self.glyphs.len())
            .finish_non_exhaustive()
    }
}

impl ShapeCache {
    /// Default geometry: 256 sets of 8 ways.
    pub const DEFAULT_SETS: usize = 256;
    /// Default associativity.
    pub const DEFAULT_WAYS: usize = 8;

    /// A cache of `sets` (rounded up to a power of two) by `ways` entries.
    pub fn new(sets: usize, ways: usize) -> Self {
        let sets = sets.max(1).next_power_of_two();
        let ways = ways.max(1);
        Self {
            sets: (0..sets).map(|_| Vec::with_capacity(ways)).collect(),
            ways,
            mask: sets - 1,
            tick: 0,
            hits: 0,
            misses: 0,
        }
    }

    /// Drops every entry.
    pub fn clear(&mut self) {
        for s in &mut self.sets {
            s.clear();
        }
    }

    /// Number of cached runs.
    pub fn len(&self) -> usize {
        self.sets.iter().map(Vec::len).sum()
    }

    /// True if empty.
    pub fn is_empty(&self) -> bool {
        self.sets.iter().all(Vec::is_empty)
    }

    /// Looks up a borrowed key.
    pub fn get(&mut self, key: &ShapeKey<'_>) -> Option<Arc<[ShapedGlyph]>> {
        let hash = key.hash();
        self.tick += 1;
        let tick = self.tick;
        let set = &mut self.sets[hash as usize & self.mask];
        for e in set.iter_mut() {
            if e.hash == hash
                && e.face_key == key.face_key
                && e.features == key.features
                && e.size_bits == key.size_bits
                && e.script == key.script
                && &*e.text == key.text
            {
                e.stamp = tick;
                self.hits += 1;
                return Some(e.glyphs.clone());
            }
        }
        self.misses += 1;
        None
    }

    /// Inserts a result (allocates the owned key; only on a miss).
    pub fn insert(&mut self, key: &ShapeKey<'_>, glyphs: Arc<[ShapedGlyph]>) {
        let hash = key.hash();
        self.tick += 1;
        let entry = Entry {
            hash,
            text: key.text.into(),
            face_key: key.face_key,
            features: key.features,
            size_bits: key.size_bits,
            script: key.script,
            glyphs,
            stamp: self.tick,
        };
        let ways = self.ways;
        let set = &mut self.sets[hash as usize & self.mask];
        if set.len() < ways {
            set.push(entry);
        } else if let Some(victim) = set.iter_mut().min_by_key(|e| e.stamp) {
            *victim = entry;
        }
    }
}

// ----------------------------------------------------------------------- shaper

type PlanKey = (u64, [u8; 4], u64);

/// Shapes rows. Holds the shape cache and the shape-plan cache; both are dropped when the font
/// generation changes.
pub struct RowShaper {
    cache: ShapeCache,
    plans: HashMap<PlanKey, ShapePlan>,
    generation: u64,
    scratch_text: String,
    scratch_clusters: Vec<(u32, usize)>,
    scratch_res: Vec<(Kind, crate::system::Resolved)>,
    /// Shaper invocations (misses that ran harfrust), for tests and statistics.
    pub shaped: u64,
}

impl std::fmt::Debug for RowShaper {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RowShaper")
            .field("cached_runs", &self.cache.len())
            .field("plans", &self.plans.len())
            .field("generation", &self.generation)
            .finish_non_exhaustive()
    }
}

impl Default for RowShaper {
    fn default() -> Self {
        Self::new()
    }
}

fn hash_features(f: &[FontFeature]) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    f.hash(&mut h);
    h.finish()
}

fn to_harfrust(f: &[FontFeature]) -> Vec<Feature> {
    f.iter()
        .map(|f| Feature::new(Tag::new(&f.tag), f.value, ..))
        .collect()
}

/// Simple characters the face can map 1:1 without the shaper when no feature applies.
fn fast_ok(cp: u32) -> bool {
    (0x20..=0x7E).contains(&cp) || (0xA0..0x300).contains(&cp) || (0x4E00..=0x9FFF).contains(&cp)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Face(u64),
    Sprite,
    Tofu,
}

impl RowShaper {
    /// A shaper with the default cache geometry.
    pub fn new() -> Self {
        Self::with_cache(ShapeCache::DEFAULT_SETS, ShapeCache::DEFAULT_WAYS)
    }

    /// A shaper with an explicit cache geometry (`font.shape-cache-size`).
    pub fn with_cache(sets: usize, ways: usize) -> Self {
        Self {
            cache: ShapeCache::new(sets, ways),
            plans: HashMap::new(),
            generation: 0,
            scratch_text: String::new(),
            scratch_clusters: Vec::new(),
            scratch_res: Vec::new(),
            shaped: 0,
        }
    }

    /// The shape cache (statistics).
    pub fn cache(&self) -> &ShapeCache {
        &self.cache
    }

    /// Shapes one row at `px_size` pixels per em into `out` (cleared first). `cursor_col` is the
    /// cursor cell when the ligature policy needs to un-fuse it.
    pub fn shape_row(
        &mut self,
        fonts: &mut FontSystem,
        px_size: f32,
        row: &[RowCluster<'_>],
        cursor_col: Option<u16>,
        out: &mut ShapedRow,
    ) {
        out.clear();
        if self.generation != fonts.generation() {
            self.cache.clear();
            self.plans.clear();
            self.generation = fonts.generation();
        }
        let policy = fonts.config().ligatures;
        let builtin_on = fonts.config().builtin_glyphs;
        let size_bits = px_size.to_bits();

        let mut res = std::mem::take(&mut self.scratch_res);
        res.clear();
        for c in row {
            res.push(Self::classify(fonts, builtin_on, c));
        }

        let mut i = 0;
        while i < row.len() {
            // Extend the run while face, style and script agree.
            let head = row[i];
            let (kind, resolved) = res[i].clone();
            let isolate_head =
                policy == Ligatures::NotUnderCursor && cursor_col.is_some_and(|c| covers(&head, c));
            let mut script: Option<[u8; 4]> = first_strong_script(head.text);
            let mut j = i + 1;
            if !isolate_head {
                while j < row.len() {
                    let c = row[j];
                    if c.style != head.style {
                        break;
                    }
                    if policy == Ligatures::NotUnderCursor
                        && cursor_col.is_some_and(|cc| covers(&c, cc))
                    {
                        break;
                    }
                    if res[j].0 != kind {
                        break;
                    }
                    if let Some(s) = first_strong_script(c.text) {
                        match script {
                            Some(cur) if cur != s => break,
                            None => script = Some(s),
                            _ => {}
                        }
                    }
                    j += 1;
                }
            }
            let cluster_slice = &row[i..j];
            let first_col = cluster_slice[0].col;
            let last = cluster_slice[cluster_slice.len() - 1];
            let end_col = last.col + u16::from(last.cells);
            let g0 = out.glyphs.len();
            let mut fast_path = false;
            match kind {
                Kind::Sprite => {
                    for c in cluster_slice {
                        let cp = c.text.chars().next().map_or(0, |ch| ch as u32);
                        out.glyphs.push(PlacedGlyph {
                            source: GlyphSource::Sprite(cp),
                            col: c.col,
                            span: u16::from(c.cells),
                            x_offset: 0,
                            y_offset: 0,
                        });
                    }
                }
                Kind::Tofu => {
                    for c in cluster_slice {
                        let cp = c.text.chars().next().map_or(0, |ch| ch as u32);
                        out.glyphs.push(PlacedGlyph {
                            source: GlyphSource::Tofu(cp),
                            col: c.col,
                            span: u16::from(c.cells),
                            x_offset: 0,
                            y_offset: 0,
                        });
                    }
                }
                Kind::Face(_) => {
                    if let Some(inst) = &resolved.face {
                        fast_path = self.shape_run(
                            fonts,
                            inst,
                            head.style,
                            policy,
                            px_size,
                            size_bits,
                            script.unwrap_or(*b"Latn"),
                            cluster_slice,
                            &mut out.glyphs,
                        );
                    }
                }
            }
            out.runs.push(ShapedRun {
                face: resolved.face.clone(),
                style: head.style,
                rule: resolved.rule,
                first_col,
                end_col,
                range: (g0, out.glyphs.len()),
                fast_path,
            });
            i = j;
        }
        self.scratch_res = res;
    }

    fn classify(
        fonts: &mut FontSystem,
        builtin_on: bool,
        c: &RowCluster<'_>,
    ) -> (Kind, crate::system::Resolved) {
        if builtin_on {
            let mut it = c.text.chars();
            if let (Some(ch), None) = (it.next(), it.next())
                && builtin::is_sprite(ch as u32)
            {
                return (
                    Kind::Sprite,
                    crate::system::Resolved {
                        face: None,
                        rule: Rule::StyleFamily,
                        partial: false,
                    },
                );
            }
        }
        let r = fonts.resolve(c.style, c.text);
        match &r.face {
            Some(f) => (Kind::Face(f.key()), r),
            None => (Kind::Tofu, r),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn shape_run(
        &mut self,
        fonts: &FontSystem,
        inst: &Arc<FaceInstance>,
        style: Style,
        policy: Ligatures,
        px_size: f32,
        size_bits: u32,
        script: [u8; 4],
        clusters: &[RowCluster<'_>],
        glyphs_out: &mut Vec<PlacedGlyph>,
    ) -> bool {
        let face = inst.face();
        let ligatures_enabled = policy != Ligatures::Off;
        let features = fonts.config().effective_features(style, ligatures_enabled);
        let user_features_empty = fonts.config().style(style).features.is_empty();

        // Simple-text fast path (FONTS-12): every cluster one simple code point the face maps,
        // no feature configured, and either ligatures are off or the face has none.
        let glyph_of = |cp: u32| -> Option<u32> {
            if !fast_ok(cp) {
                None
            } else if cp < 0x300 {
                face.latin_glyph(cp)
            } else {
                face.glyph_for(cp)
            }
        };
        let single_cp = |c: &RowCluster<'_>| {
            let mut it = c.text.chars();
            match (it.next(), it.next()) {
                (Some(ch), None) => Some(ch as u32),
                _ => None,
            }
        };
        let fast = user_features_empty
            && (!ligatures_enabled || !face.has_ligature_features())
            && clusters
                .iter()
                .all(|c| single_cp(c).is_some_and(|cp| glyph_of(cp).is_some()));
        if fast {
            for c in clusters {
                let g = single_cp(c).and_then(glyph_of).unwrap_or(0);
                glyphs_out.push(PlacedGlyph {
                    source: GlyphSource::Glyph(g),
                    col: c.col,
                    span: u16::from(c.cells),
                    x_offset: 0,
                    y_offset: 0,
                });
            }
            return true;
        }

        // Slow path: the run text, cached by (text, face, features, size).
        self.scratch_text.clear();
        self.scratch_clusters.clear();
        for (idx, c) in clusters.iter().enumerate() {
            self.scratch_clusters
                .push((self.scratch_text.len() as u32, idx));
            self.scratch_text.push_str(c.text);
        }
        let fhash = hash_features(&features);
        let key = ShapeKey {
            text: &self.scratch_text,
            face_key: inst.key(),
            features: fhash,
            size_bits,
            script,
        };
        let shaped = match self.cache.get(&key) {
            Some(g) => g,
            None => {
                let g: Arc<[ShapedGlyph]> = shape_uncached(
                    &mut self.plans,
                    inst,
                    &self.scratch_text,
                    script,
                    &features,
                    fhash,
                )
                .into();
                self.shaped += 1;
                self.cache.insert(&key, g.clone());
                g
            }
        };

        let upem = f32::from(face.units_per_em().max(1));
        let scale = px_size / upem;
        let n = clusters.len();
        let mut gi = 0;
        while gi < shaped.len() {
            let cl = shaped[gi].cluster;
            let mut gj = gi + 1;
            while gj < shaped.len() && shaped[gj].cluster == cl {
                gj += 1;
            }
            let start_idx = cluster_index(&self.scratch_clusters, cl);
            let end_idx = if gj < shaped.len() {
                cluster_index(&self.scratch_clusters, shaped[gj].cluster).max(start_idx + 1)
            } else {
                n
            };
            let end_idx = end_idx.clamp(start_idx + 1, n.max(start_idx + 1));
            let col = clusters[start_idx.min(n - 1)].col;
            let last = clusters[(end_idx - 1).min(n - 1)];
            let span = (last.col + u16::from(last.cells))
                .saturating_sub(col)
                .max(1);
            for g in &shaped[gi..gj] {
                glyphs_out.push(PlacedGlyph {
                    source: GlyphSource::Glyph(g.glyph_id),
                    col,
                    span,
                    x_offset: (g.x_offset as f32 * scale).round() as i32,
                    y_offset: (g.y_offset as f32 * scale).round() as i32,
                });
            }
            gi = gj;
        }
        false
    }
}

fn covers(c: &RowCluster<'_>, col: u16) -> bool {
    col >= c.col && col < c.col + u16::from(c.cells)
}

fn first_strong_script(text: &str) -> Option<[u8; 4]> {
    text.chars()
        .map(|c| script_of(c as u32))
        .find(|&s| !script_is_weak(s))
}

fn cluster_index(starts: &[(u32, usize)], byte: u32) -> usize {
    let p = starts.partition_point(|&(b, _)| b <= byte);
    p.saturating_sub(1)
}

fn shape_uncached(
    plans: &mut HashMap<PlanKey, ShapePlan>,
    inst: &FaceInstance,
    text: &str,
    script: [u8; 4],
    features: &[FontFeature],
    fhash: u64,
) -> Vec<ShapedGlyph> {
    let face = inst.face();
    let Some(font) = face.font() else {
        return Vec::new();
    };
    let shaper = face
        .shaper_data()
        .shaper(&font)
        .instance(Some(inst.shaper_instance()))
        .build();
    let hb_script = Script::from_iso15924_tag(Tag::new(&script));
    let hb_features = to_harfrust(features);
    let plan = plans.entry((inst.key(), script, fhash)).or_insert_with(|| {
        ShapePlan::new(
            &shaper,
            Direction::LeftToRight,
            hb_script,
            None,
            &hb_features,
        )
    });
    let mut buf = UnicodeBuffer::new();
    buf.push_str(text);
    buf.set_direction(Direction::LeftToRight);
    if let Some(s) = hb_script {
        buf.set_script(s);
    }
    let out = shaper.shape(
        buf,
        ShapeOptions::new()
            .plan(Some(&*plan))
            .features(&hb_features),
    );
    out.glyph_infos()
        .iter()
        .zip(out.glyph_positions())
        .map(|(i, p)| ShapedGlyph {
            glyph_id: i.glyph_id,
            cluster: i.cluster,
            x_advance: p.x_advance,
            x_offset: p.x_offset,
            y_offset: p.y_offset,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(text: &str) -> ShapeKey<'_> {
        ShapeKey {
            text,
            face_key: 1,
            features: 2,
            size_bits: 3,
            script: *b"Latn",
        }
    }

    fn g(id: u32) -> Arc<[ShapedGlyph]> {
        vec![ShapedGlyph {
            glyph_id: id,
            cluster: 0,
            x_advance: 1,
            x_offset: 0,
            y_offset: 0,
        }]
        .into()
    }

    #[test]
    fn cache_hit_miss_and_key_fields() {
        let mut c = ShapeCache::new(4, 2);
        assert!(c.get(&key("a")).is_none());
        c.insert(&key("a"), g(7));
        assert_eq!(c.get(&key("a")).unwrap()[0].glyph_id, 7);
        assert!(
            c.get(&ShapeKey {
                face_key: 9,
                ..key("a")
            })
            .is_none()
        );
        assert!(
            c.get(&ShapeKey {
                size_bits: 9,
                ..key("a")
            })
            .is_none()
        );
        assert!(
            c.get(&ShapeKey {
                features: 9,
                ..key("a")
            })
            .is_none()
        );
        assert_eq!((c.hits, c.misses), (1, 4));
    }

    #[test]
    fn cache_is_bounded_and_lru_within_a_set() {
        // One set of 2 ways: the least recently used of three keys is evicted.
        let mut c = ShapeCache::new(1, 2);
        c.insert(&key("a"), g(1));
        c.insert(&key("b"), g(2));
        assert!(c.get(&key("a")).is_some()); // a is now newest
        c.insert(&key("c"), g(3));
        assert_eq!(c.len(), 2);
        assert!(c.get(&key("b")).is_none());
        assert!(c.get(&key("a")).is_some());
        assert!(c.get(&key("c")).is_some());
        c.clear();
        assert!(c.is_empty());
    }

    #[test]
    fn many_inserts_stay_within_capacity() {
        let mut c = ShapeCache::new(8, 4);
        for i in 0..1000 {
            let t = format!("run{i}");
            c.insert(&key(&t), g(i));
        }
        assert!(c.len() <= 32);
    }

    #[test]
    fn slice_source_reuses_middle_for_long_repeats() {
        let mk = |span| PlacedGlyph {
            source: GlyphSource::Glyph(1),
            col: 0,
            span,
            x_offset: 0,
            y_offset: 0,
        };
        let short = mk(3);
        assert_eq!([0, 1, 2].map(|i| short.slice_source(i)), [0, 1, 2]);
        let long = mk(12);
        assert_eq!(long.slice_source(0), 0);
        assert_eq!(long.slice_source(11), 11);
        assert!((1..11).all(|i| long.slice_source(i) == 1));
        let edge = mk(9); // exactly nine is not infinite
        assert_eq!(edge.slice_source(4), 4);
    }

    #[test]
    fn cluster_index_maps_bytes() {
        let starts = [(0u32, 0usize), (1, 1), (4, 2)];
        assert_eq!(cluster_index(&starts, 0), 0);
        assert_eq!(cluster_index(&starts, 1), 1);
        assert_eq!(cluster_index(&starts, 4), 2);
        assert_eq!(cluster_index(&starts, 3), 1);
    }

    #[test]
    fn fast_ok_set() {
        assert!(fast_ok('a' as u32) && fast_ok(0xE9) && fast_ok('中' as u32));
        assert!(!fast_ok(0x301) && !fast_ok(0x41F) && !fast_ok(0x7F));
    }
}
