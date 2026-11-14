// SPDX-License-Identifier: AGPL-3.0-only
use apoth_image::kitty::{DeleteSpec, DeleteTarget, PlaceSpec};
use apoth_image::store::*;
use apoth_image::{ErrCode, Image, Limits};

fn img(seed: u8) -> Image {
    // 10x25 RGBA = 1000 bytes
    Image {
        width: 10,
        height: 25,
        rgba: vec![seed; 1000],
    }
}

fn spec(z: i32) -> PlaceSpec {
    PlaceSpec {
        src_x: 0,
        src_y: 0,
        src_w: 0,
        src_h: 0,
        cols: 0,
        rows: 0,
        x_off: 0,
        y_off: 0,
        z,
        no_cursor_move: false,
        unicode: false,
        relative: None,
    }
}

fn store(quota: u64, total: u64) -> ImageStore {
    let lim = Limits {
        quota,
        quota_total: total,
        ..Limits::default()
    };
    ImageStore::new(lim, ProcessBudget::new())
}

const M: Screen = Screen::Main;

fn no_pos(_: &Placement) -> bool {
    false
}

#[test]
fn ids_numbers_and_auto_upper_half() {
    let mut s = store(1 << 20, 1 << 30);
    let r = s.insert(M, 5, 0, img(1)).unwrap();
    assert_eq!(r.id, 5);
    let auto = s.insert(M, 0, 77, img(2)).unwrap().id;
    assert!(auto >= AUTO_ID_BASE);
    assert_eq!(s.lookup_number(M, 77), Some(auto));
    assert!(s.get(M, auto).is_some());
    assert!(s.get(Screen::Alt, auto).is_none());
    let pid = s.place(M, 5, 0, spec(0)).unwrap();
    assert!(pid >= AUTO_ID_BASE);
    assert_eq!(s.place(M, 999, 0, spec(0)).unwrap_err(), ErrCode::Enoent);
}

#[test]
fn identical_payloads_share_one_image() {
    let mut s = store(1 << 20, 1 << 30);
    s.insert(M, 1, 0, img(9)).unwrap();
    s.insert(M, 2, 0, img(9)).unwrap();
    s.insert(Screen::Alt, 1, 0, img(9)).unwrap();
    assert_eq!(s.used(), 1000);
    assert_eq!(s.image_count(), 3);
    s.clear(Screen::Alt);
    assert_eq!(s.used(), 1000);
    let d = DeleteSpec {
        target: DeleteTarget::Id {
            id: 1,
            placement: 0,
        },
        free: true,
    };
    s.delete(M, &d, &no_pos);
    assert_eq!(s.used(), 1000, "id 2 still references the shared pixels");
    let d = DeleteSpec {
        target: DeleteTarget::Id {
            id: 2,
            placement: 0,
        },
        free: true,
    };
    s.delete(M, &d, &no_pos);
    assert_eq!(s.used(), 0);
}

#[test]
fn tiered_eviction() {
    let mut s = store(3000, 1 << 30);
    for (id, seed) in [(1, 1), (2, 2), (3, 3)] {
        s.insert(M, id, 0, img(seed)).unwrap();
    }
    s.place(M, 2, 1, spec(0)).unwrap(); // visible
    s.place(M, 3, 1, spec(0)).unwrap();
    s.set_visible(M, 3, 1, false); // off screen
    // 1 is unreferenced -> evicted first
    let r = s.insert(M, 4, 0, img(4)).unwrap();
    assert_eq!(r.evicted, vec![(M, 1)]);
    // next: only off-screen placement (3) is evictable; 2 (visible) and 4 (unreferenced, newest) ...
    // 4 is unreferenced so it goes before 3 (tier 1 precedes tier 2)
    let r = s.insert(M, 5, 0, img(5)).unwrap();
    assert_eq!(r.evicted, vec![(M, 4)]);
    let r = s.insert(M, 6, 0, img(6)).unwrap();
    assert_eq!(r.evicted, vec![(M, 5)]);
    // reference everything on screen: nothing evictable -> ENOSPC, nothing changed
    s.place(M, 6, 1, spec(0)).unwrap();
    s.set_visible(M, 3, 1, true);
    let before = s.used();
    let e = s.insert(M, 7, 0, img(7)).unwrap_err();
    assert_eq!(e, StoreError::NoSpace);
    assert_eq!(e.code(), ErrCode::Enospc);
    assert_eq!(s.used(), before);
    assert!(s.get(M, 2).is_some() && s.get(M, 3).is_some() && s.get(M, 6).is_some());
    // off-screen placement is evictable again, oldest first
    s.set_visible(M, 3, 1, false);
    let r = s.insert(M, 7, 0, img(7)).unwrap();
    assert_eq!(r.evicted, vec![(M, 3)]);
}

