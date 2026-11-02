//! Раскладка карты (F01-07): плитки не пересекаются, покрывают прямоугольник, площадь
//! ∝ размеру; квадратность; время раскладки 5000 элементов.
#![allow(clippy::cast_precision_loss, reason = "размеры в тестах — до 10⁹")]

use compact_str::CompactString;
use proptest::prelude::*;
use urfin_vfs::du::{DuType, MapItem, MapKind};

use super::{Cache, Key, MIN_AREA, Rect, TileKind, hit, layout, squarify};

/// Площадь пересечения; соседние плитки делят край — погрешность f32 срезается.
fn overlap(a: &Rect, b: &Rect) -> f32 {
    const E: f32 = 0.01;
    let w = (a.x + a.w - E).min(b.x + b.w - E) - (a.x + E).max(b.x + E);
    let h = (a.y + a.h - E).min(b.y + b.h - E) - (a.y + E).max(b.y + E);
    if w > 0.0 && h > 0.0 { w * h } else { 0.0 }
}

/// Псевдослучайные размеры по убыванию (LCG — без зависимостей, воспроизводимо).
fn random_sizes(seed: u64, n: usize) -> Vec<u64> {
    let mut x = seed;
    let mut v: Vec<u64> = (0..n)
        .map(|_| {
            x = x
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            // Тяжёлый хвост, как у размеров файлов.
            1 + (x >> 33) % 1_000 * ((x >> 20) % 50 + 1)
        })
        .collect();
    v.sort_unstable_by(|a, b| b.cmp(a));
    v
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(200))]

    /// Плитки не пересекаются, лежат в прямоугольнике и покрывают его; площадь
    /// плитки ∝ размеру (±1 px²).
    #[test]
    fn tiles_cover_without_overlap(
        mut sizes in prop::collection::vec(1u64..1_000_000_000, 1..120),
        w in 10f32..2000.0,
        h in 10f32..2000.0,
    ) {
        sizes.sort_unstable_by(|a, b| b.cmp(a));
        let rect = Rect::new(3.0, 5.0, w, h);
        let mut out = Vec::new();
        squarify(&sizes, rect, &mut out);
        prop_assert_eq!(out.len(), sizes.len());
        let total: f64 = sizes.iter().map(|&s| s as f64).sum();
        let mut sum = 0f64;
        for (r, &s) in out.iter().zip(&sizes) {
            let eps = 0.01;
            prop_assert!(r.x >= rect.x - eps && r.y >= rect.y - eps, "{r:?}");
            prop_assert!(r.x + r.w <= rect.x + rect.w + eps, "{r:?}");
            prop_assert!(r.y + r.h <= rect.y + rect.h + eps, "{r:?}");
            let want = s as f64 / total * f64::from(w) * f64::from(h);
            prop_assert!((f64::from(r.area()) - want).abs() <= 1.0, "{r:?} want {want}");
            sum += f64::from(r.area());
        }
        prop_assert!((sum - f64::from(w) * f64::from(h)).abs() <= 1.0);
        for (i, a) in out.iter().enumerate() {
            for b in &out[i + 1..] {
                prop_assert!(overlap(a, b) <= 0.01, "{a:?} {b:?}");
            }
        }
    }
}

/// Средний aspect плиток на случайных данных ≤ 2.
#[test]
fn average_aspect_at_most_two() {
    let mut sum = 0f64;
    let mut n = 0u32;
    for seed in 1..40u64 {
        let count = 20 + (seed as usize * 37) % 900;
        let sizes = random_sizes(seed, count);
        let mut out = Vec::new();
        squarify(&sizes, Rect::new(0.0, 0.0, 1200.0, 800.0), &mut out);
        for r in out.iter().filter(|r| r.w > 0.0 && r.h > 0.0) {
            sum += f64::from((r.w / r.h).max(r.h / r.w));
            n += 1;
        }
    }
    let avg = sum / f64::from(n);
    assert!(avg <= 2.0, "average aspect {avg}");
}

/// Раскладка 5000 элементов ≤ 1 мс (release; в отладочной сборке — только проверка, что
/// считается).
#[test]
fn layout_5000_within_budget() {
    let sizes = random_sizes(7, 5000);
    let mut out = Vec::with_capacity(5000);
    let mut best = std::time::Duration::MAX;
    for _ in 0..20 {
        out.clear();
        let t0 = std::time::Instant::now();
        squarify(&sizes, Rect::new(0.0, 0.0, 640.0, 560.0), &mut out);
        best = best.min(t0.elapsed());
    }
    assert_eq!(out.len(), 5000);
    if !cfg!(debug_assertions) {
        assert!(best <= std::time::Duration::from_millis(1), "{best:?}");
    }
}

