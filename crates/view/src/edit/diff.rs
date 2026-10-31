//! Полоса изменений редактора (S7 §5.3): хэши строк сохранённого текста и построчный diff
//! с текущим в фоне. Строки делятся по LF, CRLF и одиночному CR; хэш строки — без
//! перевода строки и не зависит от того, как текст порезан на куски.

use std::hash::Hasher;
use std::sync::Arc;

use rustc_hash::FxHasher;

/// Изменение строки текущего текста.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Change {
    /// Новая строка (`type.exec` на полосе).
    Added,
    /// Строка изменена (`accent`).
    Modified,
    /// Перед этой строкой удалены строки сохранённого текста (номер может быть равен
    /// числу строк — удалено в конце).
    Deleted,
}

/// Итог diff: счётчик в шапке («+2 добавлено · 1 изменена») и метки полосы по строкам
/// текущего текста (с 0, по возрастанию).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LineChanges {
    pub added: u32,
    pub modified: u32,
    pub deleted: u32,
    pub lines: Arc<[(u32, Change)]>,
}

impl LineChanges {
    pub fn is_empty(&self) -> bool {
        self.added == 0 && self.modified == 0 && self.deleted == 0
    }
}

/// Концы строк в тексте.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EolCount {
    pub lf: u64,
    pub crlf: u64,
    pub cr: u64,
}

/// Проход по тексту кусками: хэши строк и счёт концов строк.
#[derive(Debug, Default)]
pub struct LineScan {
    /// Начало строки из прошлых кусков (хэш считается по целой строке).
    carry: Vec<u8>,
    /// Последним был CR: следующий LF — его пара.
    cr: bool,
    pub hashes: Vec<u64>,
    pub eol: EolCount,
}

fn hash(b: &[u8]) -> u64 {
    let mut h = FxHasher::default();
    h.write(b);
    h.write_usize(b.len());
    h.finish()
}

impl LineScan {
    pub fn feed(&mut self, s: &str) {
        let b = s.as_bytes();
        let mut at = 0;
        for i in memchr::memchr2_iter(b'\n', b'\r', b) {
            if i > at {
                self.lone_cr();
            }
            let seg = &b[at..i];
            at = i + 1;
            if b[i] == b'\n' && self.cr {
                // Вторая половина CRLF: строка уже закончилась на CR.
                self.cr = false;
                self.eol.crlf += 1;
                continue;
            }
            self.lone_cr();
            self.end(seg);
            if b[i] == b'\r' {
                self.cr = true;
            } else {
                self.eol.lf += 1;
            }
        }
        if at < b.len() {
            self.lone_cr();
            self.carry.extend_from_slice(&b[at..]);
        }
    }

    /// CR без LF за ним.
    fn lone_cr(&mut self) {
        if std::mem::take(&mut self.cr) {
            self.eol.cr += 1;
        }
    }

    fn end(&mut self, tail: &[u8]) {
        let h = if self.carry.is_empty() {
            hash(tail)
        } else {
            self.carry.extend_from_slice(tail);
            let h = hash(&self.carry);
            self.carry.clear();
            h
        };
        self.hashes.push(h);
    }

    /// Последняя строка (после последнего перевода — пустая).
    pub fn finish(mut self) -> (Vec<u64>, EolCount) {
        self.lone_cr();
        let carry = std::mem::take(&mut self.carry);
        self.hashes.push(hash(&carry));
        (self.hashes, self.eol)
    }
}

/// Хэши строк текста (Rope — по кускам).
pub fn line_hashes(text: &ropey::Rope) -> Vec<u64> {
    let mut s = LineScan::default();
    for c in text.chunks() {
        s.feed(c);
    }
    s.finish().0
}

/// Дальше столько правок — один общий блок вместо точного сценария (память Myers —
/// квадрат числа правок).
const MAX_D: usize = 1024;

/// Блок правок: строки `[a, a + del)` сохранённого текста стали строками `[b, b + ins)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Hunk {
    b: usize,
    del: usize,
    ins: usize,
}