#[test]
fn replacing_an_id_reclaims_its_bytes() {
    let mut s = store(1000, 1 << 30);
    s.insert(M, 1, 0, img(1)).unwrap();
    s.place(M, 1, 1, spec(0)).unwrap(); // visible: not evictable, but being replaced
    s.insert(M, 1, 0, img(2)).unwrap();
    assert_eq!(s.used(), 1000);
    assert!(
        s.placements().is_empty(),
        "replacing drops the old placements"
    );
}

#[test]
fn too_large_and_process_budget() {
    let mut s = store(500, 1 << 30);
    assert_eq!(s.insert(M, 1, 0, img(1)).unwrap_err(), StoreError::TooLarge);
    let budget = ProcessBudget::new();
    let lim = Limits {
        quota: 10_000,
        quota_total: 1500,
        ..Limits::default()
    };
    let mut a = ImageStore::new(lim.clone(), budget.clone());
    let mut b = ImageStore::new(lim, budget.clone());
    a.insert(M, 1, 0, img(1)).unwrap();
    assert_eq!(budget.used(), 1000);
    // b has room under its own quota but the process is full and b has nothing to evict
    assert_eq!(b.insert(M, 1, 0, img(2)).unwrap_err(), StoreError::NoSpace);
    drop(a);
    assert_eq!(budget.used(), 0);
    b.insert(M, 1, 0, img(2)).unwrap();
    assert_eq!(budget.used(), 1000);
}

#[test]
fn delete_lowercase_keeps_data_uppercase_frees() {
    let mut s = store(1 << 20, 1 << 30);
    s.insert(M, 1, 0, img(1)).unwrap();
    s.place(M, 1, 1, spec(0)).unwrap();
    s.place(M, 1, 2, spec(0)).unwrap();
    let low = DeleteSpec {
        target: DeleteTarget::Id {
            id: 1,
            placement: 1,
        },
        free: false,
    };
    assert_eq!(s.delete(M, &low, &no_pos).placements_removed, 1);
    assert!(s.get(M, 1).is_some());
    let up = DeleteSpec {
        target: DeleteTarget::Id {
            id: 1,
            placement: 2,
        },
        free: true,
    };
    let r = s.delete(M, &up, &no_pos);
    assert_eq!((r.placements_removed, r.images_freed), (1, vec![1]));
    assert!(s.get(M, 1).is_none());
    assert_eq!(s.used(), 0);
}