#[test]
fn zero_sizes_and_empty_rect() {
    let mut out = Vec::new();
    squarify(&[0, 0], Rect::new(0.0, 0.0, 100.0, 100.0), &mut out);
    assert!(out.iter().all(|r| r.area() == 0.0));
    out.clear();
    squarify(&[5, 3], Rect::new(0.0, 0.0, 0.0, 100.0), &mut out);
    assert_eq!(out.len(), 2);
    out.clear();
    squarify(&[], Rect::new(0.0, 0.0, 10.0, 10.0), &mut out);
    assert_eq!(out.len(), 0);
}

fn item(name: &str, size: u64, kind: MapKind, kids: Vec<MapItem>) -> MapItem {
    MapItem {
        name: CompactString::from(name),
        size,
        kind,
        temp: false,
        kids,
    }
}

/// Два уровня: подкаталог с полосой подписи и детьми; мелочь — «ещё N».
#[test]
fn two_levels_and_rest() {
    let mut kids = vec![
        item("release", 600_000, MapKind::Dir, Vec::new()),
        item("debug", 300_000, MapKind::Dir, Vec::new()),
    ];
    kids.extend((0..50).map(|i| item(&format!("k{i}"), 1, MapKind::Dir, Vec::new())));
    let mut items = vec![
        item("target", 1_000_000, MapKind::Dir, kids),
        item(
            "Win11.iso",
            400_000,
            MapKind::File(DuType::DiskImage),
            Vec::new(),
        ),
        item("", 100_000, MapKind::Small { files: 9 }, Vec::new()),
    ];
    items.extend((0..300).map(|i| item(&format!("t{i}"), 1, MapKind::Dir, Vec::new())));
    let tiles = layout(&items, Rect::new(0.0, 0.0, 600.0, 400.0));
    let target = tiles.iter().find(|t| t.name == "target").unwrap();
    assert!(target.head);
    assert_eq!(target.depth, 1);
    let rel = tiles.iter().find(|t| t.name == "release").unwrap();
    assert_eq!(rel.depth, 2);
    assert_eq!(rel.path, r"target\release");
    assert!(rel.rect.y >= target.rect.y + super::HEAD_H - 0.01);
    // Мелочь первого уровня и детей — по «прочему».
    let rests: Vec<_> = tiles
        .iter()
        .filter(|t| matches!(t.kind, TileKind::Rest { .. }))
        .collect();
    assert_eq!(rests.len(), 2, "{rests:?}");
    assert!(
        rests
            .iter()
            .any(|t| t.depth == 1 && t.kind == TileKind::Rest { count: 300 })
    );
    assert!(rests.iter().any(|t| t.depth == 2 && t.path == "target"));
    for t in tiles
        .iter()
        .filter(|t| !matches!(t.kind, TileKind::Rest { .. }))
    {
        assert!(t.rect.area() >= MIN_AREA, "{t:?}");
    }
    // Точка в «release» — его плитка (глубже каталога).
    let r = rel.rect;
    let k = hit(&tiles, r.x + r.w / 2.0, r.y + r.h / 2.0).unwrap();
    assert_eq!(tiles[k].name, "release");
    assert!(hit(&tiles, -5.0, 0.0).is_none());
}

/// Кэш раскладок: тот же ключ — без пересчёта; не больше 8.
#[test]
fn layout_cache_lru() {
    let mut c = Cache::default();
    let mut made = 0;
    for i in 0..10u64 {
        c.get_or(Key::new(i, 0, 100.0, 100.0, 1.0), || {
            made += 1;
            Vec::new()
        });
    }
    assert_eq!((made, c.len()), (10, 8));
    c.get_or(Key::new(9, 0, 100.0, 100.0, 1.0), || {
        made += 1;
        Vec::new()
    });
    assert_eq!(made, 10, "hit");
    c.get_or(Key::new(0, 0, 100.0, 100.0, 1.0), || {
        made += 1;
        Vec::new()
    });
    assert_eq!(made, 11, "evicted");
    c.get_or(Key::new(9, 1, 100.0, 100.0, 1.0), || {
        made += 1;
        Vec::new()
    });
    assert_eq!(made, 12, "snapshot changed");
}