/// Построчный diff `base` (сохранённый текст) → `now`. В блоке правок первые
/// `min(удалено, вставлено)` строк — изменены, остальные вставленные — добавлены,
/// лишние удалённые — метка `Deleted` после блока.
pub fn diff(base: &[u64], now: &[u64]) -> LineChanges {
    let pre = base.iter().zip(now).take_while(|(a, b)| a == b).count();
    let suf = base[pre..]
        .iter()
        .rev()
        .zip(now[pre..].iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    let a = &base[pre..base.len() - suf];
    let b = &now[pre..now.len() - suf];
    let hunks = myers(a, b, MAX_D).unwrap_or_else(|| {
        vec![Hunk {
            b: 0,
            del: a.len(),
            ins: b.len(),
        }]
    });
    let mut out = LineChanges::default();
    let mut lines = Vec::new();
    for h in hunks.into_iter().filter(|h| h.del + h.ins > 0) {
        let modified = h.del.min(h.ins);
        let at = pre + h.b;
        for i in 0..h.ins {
            let c = if i < modified {
                Change::Modified
            } else {
                Change::Added
            };
            lines.push((to_u32(at + i), c));
        }
        if h.del > h.ins {
            lines.push((to_u32(at + h.ins), Change::Deleted));
        }
        out.modified += to_u32(modified);
        out.added += to_u32(h.ins - modified);
        out.deleted += to_u32(h.del.saturating_sub(h.ins));
    }
    out.lines = lines.into();
    out
}

fn to_u32(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// Myers O((N+M)·D): блоки правок кратчайшего сценария; `None` — правок больше `max_d`.
#[allow(
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::many_single_char_names,
    reason = "индексы диагоналей Myers: длины строк ≪ isize::MAX, имена — как в статье"
)]
fn myers(a: &[u64], b: &[u64], max_d: usize) -> Option<Vec<Hunk>> {
    let (n, m) = (a.len() as isize, b.len() as isize);
    if n == 0 && m == 0 {
        return Some(Vec::new());
    }
    let lim = (a.len() + b.len()).min(max_d) as isize;
    let off = lim + 1;
    let mut v = vec![0isize; (2 * lim + 3) as usize];
    // trace[d] — V до шага d (диагонали -(d-1)..=d-1).
    let mut trace: Vec<Vec<isize>> = Vec::new();
    let at = |k: isize| (k + off) as usize;
    for d in 0..=lim {
        trace.push(if d == 0 {
            Vec::new()
        } else {
            v[at(-(d - 1))..=at(d - 1)].to_vec()
        });
        let mut k = -d;
        while k <= d {
            let mut x = if k == -d || (k != d && v[at(k - 1)] < v[at(k + 1)]) {
                v[at(k + 1)]
            } else {
                v[at(k - 1)] + 1
            };
            let mut y = x - k;
            while x < n && y < m && a[x as usize] == b[y as usize] {
                x += 1;
                y += 1;
            }
            v[at(k)] = x;
            if x >= n && y >= m {
                return Some(backtrack(&trace, n, m));
            }
            k += 2;
        }
    }
    None
}