#[test]
fn delete_selectors() {
    let mut s = store(1 << 20, 1 << 30);
    for id in 1..=4u32 {
        s.insert(M, id, id + 100, img(id as u8)).unwrap();
        s.place(M, id, 1, spec(id as i32)).unwrap();
    }
    s.set_visible(M, 4, 1, false);
    let d = |t, free| DeleteSpec { target: t, free };
    // z
    assert_eq!(
        s.delete(M, &d(DeleteTarget::Z(2), false), &no_pos)
            .placements_removed,
        1
    );
    // number
    let r = s.delete(
        M,
        &d(
            DeleteTarget::Number {
                number: 103,
                placement: 0,
            },
            true,
        ),
        &no_pos,
    );
    assert_eq!(r.images_freed, vec![3]);
    // positional via the grid predicate (column selector)
    let at_col = |p: &Placement| p.image == 1;
    assert_eq!(
        s.delete(M, &d(DeleteTarget::Column(0), false), &at_col)
            .placements_removed,
        1
    );
    // visible only: 4 is off screen and survives
    s.place(M, 1, 9, spec(0)).unwrap();
    assert_eq!(
        s.delete(M, &d(DeleteTarget::Visible, false), &no_pos)
            .placements_removed,
        1
    );
    assert_eq!(s.placements().len(), 1);
    // id range frees images even without placements
    let r = s.delete(M, &d(DeleteTarget::IdRange { lo: 1, hi: 4 }, true), &no_pos);
    assert_eq!(r.images_freed, vec![1, 2, 4]);
    assert_eq!(s.image_count(), 0);
    assert_eq!(s.used(), 0);
}

#[test]
fn alt_screen_partition() {
    let mut s = store(1 << 20, 1 << 30);
    s.insert(M, 1, 0, img(1)).unwrap();
    s.insert(Screen::Alt, 1, 0, img(2)).unwrap();
    s.place(Screen::Alt, 1, 1, spec(0)).unwrap();
    assert!(s.get(M, 1).is_some() && s.get(Screen::Alt, 1).is_some());
    s.exit_alt();
    assert!(s.get(Screen::Alt, 1).is_none());
    assert!(s.get(M, 1).is_some());
    assert!(s.placements().is_empty());
    assert_eq!(s.used(), 1000);
}

#[test]
fn rows_leaving_history_drop_placements() {
    let mut s = store(1 << 20, 1 << 30);
    s.insert(M, 1, 0, img(1)).unwrap();
    s.place(M, 1, 1, spec(0)).unwrap();
    s.drop_placement(M, 1, 1);
    assert!(s.placements().is_empty());
    assert!(s.get(M, 1).is_some());
}

#[test]
fn clear_frees_everything() {
    let mut s = store(1 << 20, 1 << 30);
    for i in 1..5 {
        s.insert(M, i, 0, img(i as u8)).unwrap();
    }
    s.clear(M);
    assert_eq!((s.used(), s.image_count()), (0, 0));
}

#[test]
fn resend_same_image_to_same_id_keeps_blob() {
    let mut s = store(1 << 20, 1 << 30);
    s.insert(M, 5, 0, img(1)).unwrap();
    s.insert(M, 5, 0, img(1)).unwrap();
    assert!(s.get(M, 5).is_some());
    s.insert(M, 6, 0, img(2)).unwrap();
    assert_eq!(s.get(M, 5).unwrap().rgba[0], 1);
    assert_eq!(s.get(M, 6).unwrap().rgba[0], 2);
    assert_eq!(s.used(), 2000);
}

#[test]
fn shared_blob_survives_eviction_of_other_holder() {
    // quota holds two blobs; id 1 and id 2 hold A, id 3 holds B (full).
    let mut s = store(2000, 1 << 30);
    s.insert(M, 1, 0, img(1)).unwrap();
    s.insert(M, 3, 0, img(2)).unwrap();
    // id 1 is the only holder of A; inserting A under id 2 must keep A alive.
    s.insert(M, 2, 0, img(1)).unwrap();
    assert_eq!(s.get(M, 2).unwrap().rgba[0], 1);
    assert_eq!(s.get(M, 1).unwrap().rgba[0], 1);
    assert_eq!(s.used(), 2000);
}

#[test]
fn placements_are_capped_per_pane() {
    let mut s = store(1 << 20, 1 << 30);
    s.insert(M, 1, 0, img(1)).unwrap();
    for n in 1..=(MAX_PLACEMENTS as u32 + 100) {
        s.place(M, 1, n, spec(0)).unwrap();
    }
    assert_eq!(s.placements().len(), MAX_PLACEMENTS);
    assert_eq!(s.placements()[0].id, 101);
}
