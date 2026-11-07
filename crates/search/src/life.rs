//! Время жизни сторов поиска (S8 §2.4) — вся политика здесь, handler только спрашивает.
//!
//! Сейчас: стор держат экран, вкладка с `search:/id` (любой вид) и идущая job. Никто не
//! держит — стор в «недавних» (не больше 8 поисков и 64 мегабайт), оттуда вытесняется старый.
//! Закрытие экрана (Esc, новый поиск с экрана) отменяет поиск, найденное уходит в недавние:
//! Alt+F7 возвращает последний поиск, пока он не вытеснен (решение 2026-11-08
//! «s8-esc-nav»). Поменять политику — [`on_screen_closed`], [`fate`], [`RECENT`] и
//! [`RECENT_BYTES`].

use std::collections::VecDeque;

use urfin_core::search::SearchId;

/// Кто держит стор.
#[allow(clippy::struct_excessive_bools, reason = "три независимых держателя")]
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Holds {
    /// Стор на экране поиска.
    pub screen: bool,
    /// Есть вкладка `search:/id`.
    pub tab: bool,
    /// Job поиска ещё идёт.
    pub job: bool,
}

/// Что делать со стором.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum Fate {
    /// Держать.
    Keep,
    /// В недавние: живёт, пока не вытеснен.
    Recent,
    /// Убрать сейчас.
    Drop,
}

/// Недавних поисков, не больше.
const RECENT: usize = 8;
/// Памяти у недавних, байт, не больше.
const RECENT_BYTES: u64 = 64 << 20;

/// Экран закрыт (Esc, второй запуск на том же экране). Поиск уже отменён; стор дальше
/// живёт по своим держателям ([`fate`]): нет вкладки — в недавние.
pub(crate) fn on_screen_closed() -> Fate {
    Fate::Recent
}

/// Срок стора по его держателям (каждый pump).
pub(crate) fn fate(h: Holds) -> Fate {
    if h.screen || h.tab || h.job {
        Fate::Keep
    } else {
        Fate::Recent
    }
}

/// Недавние сверх лимита — старые первыми.
pub(crate) fn trim(recent: &mut Recent) -> Vec<SearchId> {
    let mut out = Vec::new();
    while recent.list.len() > RECENT || recent.bytes() > RECENT_BYTES {
        let Some((id, _)) = recent.list.pop_front() else {
            break;
        };
        out.push(id);
    }
    out
}

/// Недавние: старые впереди.
#[derive(Debug, Default)]
pub(crate) struct Recent {
    list: VecDeque<(SearchId, u64)>,
}

impl Recent {
    pub fn contains(&self, id: SearchId) -> bool {
        self.list.iter().any(|(i, _)| *i == id)
    }

    /// Стор отпущен: в конец очереди.
    pub fn push(&mut self, id: SearchId, bytes: u64) {
        if !self.contains(id) {
            self.list.push_back((id, bytes));
        }
    }

    /// Стор снова держат (вернулись во вкладку) или убран.
    pub fn forget(&mut self, id: SearchId) {
        self.list.retain(|(i, _)| *i != id);
    }

    fn bytes(&self) -> u64 {
        self.list.iter().map(|(_, b)| b).sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recent_keeps_eight_and_the_byte_limit() {
        let mut r = Recent::default();
        for i in 1..=10 {
            r.push(SearchId(i), 1);
        }
        assert_eq!(trim(&mut r), [SearchId(1), SearchId(2)]);
        r.push(SearchId(11), (64 << 20) + 1);
        let gone = trim(&mut r);
        assert_eq!(gone.len(), 9, "большой вытесняет всех и себя: {gone:?}");
        assert!(!r.contains(SearchId(11)));
    }

    #[test]
    fn fate_follows_holders() {
        assert_eq!(fate(Holds::default()), Fate::Recent);
        for h in [
            Holds {
                screen: true,
                ..Holds::default()
            },
            Holds {
                tab: true,
                ..Holds::default()
            },
            Holds {
                job: true,
                ..Holds::default()
            },
        ] {
            assert_eq!(fate(h), Fate::Keep);
        }
        assert_eq!(on_screen_closed(), Fate::Recent);
    }
}
