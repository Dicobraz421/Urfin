use std::time::{Duration, Instant};

use urfin_core::{Attrs, Entry, EntryKind};

use super::*;
use crate::api::{WatchEvent, WatchId};

const DEB: Duration = Duration::from_millis(50);
const MAX: Duration = Duration::from_millis(250);
const W: WatchId = WatchId(1);

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

fn e(name: &str) -> Entry {
    Entry {
        ctime: i64::MIN,
        atime: i64::MIN,
        name: name.into(),
        size: 1,
        mtime: 0,
        attrs: Attrs(0),
        kind: EntryKind::File,
    }
}

fn added(n: &str) -> WatchEvent {
    WatchEvent::Added(n.into(), Some(e(n)))
}

fn deb() -> Debouncer {
    Debouncer::new(DEB, MAX)
}

/// `Names` в виде отсортированных строк `+a`, `~b`, `-c`, `?d`.
fn summary(c: &DirChange) -> Vec<String> {
    let DirChange::Names {
        added,
        changed,
        removed,
        unresolved,
    } = c
    else {
        return vec![format!("{c:?}")];
    };
    let mut v: Vec<String> = added
        .iter()
        .map(|e| format!("+{}", e.name))
        .chain(changed.iter().map(|e| format!("~{}", e.name)))
        .chain(removed.iter().map(|n| format!("-{n}")))
        .chain(unresolved.iter().map(|n| format!("?{n}")))
        .collect();
    v.sort();
    v
}

#[test]
fn flush_after_quiet_debounce() {
    let t0 = Instant::now();
    let mut d = deb();
    d.push(W, added("a"), t0);
    assert_eq!(d.deadline(), Some(t0 + DEB));
    d.push(W, added("b"), t0 + ms(30));
    assert_eq!(
        d.deadline(),
        Some(t0 + ms(80)),
        "debounce restarts on each event"
    );
    assert_eq!(d.take_due(t0 + ms(79)), Vec::new());
    let out = d.take_due(t0 + ms(80));
    assert_eq!(out.len(), 1);
    assert_eq!(summary(&out[0].1), ["+a", "+b"]);
    assert_eq!(d.deadline(), None);
}

#[test]
fn max_latency_caps_a_steady_stream() {
    let t0 = Instant::now();
    let mut d = deb();
    for i in 0..10u64 {
        d.push(W, added(&format!("f{i}")), t0 + ms(i * 30));
    }
    // Последнее событие в 270 мс, но первое — в 0: срок 250.
    assert_eq!(d.deadline(), Some(t0 + MAX));
}

#[test]
fn coalescing_by_name() {
    let t0 = Instant::now();
    let mut d = deb();
    // Появился и исчез — ничего.
    d.push(W, added("tmp"), t0);
    d.push(W, WatchEvent::Removed("tmp".into()), t0);
    // Исчез и появился — изменён.
    d.push(W, WatchEvent::Removed("swap".into()), t0);
    d.push(W, added("swap"), t0);
    // Новый и изменён — новый.
    d.push(W, added("new"), t0);
    d.push(W, WatchEvent::Modified("new".into(), None), t0);
    // Переименование — удаление старого и новое имя.
    d.push(
        W,
        WatchEvent::Renamed {
            from: "old".into(),
            to: "renamed".into(),
            entry: Some(e("renamed")),
        },
        t0,
    );
    // Без Entry (обычный ReadDirectoryChangesW) — неразрешённое имя.
    d.push(W, WatchEvent::Modified("bare".into(), None), t0);
    let out = d.take_due(t0 + DEB);
    assert_eq!(
        summary(&out[0].1),
        ["+new", "+renamed", "-old", "?bare", "~swap"]
    );
}

#[test]
fn only_cancelled_names_flush_nothing() {
    let t0 = Instant::now();
    let mut d = deb();
    d.push(W, added("tmp"), t0);
    d.push(W, WatchEvent::Removed("tmp".into()), t0);
    assert_eq!(d.take_due(t0 + DEB), Vec::new());
    assert_eq!(d.deadline(), None);
}

