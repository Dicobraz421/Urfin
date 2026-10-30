#![allow(
    clippy::many_single_char_names,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::type_complexity,
    reason = "тесты: короткие имена, доли длины, таблицы случаев"
)]

use std::sync::Arc;

use urfin_core::CancelToken;

use super::*;
use crate::source::OwnedSource;

fn data(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i % 251) as u8).collect()
}

fn cache(len: usize, budget: usize) -> (PageCache, Loader, Vec<u8>) {
    let d = data(len);
    let src: Arc<dyn ByteSource> = Arc::new(OwnedSource::new(d.clone()));
    (
        PageCache::with_source(budget, src.clone()),
        Loader::new(src),
        d,
    )
}

/// Выполняет все запросы кэша синхронно.
fn drain(c: &mut PageCache, l: &Loader) {
    while let Some(req) = c.take_request() {
        let loaded = l.load(&req, &CancelToken::new());
        c.insert(loaded);
    }
}

#[test]
fn get_misses_then_hits_across_pages() {
    let (mut c, l, d) = cache(300_000, 8 << 20);
    let r = PAGE - 10..PAGE + 30;
    assert_eq!(
        c.get(r.clone()).unwrap_err(),
        Missing { pages: 0..2 },
        "пустой кэш"
    );
    c.want(r.clone(), Prio::Visible);
    drain(&mut c, &l);
    let b = c.get(r.clone()).unwrap();
    assert_eq!(b.head.len(), 10);
    assert_eq!(b.to_vec(), &d[r.start as usize..r.end as usize]);
    // Обрезка по длине документа.
    assert_eq!(
        c.get(299_990..400_000).map(|b| b.len()),
        Err(Missing { pages: 4..5 })
    );
    c.want(299_000..300_000, Prio::Visible);
    drain(&mut c, &l);
    assert_eq!(c.get(299_990..400_000).unwrap().to_vec(), &d[299_990..]);
}

#[test]
fn want_prefetches_one_screen_around() {
    let (mut c, _, _) = cache(40 * PAGE as usize, 8 << 20);
    c.want(10 * PAGE..11 * PAGE, Prio::Visible);
    let req = c.take_request().unwrap();
    assert_eq!(&req.pages[..], &[10, 11, 9], "видимая, ниже, выше");
    assert_eq!(c.inflight(), 1);
    c.want(10 * PAGE..11 * PAGE, Prio::Visible);
    assert!(c.take_request().is_none(), "всё уже в полёте");
}

/// Приёмка S7-03: 1000 прыжков подряд — не больше 2 запросов в полёте; последний
/// запрос побеждает.
#[test]
fn thousand_jumps_keep_two_in_flight() {
    let (mut c, l, _) = cache(4000 * PAGE as usize, 8 << 20);
    let mut flying = Vec::new();
    let mut x = 12345u64;
    for i in 0..1000 {
        x = x.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
        let at = (x >> 11) % (3990 * PAGE);
        c.want(at..at + 4000, Prio::Visible);
        while let Some(req) = c.take_request() {
            flying.push(req);
        }
        assert!(c.inflight() <= MAX_INFLIGHT, "прыжок {i}");
        assert!(flying.len() <= MAX_INFLIGHT);
        // Loader отвечает на каждый пятый кадр.
        if i % 5 == 0 && !flying.is_empty() {
            let req = flying.remove(0);
            c.insert(l.load(&req, &CancelToken::new()));
        }
    }
    let last = 3_000 * PAGE;
    c.want(last..last + 100, Prio::Visible);
    for req in flying.drain(..) {
        c.insert(l.load(&req, &CancelToken::new()));
    }
    drain(&mut c, &l);
    assert!(c.get(last..last + 100).is_ok(), "последний прыжок загружен");
}

/// Приёмка S7-03: бюджет памяти не превышается.
#[test]
fn budget_is_respected_lru() {
    let (mut c, l, _) = cache(400 * PAGE as usize, 1 << 20);
    assert_eq!(c.cap(), 16);
    for i in 0..100u64 {
        let at = i * 3 * PAGE;
        c.want(at..at + 100, Prio::Visible);
        drain(&mut c, &l);
        assert!(c.resident() <= 1 << 20, "шаг {i}: {}", c.resident());
        assert!(c.get(at..at + 100).is_ok(), "последний видимый — в кэше");
    }
    // Недавно использованная страница переживает вытеснение.
    let hot = 297 * PAGE;
    for i in 0..10u64 {
        assert!(c.get(hot..hot + 10).is_ok());
        let at = (i * 7 + 1) * PAGE;
        c.want(at..at + 10, Prio::Visible);
        drain(&mut c, &l);
    }
    assert!(c.get(hot..hot + 10).is_ok());
}

#[test]
fn growing_source_rereads_partial_tail() {
    let (mut c, l, _) = cache(100_000, 8 << 20);
    c.set_len(70_000, false);
    c.want(60_000..70_000, Prio::Visible);
    drain(&mut c, &l);
    assert!(c.get(65_536..70_000).is_ok());
    c.set_len(100_000, true);
    assert!(
        c.get(65_536..66_000).is_err(),
        "неполная страница перечитывается"
    );
    c.want(65_536..100_000, Prio::Visible);
    drain(&mut c, &l);
    assert_eq!(c.get(99_000..100_000).unwrap().len(), 1000);
}

#[test]
fn byte_view_chunks() {
    let (mut c, l, d) = cache(200_000, 8 << 20);
    c.want(0..200_000, Prio::Visible);
    drain(&mut c, &l);
    drain(&mut c, &l);
    c.want(130_000..200_000, Prio::Visible);
    drain(&mut c, &l);
    assert_eq!(c.chunk(70_000).unwrap(), &d[70_000..2 * PAGE as usize]);
    assert_eq!(c.chunk(196_607).unwrap(), &d[196_607..196_608]);
    assert_eq!(c.chunk(200_000).unwrap(), &[] as &[u8]);
    assert_eq!(c.chunk_before(70_000).unwrap(), &d[PAGE as usize..70_000]);
    assert_eq!(c.chunk_before(PAGE).unwrap(), &d[..PAGE as usize]);
}

#[test]
fn mapped_pages_are_zero_copy() {
    use std::io::Write;
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("m.bin");
    let d = data(150_000);
    std::fs::File::create(&path).unwrap().write_all(&d).unwrap();
    let f = std::fs::File::open(&path).unwrap();
    let src: Arc<dyn ByteSource> =
        Arc::new(crate::source::MappedSource::new(&f, path.clone()).unwrap());
    let mut c = PageCache::with_source(8 << 20, src.clone());
    let l = Loader::new(src);
    c.want(0..150_000, Prio::Visible);
    let req = c.take_request().unwrap();
    let loaded = l.load(&req, &CancelToken::new());
    assert!(
        loaded
            .pages
            .iter()
            .all(|(_, p)| matches!(p, Page::Mapped { .. }))
    );
    c.insert(loaded);
    assert_eq!(c.get(140_000..150_000).unwrap().to_vec(), &d[140_000..]);
}
