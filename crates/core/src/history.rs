//! История навигации таба и глобальный MRU каталогов.

use std::collections::VecDeque;

use compact_str::CompactString;

use crate::loc::Location;

/// Точка истории: каталог, имя под курсором и прокрутка.
#[derive(Clone, Debug, PartialEq)]
pub struct HistEntry {
    pub loc: Location,
    pub focus: Option<CompactString>,
    pub top: u32,
}

/// История таба «назад/вперёд», каждая сторона не длиннее `cap`.
#[derive(Clone, Debug)]
pub struct NavHistory {
    back: Vec<HistEntry>,
    fwd: Vec<HistEntry>,
    cap: usize,
}

impl Default for NavHistory {
    fn default() -> Self {
        Self::new(NavHistory::DEFAULT_CAP)
    }
}

impl NavHistory {
    pub const DEFAULT_CAP: usize = 100;

    pub fn new(cap: usize) -> Self {
        NavHistory {
            back: Vec::new(),
            fwd: Vec::new(),
            cap: cap.max(1),
        }
    }

    /// Уход из `h` в новый каталог: `h` в «назад», «вперёд» очищается.
    pub fn push(&mut self, h: HistEntry) {
        self.fwd.clear();
        push_capped(&mut self.back, h, self.cap);
    }

    /// Шаг назад из текущей точки `cur`; `cur` уходит в «вперёд».
    pub fn back(&mut self, cur: HistEntry) -> Option<HistEntry> {
        let to = self.back.pop()?;
        push_capped(&mut self.fwd, cur, self.cap);
        Some(to)
    }

    /// Шаг вперёд из `cur`; `cur` уходит в «назад».
    pub fn forward(&mut self, cur: HistEntry) -> Option<HistEntry> {
        let to = self.fwd.pop()?;
        push_capped(&mut self.back, cur, self.cap);
        Some(to)
    }

    /// Шаги «назад», старые первыми.
    pub fn back_entries(&self) -> &[HistEntry] {
        &self.back
    }

    pub fn can_back(&self) -> bool {
        !self.back.is_empty()
    }

    pub fn can_forward(&self) -> bool {
        !self.fwd.is_empty()
    }
}

fn push_capped(v: &mut Vec<HistEntry>, h: HistEntry, cap: usize) {
    if v.len() >= cap {
        v.drain(..=v.len() - cap);
    }
    v.push(h);
}

/// Глобальный MRU каталогов (Alt+F12): без повторов, свежие первыми, не длиннее `cap`.
/// У записи — время посещения (секунды Unix; 0 — неизвестно, из старой сессии).
#[derive(Clone, Debug)]
pub struct VisitedDirs {
    items: VecDeque<(Location, u64)>,
    cap: usize,
}

impl Default for VisitedDirs {
    fn default() -> Self {
        Self::new(VisitedDirs::DEFAULT_CAP)
    }
}

/// Сейчас, секунды Unix.
fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

impl VisitedDirs {
    pub const DEFAULT_CAP: usize = 200;

    pub fn new(cap: usize) -> Self {
        VisitedDirs {
            items: VecDeque::new(),
            cap: cap.max(1),
        }
    }

    /// Новая ёмкость (из конфига); лишние старые записи отбрасываются.
    pub fn set_cap(&mut self, cap: usize) {
        self.cap = cap.max(1);
        self.items.truncate(self.cap);
    }

    /// Посещение сейчас.
    pub fn touch(&mut self, l: &Location) {
        self.touch_at(l, now_secs());
    }

    /// Посещение во время `ts` (секунды Unix; восстановление сессии).
    pub fn touch_at(&mut self, l: &Location, ts: u64) {
        self.remove(l);
        self.items.push_front((l.clone(), ts));
        self.items.truncate(self.cap);
    }

    /// Убрать каталог из истории; `false` — его там не было.
    pub fn remove(&mut self, l: &Location) -> bool {
        let at = self.items.iter().position(|x| x.0 == *l);
        at.and_then(|i| self.items.remove(i)).is_some()
    }

    /// От свежих к старым.
    pub fn iter(&self) -> impl Iterator<Item = &Location> {
        self.items.iter().map(|x| &x.0)
    }

    /// От свежих к старым, со временем посещения (0 — неизвестно).
    pub fn iter_at(&self) -> impl Iterator<Item = (&Location, u64)> {
        self.items.iter().map(|x| (&x.0, x.1))
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::{HistEntry, NavHistory, VisitedDirs};
    use crate::loc::Location;

    fn h(n: u32) -> HistEntry {
        HistEntry {
            loc: Location::local(format!("/d{n}")),
            focus: None,
            top: n,
        }
    }

    #[test]
    fn back_forward() {
        let mut nav = NavHistory::new(10);
        assert_eq!(nav.back(h(0)), None);
        nav.push(h(1));
        nav.push(h(2));
        assert_eq!(nav.back(h(3)), Some(h(2)));
        assert_eq!(nav.back(h(2)), Some(h(1)));
        assert!(!nav.can_back());
        assert_eq!(nav.forward(h(1)), Some(h(2)));
        assert_eq!(nav.forward(h(2)), Some(h(3)));
        assert_eq!(nav.forward(h(3)), None);
        nav.back(h(3));
        nav.push(h(9));
        assert!(!nav.can_forward());
    }

    #[test]
    fn capped_on_both_sides() {
        let mut nav = NavHistory::new(3);
        for i in 0..10 {
            nav.push(h(i));
        }
        let mut seen = Vec::new();
        let mut cur = h(100);
        while let Some(p) = nav.back(cur.clone()) {
            seen.push(p.top);
            cur = p;
        }
        assert_eq!(seen, [9, 8, 7]);
        let mut fwd = 0;
        while let Some(n) = nav.forward(cur.clone()) {
            fwd += 1;
            cur = n;
        }
        assert_eq!(fwd, 3);
    }

    #[test]
    fn visited_time_and_remove() {
        let mut v = VisitedDirs::new(5);
        let (a, b) = (Location::local("/a"), Location::local("/b"));
        v.touch_at(&a, 10);
        v.touch(&b);
        let at: Vec<u64> = v.iter_at().map(|(_, t)| t).collect();
        assert!(at[0] > 1_700_000_000 && at[1] == 10, "{at:?}");
        assert!(v.remove(&b));
        assert!(!v.remove(&b));
        assert_eq!(v.iter().collect::<Vec<_>>(), [&a]);
    }

    #[test]
    fn visited_mru() {
        let mut v = VisitedDirs::new(3);
        for n in [1, 2, 3, 1, 4] {
            v.touch(&Location::local(format!("/d{n}")));
        }
        let got: Vec<_> = v
            .iter()
            .map(|l| l.path.to_string_lossy().into_owned())
            .collect();
        let want: Vec<_> = [4, 1, 3]
            .iter()
            .map(|n| {
                Location::local(format!("/d{n}"))
                    .path
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        assert_eq!(got, want);
        assert_eq!(v.len(), 3);
    }
}
