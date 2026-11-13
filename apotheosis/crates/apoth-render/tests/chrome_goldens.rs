// SPDX-License-Identifier: AGPL-3.0-only
//! Glass chrome pass goldens (T-08 §3, T-25 §5): the CPU reference of the chrome list is the
//! stored PNG; the wgpu pass is compared with it on whatever Vulkan adapter exists (lavapipe
//! counts). With no adapter the GPU test prints a skip and passes unless `APOTH_REQUIRE_GPU=1`.
//! Missing goldens fail; create them with `APOTH_BLESS=1` and read the PNGs before committing.

use apoth_render::chrome::{
    ChromeList, ChromeRect, ChromeShadow, ChromeSink, premultiply, reference as chrome_ref,
};
use apoth_render::gpu::{Gpu, GpuOptions};
use apoth_render::presenter::{OffscreenPresenter, PresentOutcome, Presenter};
use apoth_render::reference::{self, Image};
use apoth_render::renderer::{FrameInput, Renderer, RendererConfig};
use apoth_render::scene::{GridScene, RectInstance};
use std::path::PathBuf;

const W: usize = 200;
const H: usize = 120;
const BASE: [u8; 4] = [13, 15, 20, 255];
const TOLERANCE: u8 = 2;
const STRUCTURAL_FRACTION: f64 = 0.001;

fn rect(r: [f32; 4], radius: f32, fill: [u8; 4], rim: [u8; 4], rim_width: f32) -> ChromeRect {
    ChromeRect {
        rect: r,
        radii: [radius; 4],
        fill,
        rim,
        rim_width,
    }
}

/// A representative glass scene: shadowed card, a pill button, a toast clipped by its bar,
/// and a rect with mixed corner radii.
fn scene() -> ChromeList {
    let mut l = ChromeList::new();
    l.shadow(&ChromeShadow {
        rect: [20.0, 20.0, 120.0, 70.0],
        radii: [12.0; 4],
        spread: 10.0,
        alpha: 0.35,
    });
    l.rounded_rect(&rect(
        [20.0, 16.0, 120.0, 70.0],
        12.0,
        [255, 255, 255, 26],
        [255, 255, 255, 64],
        1.0,
    ));
    // Pill (radius = half height), gold accent fill, brighter rim.
    l.rounded_rect(&rect(
        [32.0, 28.0, 64.0, 20.0],
        10.0,
        [212, 175, 55, 200],
        [255, 230, 140, 255],
        1.0,
    ));
    // Clipped child: cut by the card's inner box.
    l.push_clip([30.0, 56.0, 90.0, 20.0]);
    l.rounded_rect(&rect(
        [100.0, 52.0, 70.0, 28.0],
        6.0,
        [120, 160, 255, 90],
        [120, 160, 255, 160],
        1.0,
    ));
    l.pop_clip();
    // Mixed radii, no rim.
    let mut r = rect(
        [150.0, 70.0, 40.0, 40.0],
        0.0,
        [255, 255, 255, 40],
        [0; 4],
        0.0,
    );
    r.radii = [16.0, 2.0, 16.0, 2.0];
    l.rounded_rect(&r);
    l
}

fn base_image() -> Image {
    let mut img = Image::new(W, H);
    img.fill(&RectInstance::new(0, 0, W as i32, H as i32, BASE));
    img
}

fn snapshot_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests")
}

