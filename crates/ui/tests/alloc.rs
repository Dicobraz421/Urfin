//! `cell_text` без кучи (S2-08): считающий аллокатор на потоке теста.
//! До `count-alloc` (S9) — свой тестовый `GlobalAlloc` (S0 v5 №11).

#![allow(unsafe_code, reason = "тестовый GlobalAlloc — обёртка над System")]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell as StdCell;

use urfin_core::{Attrs, Entry, EntryKind};
use urfin_ui::fmt_cell::{Cell, ColKind, FmtCfg, cell_text};
use urfin_ui::sys::Zone;

struct Counting;

thread_local! {
    /// Считать на этом потоке; `const` — без ленивой инициализации (и аллокаций).
    static ON: StdCell<bool> = const { StdCell::new(false) };
    static COUNT: StdCell<usize> = const { StdCell::new(0) };
}

// SAFETY: делегирует System; счётчик — потоковый `Cell` без аллокаций.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if ON.with(StdCell::get) {
            COUNT.with(|c| c.set(c.get() + 1));
        }
        // SAFETY: тот же контракт, что у вызывающего.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: `ptr` выделен `System` с тем же `layout`.
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new: usize) -> *mut u8 {
        if ON.with(StdCell::get) {
            COUNT.with(|c| c.set(c.get() + 1));
        }
        // SAFETY: тот же контракт, что у вызывающего.
        unsafe { System.realloc(ptr, layout, new) }
    }
}

#[global_allocator]
static A: Counting = Counting;

fn allocations(f: impl FnOnce()) -> usize {
    COUNT.with(|c| c.set(0));
    ON.with(|o| o.set(true));
    f();
    ON.with(|o| o.set(false));
    COUNT.with(StdCell::get)
}

#[test]
fn cell_text_does_not_allocate() {
    let kinds = [EntryKind::File, EntryKind::Dir, EntryKind::LinkBroken];
    let names = [
        "Cargo.lock",
        ".gitignore",
        "setup.exe",
        "backup.7z",
        "отчёт_по_производству_за_квартал.docx",
        "noext",
        "a.tar.gz",
    ];
    let entries: Vec<Entry> = (0..700u32)
        .map(|i| Entry {
            ctime: i64::MIN,
            atime: i64::MIN,
            name: names[i as usize % names.len()].into(),
            size: u64::from(i) * 7919 * 1024,
            mtime: if i % 50 == 0 {
                i64::MIN
            } else {
                1_791_969_120_000_000_000 + i64::from(i) * 60_000_000_000
            },
            attrs: Attrs(if i % 3 == 0 {
                Attrs::HIDDEN | Attrs::ARCHIVE | Attrs::SIZE_KNOWN
            } else {
                Attrs::ARCHIVE
            }),
            kind: kinds[i as usize % kinds.len()],
        })
        .collect();
    for tag in ["ru", "en"] {
        let i18n = urfin_i18n::Catalog::embedded()
            .build(&tag.parse().unwrap())
            .0;
        // Системная зона: как в окне.
        let cfg = FmtCfg::new(&i18n, false, Zone::system());
        let mut out = Cell::new();
        // Прогрев вне замера: разовая ленивая инициализация — таблица расширений
        // `icon_class` (vfs) и (Linux/macOS) пояс jiff.
        cell_text(ColKind::Type, &entries[0], &cfg, &mut out);
        cell_text(ColKind::Mtime, &entries[1], &cfg, &mut out);
        let n = allocations(|| {
            for e in &entries {
                for col in [
                    ColKind::Name,
                    ColKind::Ext,
                    ColKind::Type,
                    ColKind::Size,
                    ColKind::Mtime,
                    ColKind::Attrs,
                ] {
                    cell_text(col, e, &cfg, &mut out);
                    std::hint::black_box(out.as_str());
                }
            }
        });
        assert_eq!(n, 0, "{tag}: аллокаций в cell_text");
    }
    // Счётчик работает.
    assert!(allocations(|| drop(std::hint::black_box(vec![1u8; 10]))) > 0);
}
