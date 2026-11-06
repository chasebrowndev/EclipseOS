// SPDX-License-Identifier: AGPL-3.0-only
//! The hints the freedesktop spec defines that we carry through, reduced to
//! plain data. Everything is defensive: a malformed hint is dropped, never an
//! error, because this server never fails a `Notify` it can partly read.
//!
//! Notification bodies, and the pixels of `image-data`, are never logged.

use std::collections::HashMap;

use zbus::zvariant::{OwnedValue, Value};

/// Largest `image-data` edge accepted. A bigger one is dropped (the client
/// should also have sent an icon name, which then shows instead).
pub const MAX_IMAGE_EDGE: u32 = 512;

/// A decoded `image-data` hint: straight (non-premultiplied) RGBA8, row-major,
/// no padding, `rgba.len() == width * height * 4`.
#[derive(Clone, PartialEq, Eq, Default)]
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

impl std::fmt::Debug for Image {
    // Pixels can be a contact's photo: show the shape, not the bytes.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Image({}x{})", self.width, self.height)
    }
}

/// The three sound hints. Played by `sound.rs`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Sound {
    /// `sound-name`: a freedesktop sound theme name.
    pub name: String,
    /// `sound-file`: an absolute path.
    pub file: String,
    /// `suppress-sound`: the client asks for silence.
    pub suppress: bool,
}

type RawImage = (i32, i32, i32, bool, i32, i32, Vec<u8>);

impl Image {
    /// Decode the spec's `(iiibiiay)`: width, height, rowstride, has_alpha,
    /// bits_per_sample, channels, data. Only 8 bits per sample with 3 or 4
    /// channels is understood.
    pub(crate) fn from_raw(raw: RawImage) -> Option<Self> {
        let (w, h, stride, has_alpha, bits, channels, data) = raw;
        if bits != 8 || !(3..=4).contains(&channels) || has_alpha != (channels == 4) {
            return None;
        }
        let (w, h, stride, ch) = (
            u32::try_from(w).ok()?,
            u32::try_from(h).ok()?,
            usize::try_from(stride).ok()?,
            usize::try_from(channels).ok()?,
        );
        if w == 0 || h == 0 || w > MAX_IMAGE_EDGE || h > MAX_IMAGE_EDGE {
            return None;
        }
        let row = w as usize * ch;
        if stride < row || data.len() < stride * (h as usize - 1) + row {
            return None;
        }
        let mut rgba = Vec::with_capacity(w as usize * h as usize * 4);
        for y in 0..h as usize {
            let line = &data[y * stride..y * stride + row];
            for px in line.chunks_exact(ch) {
                rgba.extend_from_slice(&px[..3]);
                rgba.push(if ch == 4 { px[3] } else { 255 });
            }
        }
        Some(Self {
            width: w,
            height: h,
            rgba,
        })
    }
}

fn raw_image(v: &OwnedValue) -> Option<RawImage> {
    let v: Value<'_> = v.try_clone().ok()?.into();
    RawImage::try_from(v).ok()
}

/// `image-data`, with the older spellings still sent by some toolkits.
pub(crate) fn image(hints: &HashMap<String, OwnedValue>) -> Option<Image> {
    ["image-data", "image_data", "icon_data"]
        .iter()
        .find_map(|k| hints.get(*k))
        .and_then(raw_image)
        .and_then(Image::from_raw)
}

fn string(hints: &HashMap<String, OwnedValue>, key: &str) -> Option<String> {
    hints
        .get(key)
        .and_then(|v| v.downcast_ref::<&str>().ok())
        .map(str::to_owned)
}

fn boolean(hints: &HashMap<String, OwnedValue>, key: &str) -> bool {
    hints
        .get(key)
        .and_then(|v| v.downcast_ref::<bool>().ok())
        .unwrap_or(false)
}

/// An integer hint in whichever width the client picked.
fn integer(hints: &HashMap<String, OwnedValue>, key: &str) -> Option<i64> {
    let v = hints.get(key)?;
    v.downcast_ref::<i32>()
        .map(i64::from)
        .or_else(|_| v.downcast_ref::<u32>().map(i64::from))
        .or_else(|_| v.downcast_ref::<i64>())
        .or_else(|_| v.downcast_ref::<u8>().map(i64::from))
        .or_else(|_| v.downcast_ref::<i16>().map(i64::from))
        .or_else(|_| v.downcast_ref::<u16>().map(i64::from))
        .ok()
}

/// The `value` hint (some clients say `progress`), clamped to 0..=100.
pub(crate) fn progress(hints: &HashMap<String, OwnedValue>) -> Option<u8> {
    let v = integer(hints, "value").or_else(|| integer(hints, "progress"))?;
    Some(v.clamp(0, 100) as u8)
}

pub(crate) fn category(hints: &HashMap<String, OwnedValue>) -> String {
    string(hints, "category").unwrap_or_default()
}