fn check_png(name: &str, img: &Image) {
    let path = snapshot_dir().join("golden").join(format!("{name}.png"));
    let straight = img.to_straight();
    if std::env::var_os("APOTH_BLESS").is_some_and(|v| v == "1") {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let f = std::io::BufWriter::new(std::fs::File::create(&path).unwrap());
        let mut enc = png::Encoder::new(f, img.w as u32, img.h as u32);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        enc.write_header()
            .unwrap()
            .write_image_data(&straight)
            .unwrap();
        return;
    }
    let f = std::fs::File::open(&path).unwrap_or_else(|_| {
        panic!(
            "missing golden {}; run with APOTH_BLESS=1 and read it",
            path.display()
        )
    });
    let mut reader = png::Decoder::new(std::io::BufReader::new(f))
        .read_info()
        .unwrap();
    let mut buf = vec![0; reader.output_buffer_size().unwrap()];
    let info = reader.next_frame(&mut buf).unwrap();
    buf.truncate(info.line_size * info.height as usize);
    let want = Image {
        w: info.width as usize,
        h: info.height as usize,
        data: buf,
    };
    let got = Image {
        w: img.w,
        h: img.h,
        data: straight,
    };
    assert_eq!((want.w, want.h), (got.w, got.h), "golden {name}: size");
    let d = reference::diff(&want, &got, TOLERANCE);
    let allowed = (d.total as f64 * STRUCTURAL_FRACTION) as usize;
    assert!(
        d.differing <= allowed,
        "golden {name}: {} of {} pixels differ (max channel diff {})",
        d.differing,
        d.total,
        d.max_channel
    );
}

fn px(img: &Image, x: usize, y: usize) -> [u8; 4] {
    let i = (y * img.w + x) * 4;
    img.data[i..i + 4].try_into().unwrap()
}

#[test]
fn glass_scene_matches_its_golden() {
    let mut img = base_image();
    chrome_ref::render(&scene(), &mut img);
    check_png("chrome_glass_scene", &img);
}

/// Over a transparent window the fill stays translucent (so the compositor's blur shows
/// through), the corner is cut by the radius, and the rim is brighter than the fill.
#[test]
fn translucent_fill_keeps_window_alpha_below_one_and_corners_are_cut() {
    let mut l = ChromeList::new();
    l.rounded_rect(&rect(
        [10.0, 10.0, 100.0, 60.0],
        12.0,
        [255, 255, 255, 26],
        [255, 255, 255, 102],
        1.0,
    ));
    let mut img = Image::new(120, 80);
    chrome_ref::render(&l, &mut img);
    assert_eq!(
        px(&img, 10, 10),
        [0; 4],
        "corner pixel is outside the radius"
    );
    assert_eq!(px(&img, 3, 3), [0; 4]);
    let mid = px(&img, 60, 40);
    assert_eq!(
        mid,
        premultiply([255, 255, 255, 26]),
        "interior is the premultiplied fill"
    );
    assert!(mid[3] < 255);
    let rim = px(&img, 60, 10);
    assert!(
        rim[3] > mid[3],
        "rim {rim:?} must be brighter than fill {mid:?}"
    );
    // The fill is premultiplied: no channel exceeds alpha.
    assert!(
        img.data
            .chunks_exact(4)
            .all(|p| p[..3].iter().all(|&c| c <= p[3]))
    );
}

#[test]
fn rim_is_one_pixel_wide_and_crisp() {
    let mut l = ChromeList::new();
    l.rounded_rect(&rect(
        [10.0, 10.0, 40.0, 40.0],
        0.0,
        [0, 0, 0, 0],
        [255, 255, 255, 255],
        1.0,
    ));
    let mut img = Image::new(60, 60);
    chrome_ref::render(&l, &mut img);
    assert_eq!(px(&img, 10, 30), [255; 4]);
    assert_eq!(px(&img, 11, 30), [0; 4], "rim is a single pixel");
    assert_eq!(px(&img, 9, 30), [0; 4]);
}

#[test]
fn shadow_is_outside_only_and_fades() {
    let mut l = ChromeList::new();
    l.shadow(&ChromeShadow {
        rect: [30.0, 30.0, 40.0, 40.0],
        radii: [8.0; 4],
        spread: 12.0,
        alpha: 0.5,
    });
    let mut img = Image::new(100, 100);
    chrome_ref::render(&l, &mut img);
    assert_eq!(px(&img, 50, 50)[3], 0, "nothing under the casting rect");
    let near = px(&img, 50, 30 - 2)[3];
    let far = px(&img, 50, 30 - 10)[3];
    assert!(near > far && far > 0, "near {near} far {far}");
    assert_eq!(px(&img, 50, 30 - 14)[3], 0, "no shadow past the spread");
}