/// Обратный проход по `trace`: шаги (равно / удаление / вставка) → блоки правок.
#[allow(
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::many_single_char_names,
    reason = "индексы диагоналей Myers, имена — как в статье"
)]
fn backtrack(trace: &[Vec<isize>], n: isize, m: isize) -> Vec<Hunk> {
    #[derive(Clone, Copy)]
    enum Op {
        Eq,
        Del,
        Ins,
    }
    let mut ops = Vec::new();
    let (mut x, mut y) = (n, m);
    for d in (0..trace.len() as isize).rev() {
        let v = &trace[d as usize];
        let get = |k: isize| v[(k + d - 1) as usize];
        let k = x - y;
        let (px, py) = if d == 0 {
            (0, 0)
        } else {
            let pk = if k == -d || (k != d && get(k - 1) < get(k + 1)) {
                k + 1
            } else {
                k - 1
            };
            let px = get(pk);
            (px, px - pk)
        };
        while x > px && y > py {
            ops.push(Op::Eq);
            x -= 1;
            y -= 1;
        }
        if d > 0 {
            ops.push(if x == px { Op::Ins } else { Op::Del });
        }
        (x, y) = (px, py);
    }
    ops.reverse();
    let mut hunks: Vec<Hunk> = Vec::new();
    let mut open = false;
    let mut yy = 0usize;
    for op in ops {
        match op {
            Op::Eq => {
                open = false;
                yy += 1;
            }
            Op::Del | Op::Ins => {
                if !open {
                    hunks.push(Hunk {
                        b: yy,
                        del: 0,
                        ins: 0,
                    });
                    open = true;
                }
                let h = hunks.last_mut().expect("блок открыт");
                if matches!(op, Op::Del) {
                    h.del += 1;
                } else {
                    h.ins += 1;
                    yy += 1;
                }
            }
        }
    }
    hunks
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn hs(lines: &[&str]) -> Vec<u64> {
        lines.iter().map(|l| hash(l.as_bytes())).collect()
    }

    #[test]
    fn scan_splits_lf_crlf_cr_across_chunks() {
        let text = "a\r\nb\nc\rd\r\r\ne";
        let whole = {
            let mut s = LineScan::default();
            s.feed(text);
            s.finish()
        };
        assert_eq!(whole.0, hs(&["a", "b", "c", "d", "", "e"]));
        assert_eq!(
            whole.1,
            EolCount {
                lf: 1,
                crlf: 2,
                cr: 2
            }
        );
        // Любой разрез на куски — те же хэши и счёт.
        for cut in 0..=text.len() {
            let mut s = LineScan::default();
            s.feed(&text[..cut]);
            s.feed(&text[cut..]);
            assert_eq!(s.finish(), whole, "разрез {cut}");
        }
        let mut s = LineScan::default();
        s.feed("x\r");
        assert_eq!(s.finish().1.cr, 1, "CR в конце текста — одиночный");
    }

    #[test]
    fn hunks_classify_lines() {
        let base = hs(&["a", "b", "c", "d", "e"]);
        let now = hs(&["a", "B", "c", "new", "d"]);
        let c = diff(&base, &now);
        assert_eq!((c.added, c.modified, c.deleted), (1, 1, 1));
        assert_eq!(
            &*c.lines,
            &[
                (1, Change::Modified),
                (3, Change::Added),
                (5, Change::Deleted)
            ]
        );
        assert!(diff(&base, &base).is_empty());
    }

    /// LCS динамикой: длина общей подпоследовательности.
    fn lcs(a: &[u64], b: &[u64]) -> usize {
        let mut dp = vec![vec![0usize; b.len() + 1]; a.len() + 1];
        for i in 0..a.len() {
            for j in 0..b.len() {
                dp[i + 1][j + 1] = if a[i] == b[j] {
                    dp[i][j] + 1
                } else {
                    dp[i][j + 1].max(dp[i + 1][j])
                };
            }
        }
        dp[a.len()][b.len()]
    }

    /// Правка в позиции базового текста.
    #[derive(Clone, Copy, Debug)]
    enum Edit {
        Insert,
        Modify,
        Delete,
    }

    proptest! {
        /// Против наивного: вставлено и удалено столько же, сколько у кратчайшего
        /// сценария по LCS.
        #[test]
        fn counts_match_naive_lcs(
            a in prop::collection::vec(0u64..6, 0..40),
            b in prop::collection::vec(0u64..6, 0..40),
        ) {
            let c = diff(&a, &b);
            let l = lcs(&a, &b);
            prop_assert_eq!((c.added + c.modified) as usize, b.len() - l);
            prop_assert_eq!((c.deleted + c.modified) as usize, a.len() - l);
            let marked = c.lines.iter().filter(|(_, x)| *x != Change::Deleted).count();
            prop_assert_eq!(marked, (c.added + c.modified) as usize);
        }

        /// Вставка, правка и удаление строк (разнесены хотя бы одной целой строкой) дают
        /// ровно свои `added` / `modified` / `deleted` и метки на своих строках.
        #[test]
        fn edits_give_exact_counts(
            n in 1usize..60,
            edits in prop::collection::btree_map(0usize..30, 0u8..3, 0..10),
        ) {
            let base: Vec<u64> = (0..n as u64).collect();
            let mut now = Vec::new();
            let mut want = (0u32, 0u32, 0u32);
            let mut marks = Vec::new();
            let mut fresh = 1_000_000u64;
            for (i, &line) in base.iter().enumerate() {
                // Правки — только на чётных позициях: между ними целая строка.
                let e = edits.get(&(i / 2)).filter(|_| i % 2 == 0).map(|k| match k {
                    0 => Edit::Insert,
                    1 => Edit::Modify,
                    _ => Edit::Delete,
                });
                match e {
                    Some(Edit::Insert) => {
                        marks.push((now.len() as u32, Change::Added));
                        now.push(fresh);
                        fresh += 1;
                        now.push(line);
                        want.0 += 1;
                    }
                    Some(Edit::Modify) => {
                        marks.push((now.len() as u32, Change::Modified));
                        now.push(fresh);
                        fresh += 1;
                        want.1 += 1;
                    }
                    Some(Edit::Delete) => {
                        marks.push((now.len() as u32, Change::Deleted));
                        want.2 += 1;
                    }
                    None => now.push(line),
                }
            }
            let c = diff(&base, &now);
            prop_assert_eq!((c.added, c.modified, c.deleted), want);
            prop_assert_eq!(&*c.lines, &marks[..]);
        }
    }

    #[test]
    fn many_edits_fall_back_to_one_block() {
        let a: Vec<u64> = (0..3000).collect();
        let b: Vec<u64> = (10_000..13_000).collect();
        let c = diff(&a, &b);
        assert_eq!((c.added, c.modified, c.deleted), (0, 3000, 0));
    }
}