#[test]
fn overflow_and_too_many_names_rescan() {
    let t0 = Instant::now();
    let mut d = deb();
    d.push(W, added("a"), t0);
    d.push(W, WatchEvent::Overflow, t0);
    d.push(W, added("b"), t0);
    assert_eq!(d.take_due(t0 + DEB), [(W, DirChange::Rescan)]);

    for i in 0..=MAX_NAMES {
        d.push(W, added(&format!("f{i}")), t0 + DEB);
    }
    assert_eq!(d.take_due(t0 + DEB * 2), [(W, DirChange::Rescan)]);
}

#[test]
fn gone_wins() {
    let t0 = Instant::now();
    let mut d = deb();
    d.push(W, WatchEvent::Overflow, t0);
    d.push(W, WatchEvent::Gone, t0);
    d.push(W, added("late"), t0);
    assert_eq!(d.take_due(t0 + DEB), [(W, DirChange::Gone)]);
}

#[test]
fn held_watch_accumulates_until_released() {
    let t0 = Instant::now();
    let mut d = deb();
    d.hold(W, true);
    d.push(W, added("during-listing"), t0);
    assert_eq!(d.deadline(), None);
    assert_eq!(d.take_due(t0 + ms(1000)), Vec::new());
    d.hold(W, false);
    // Срок давно прошёл — сброс сразу.
    let out = d.take_due(t0 + ms(1000));
    assert_eq!(summary(&out[0].1), ["+during-listing"]);
}

/// Шторм (10k событий/с): после трёх flush'ей по `max_latency` — только `Rescan`
/// и не чаще раза в секунду; после 2 с тишины — обычный режим.
#[test]
fn storm_mode_limits_to_one_rescan_per_second() {
    let t0 = Instant::now();
    let mut d = deb();
    let mut flushes = Vec::new();
    let mut i = 0u64;
    // 5 с шторма: событие каждые 0,1 мс; flush'и проверяем каждую мс.
    let end = ms(5000);
    let mut now = Duration::ZERO;
    while now < end {
        for _ in 0..10 {
            d.push(W, added(&format!("f{i}")), t0 + now);
            i += 1;
        }
        for (_, c) in d.take_due(t0 + now) {
            flushes.push((now, c));
        }
        now += ms(1);
    }
    let rescans: Vec<_> = flushes
        .iter()
        .filter(|(_, c)| *c == DirChange::Rescan)
        .map(|(t, _)| *t)
        .collect();
    // Первые flush'и — по max_latency (250, 500, 750), затем шторм.
    assert!(flushes.len() <= 3 + 5, "{} flushes", flushes.len());
    for w in rescans.windows(2) {
        assert!(
            w[1].saturating_sub(w[0]) >= ms(1000),
            "rescans too close: {rescans:?}"
        );
    }
    // Последняя секунда шторма — не больше одного flush'а.
    let tail = flushes
        .iter()
        .filter(|(t, _)| *t >= end.saturating_sub(ms(1000)))
        .count();
    assert!(tail <= 1, "{tail} flushes in the last second");

    // Остаток шторма сбрасывается, затем 2 с тишины — снова обычный режим.
    let quiet = end + ms(1000);
    let _ = d.take_due(t0 + quiet);
    let after = quiet + ms(2000);
    d.push(W, added("calm"), t0 + after);
    let out = d.take_due(t0 + after + DEB);
    assert_eq!(summary(&out[0].1), ["+calm"]);
}

#[test]
fn watches_are_independent() {
    let t0 = Instant::now();
    let mut d = deb();
    d.push(WatchId(1), added("a"), t0);
    d.push(WatchId(2), added("b"), t0 + ms(40));
    let first = d.take_due(t0 + DEB);
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].0, WatchId(1));
    let second = d.take_due(t0 + ms(90));
    assert_eq!(second[0].0, WatchId(2));
    d.remove(WatchId(2));
    d.push(WatchId(2), added("c"), t0 + ms(100));
    d.remove(WatchId(2));
    assert_eq!(d.deadline(), None);
}