/// `desktop-entry`, without a trailing `.desktop`.
pub(crate) fn desktop_entry(hints: &HashMap<String, OwnedValue>) -> String {
    let s = string(hints, "desktop-entry").unwrap_or_default();
    s.strip_suffix(".desktop").unwrap_or(&s).to_owned()
}

pub(crate) fn resident(hints: &HashMap<String, OwnedValue>) -> bool {
    boolean(hints, "resident")
}

pub(crate) fn sound(hints: &HashMap<String, OwnedValue>) -> Sound {
    Sound {
        name: string(hints, "sound-name").unwrap_or_default(),
        file: string(hints, "sound-file").unwrap_or_default(),
        suppress: boolean(hints, "suppress-sound"),
    }
}

/// The grouping key: the desktop entry if there is one, else the app name.
pub(crate) fn group_key(desktop_entry: &str, app_name: &str) -> String {
    if desktop_entry.is_empty() {
        app_name.to_lowercase()
    } else {
        desktop_entry.to_lowercase()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zbus::zvariant::StructureBuilder;

    fn hv<T>(v: T) -> OwnedValue
    where
        T: Into<Value<'static>>,
    {
        OwnedValue::try_from(v.into()).unwrap()
    }

    #[test]
    fn rgb_image_gains_alpha_and_loses_row_padding() {
        // 2x1, rgb, stride 8 (2 bytes of padding).
        let raw = (2, 1, 8, false, 8, 3, vec![1, 2, 3, 4, 5, 6, 0, 0]);
        let img = Image::from_raw(raw).unwrap();
        assert_eq!((img.width, img.height), (2, 1));
        assert_eq!(img.rgba, [1, 2, 3, 255, 4, 5, 6, 255]);
    }

    #[test]
    fn rgba_image_passes_through() {
        let img = Image::from_raw((1, 1, 4, true, 8, 4, vec![9, 8, 7, 6])).unwrap();
        assert_eq!(img.rgba, [9, 8, 7, 6]);
    }

    #[test]
    fn bad_images_are_dropped() {
        // Short data, odd depth, alpha flag lying, zero size, negative, too big.
        assert!(Image::from_raw((2, 2, 8, true, 8, 4, vec![0; 8])).is_none());
        assert!(Image::from_raw((1, 1, 4, true, 16, 4, vec![0; 4])).is_none());
        assert!(Image::from_raw((1, 1, 4, false, 8, 4, vec![0; 4])).is_none());
        assert!(Image::from_raw((0, 1, 4, true, 8, 4, vec![])).is_none());
        assert!(Image::from_raw((-1, 1, 4, true, 8, 4, vec![0; 4])).is_none());
        let big = MAX_IMAGE_EDGE as i32 + 1;
        assert!(Image::from_raw((big, 1, big * 4, true, 8, 4, vec![0; big as usize * 4])).is_none());
    }

    #[test]
    fn image_hint_decodes_from_the_wire_structure() {
        let mut h = HashMap::new();
        let s = StructureBuilder::new()
            .add_field(1_i32)
            .add_field(1_i32)
            .add_field(4_i32)
            .add_field(true)
            .add_field(8_i32)
            .add_field(4_i32)
            .add_field(vec![1_u8, 2, 3, 4])
            .build()
            .unwrap();
        h.insert("image-data".to_owned(), hv(s));
        assert_eq!(image(&h).unwrap().rgba, [1, 2, 3, 4]);
        assert!(image(&HashMap::new()).is_none());
    }

    #[test]
    fn progress_is_clamped_and_any_integer_width_works() {
        let mut h = HashMap::new();
        h.insert("value".to_owned(), hv(250_i32));
        assert_eq!(progress(&h), Some(100));
        h.insert("value".to_owned(), hv(-5_i32));
        assert_eq!(progress(&h), Some(0));
        h.insert("value".to_owned(), hv(42_u8));
        assert_eq!(progress(&h), Some(42));
        assert_eq!(progress(&HashMap::new()), None);
        h.clear();
        h.insert("value".to_owned(), hv("x"));
        assert_eq!(progress(&h), None);
    }

    #[test]
    fn strings_and_flags() {
        let mut h = HashMap::new();
        h.insert("category".to_owned(), hv("email.arrived"));
        h.insert("desktop-entry".to_owned(), hv("org.Foo.desktop"));
        h.insert("resident".to_owned(), hv(true));
        h.insert("sound-name".to_owned(), hv("message-new-instant"));
        h.insert("suppress-sound".to_owned(), hv(true));
        assert_eq!(category(&h), "email.arrived");
        assert_eq!(desktop_entry(&h), "org.Foo");
        assert!(resident(&h));
        let s = sound(&h);
        assert_eq!(s.name, "message-new-instant");
        assert!(s.file.is_empty() && s.suppress);
        assert_eq!(group_key("org.Foo", "Foo App"), "org.foo");
        assert_eq!(group_key("", "Foo App"), "foo app");
    }
}