#[test]
fn clip_confines_a_rect() {
    let mut l = ChromeList::new();
    l.push_clip([0.0, 0.0, 50.0, 100.0]);
    l.rounded_rect(&rect(
        [0.0, 0.0, 100.0, 100.0],
        0.0,
        [255, 0, 0, 255],
        [0; 4],
        0.0,
    ));
    l.pop_clip();
    let mut img = Image::new(100, 100);
    chrome_ref::render(&l, &mut img);
    assert_eq!(px(&img, 40, 50), [255, 0, 0, 255]);
    assert_eq!(px(&img, 60, 50), [0; 4]);
}

fn gpu() -> Option<Gpu> {
    match Gpu::headless(&GpuOptions {
        allow_software: true,
        compositor_hint: None,
    }) {
        Ok(g) => Some(g),
        Err(e) => {
            assert!(
                std::env::var_os("APOTH_REQUIRE_GPU").is_none_or(|v| v != "1"),
                "APOTH_REQUIRE_GPU=1 and no Vulkan adapter: {e}"
            );
            eprintln!("SKIP chrome gpu golden: no Vulkan adapter ({e})");
            None
        }
    }
}

#[test]
fn gpu_chrome_pass_matches_the_reference() {
    let Some(gpu) = gpu() else { return };
    let mut renderer = Renderer::new(&gpu, &RendererConfig::default());
    let mut pres = OffscreenPresenter::new(&gpu, (W as u32, H as u32));
    let list = scene();
    renderer.chrome_mut().clone_from(&list);
    let scene_grid = GridScene::new();
    let input = FrameInput {
        scene: &scene_grid,
        cursor: None,
        clear: BASE.map(|b| f32::from(b) / 255.0),
    };
    match pres.present(&mut renderer, &input) {
        PresentOutcome::Presented(_) => {}
        other => panic!("offscreen present failed: {other:?}"),
    }
    let got = pres.read_pixels();
    let mut want = base_image();
    chrome_ref::render(&list, &mut want);
    let d = reference::diff(&got, &want, 6);
    let allowed = (d.total as f64 * 0.005) as usize;
    assert!(
        d.differing <= allowed,
        "gpu chrome: {} of {} pixels differ by more than 6 (max channel diff {})",
        d.differing,
        d.total,
        d.max_channel
    );
}

/// The flat kind (glyph coverage from the text bridge) covers exactly its integer rect, with
/// no antialiased edge, and honours clips.
#[test]
fn flat_rects_are_exact_and_clipped() {
    let mut l = ChromeList::new();
    l.flat([4.0, 4.0, 3.0, 1.0], [255, 255, 255, 128]);
    l.push_clip([0.0, 0.0, 5.0, 10.0]);
    l.flat([10.0, 10.0, 2.0, 2.0], [255, 0, 0, 255]);
    l.flat([2.0, 6.0, 6.0, 1.0], [0, 255, 0, 255]);
    l.pop_clip();
    let mut img = Image::new(16, 16);
    chrome_ref::render(&l, &mut img);
    let want = premultiply([255, 255, 255, 128]);
    for x in 4..7 {
        assert_eq!(px(&img, x, 4), want);
    }
    assert_eq!(px(&img, 3, 4), [0; 4]);
    assert_eq!(px(&img, 7, 4), [0; 4]);
    assert_eq!(px(&img, 4, 3), [0; 4]);
    assert_eq!(px(&img, 10, 10), [0; 4], "outside the clip");
    assert_eq!(px(&img, 4, 6), [0, 255, 0, 255]);
    assert_eq!(px(&img, 5, 6), [0; 4], "clipped at x = 5");
}