/// Рваный поток (пишущий тормозит, flush'и по тишине), но по многу имён — шторм
/// со второго тяжёлого flush'а (по темпу: 80 имён за 120 мс); одиночные файлы раз в
/// 120 мс — нет.
#[test]
fn heavy_bursts_are_a_storm_single_files_are_not() {
    let t0 = Instant::now();
    let mut d = deb();
    let mut i = 0u64;
    let mut kinds = Vec::new();
    for burst in 0..5u64 {
        let at = ms(burst * 120);
        for _ in 0..40 {
            d.push(W, added(&format!("f{i}")), t0 + at);
            i += 1;
        }
        kinds.extend(
            d.take_due(t0 + at + DEB)
                .into_iter()
                .map(|(_, c)| c == DirChange::Rescan),
        );
    }
    assert_eq!(kinds, [false, true], "storm on the second heavy flush");

    let mut d = deb();
    let mut flushes = 0;
    for k in 0..20u64 {
        let at = ms(k * 120);
        d.push(W, added(&format!("one{k}")), t0 + at);
        for (_, c) in d.take_due(t0 + at + DEB) {
            assert_ne!(c, DirChange::Rescan, "single files never storm");
            flushes += 1;
        }
    }
    assert_eq!(flushes, 20);
}

/// Медленный пишущий (~200 файлов/с, раннер CI под нагрузкой): порции мелкие, flush'и
/// по тишине — шторм по темпу (`STORM_RATE` за секунду), не больше ~1 перечитывания
/// в секунду после него.
#[test]
fn slow_steady_writer_is_a_storm() {
    let t0 = Instant::now();
    let mut d = deb();
    let mut flushes = 0;
    let mut rescans = 0;
    for k in 0..600u64 {
        let at = t0 + ms(k * 5);
        d.push(W, added(&format!("s{k}")), at);
        // Пишущий запинается: каждые 20 файлов — пауза дольше debounce.
        let now = if k % 20 == 19 { at + DEB } else { at };
        for (_, c) in d.take_due(now) {
            flushes += 1;
            rescans += usize::from(c == DirChange::Rescan);
        }
    }
    assert!(rescans > 0, "slow storm detected");
    assert!(flushes <= 6, "{flushes} flushes in 3 s");
}

/// Пауза пишущего (один flush по тишине) не сбрасывает подсчёт шторма. Поток
/// «лёгкий» (< `STORM_NAMES` за окно): проверяется именно счёт по `max_latency`.
#[test]
fn storm_survives_a_short_pause() {
    let t0 = Instant::now();
    let mut d = deb();
    let mut now = Duration::ZERO;
    let mut i = 0u64;
    let mut stream = |d: &mut Debouncer, from: Duration, to: Duration| {
        let mut t = from;
        while t < to {
            d.push(W, added(&format!("f{i}")), t0 + t);
            i += 1;
            // 50 имён/с — ниже `STORM_RATE`: шторм только по счёту `max_latency`.
            t += ms(20);
        }
    };
    let mut kinds = Vec::new();
    // Поток → flush по max_latency, дважды.
    for _ in 0..2 {
        stream(&mut d, now, now + MAX);
        now += MAX;
        kinds.extend(
            d.take_due(t0 + now)
                .into_iter()
                .map(|(_, c)| c == DirChange::Rescan),
        );
    }
    // Короткий всплеск и тишина → flush по debounce.
    stream(&mut d, now, now + ms(10));
    now += ms(10) + DEB;
    kinds.extend(
        d.take_due(t0 + now)
            .into_iter()
            .map(|(_, c)| c == DirChange::Rescan),
    );
    // Снова поток: счёт 1 → 2 → 3 — шторм.
    for _ in 0..2 {
        stream(&mut d, now, now + MAX);
        now += MAX;
        kinds.extend(
            d.take_due(t0 + now)
                .into_iter()
                .map(|(_, c)| c == DirChange::Rescan),
        );
    }
    assert_eq!(
        kinds,
        [false, false, false, false, true],
        "storm after the pause"
    );
}
