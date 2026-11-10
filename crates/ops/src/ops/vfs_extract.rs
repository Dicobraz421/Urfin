//! Выборка из архива потоками `Vfs::extract_plan` (S8 §3.5, S8-27): данные файлов
//! идут в порядке, удобном архиву, а работы job — по одной на файл, как у `sftp`
//! (конфликты, пропуск, отчёт, job-бар и Finalizer каталогов те же).
//!
//! Потоки выборки — общий пул задачи. Работа файла `i` ждёт свои данные: свободный поток
//! берёт сама и проходит его по одному файлу — чужой файл пишет за его работу и кладёт
//! итог в `done`; работа того файла потом только забирает итог. Поток в каждый момент
//! ведёт один воркер, между файлами он возвращается в пул. Файла нет ни в одном живом
//! потоке (поток кончился, ошибка выборки, ошибка записи файла) — работа идёт обычным
//! путём `open_read` с докачкой и политикой ошибок.

use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::sync::{Condvar, Mutex, PoisonError};
use std::time::Duration;

use urfin_vfs::{ExtractStream, VfsError};

use crate::engine::{ExecCx, ItemResult};

/// Шаг ожидания чужого потока: отмена и пауза видны не позже.
const WAIT: Duration = Duration::from_millis(10);

/// Итог файла, записанного потоком выборки.
pub(crate) enum Outcome {
    Done(ItemResult),
    /// Не вышло из потока: файл — обычным путём.
    Fallback,
}

struct St<I> {
    /// Поток в пуле; `None` — его ведёт воркер или он кончился (`live`).
    streams: Vec<Option<Box<dyn ExtractStream>>>,
    live: Vec<bool>,
    /// Файлы, чьи работы поставлены, — данные для записи за них.
    wanted: HashMap<u32, I>,
    /// Пишутся сейчас.
    flight: HashSet<u32>,
    done: HashMap<u32, Outcome>,
}

/// Пул потоков выборки задачи; `I` — работа файла.
pub(crate) struct Extract<I> {
    st: Mutex<St<I>>,
    cv: Condvar,
}

/// Шаг потока.
enum Step {
    More,
    End,
    Cancelled,
}

impl<I: Clone> Extract<I> {
    /// Пул над `streams`; `wanted` — работы файлов по номеру во входном списке плана.
    pub(crate) fn new(streams: Vec<Box<dyn ExtractStream>>, wanted: HashMap<u32, I>) -> Self {
        let live = vec![true; streams.len()];
        Extract {
            st: Mutex::new(St {
                streams: streams.into_iter().map(Some).collect(),
                live,
                wanted,
                flight: HashSet::new(),
                done: HashMap::new(),
            }),
            cv: Condvar::new(),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, St<I>> {
        self.st.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Итог файла `i`: `None` — данных из потоков не будет, файл — обычным путём.
    /// `write` пишет файл из данных потока (любой, не только `i`).
    pub(crate) fn take(
        &self,
        i: u32,
        cx: &ExecCx<'_>,
        write: &dyn Fn(&I, &mut dyn Read) -> Outcome,
    ) -> Option<ItemResult> {
        let mut g = self.lock();
        loop {
            match g.done.remove(&i) {
                Some(Outcome::Done(r)) => return Some(r),
                Some(Outcome::Fallback) => return None,
                None => {}
            }
            if !g.flight.contains(&i) {
                if !g.wanted.contains_key(&i) {
                    return None;
                }
                if let Some(k) = g.streams.iter().position(Option::is_some) {
                    let Some(mut s) = g.streams[k].take() else {
                        continue;
                    };
                    drop(g);
                    let step = self.drive(s.as_mut(), cx, write);
                    g = self.lock();
                    match step {
                        Step::More => g.streams[k] = Some(s),
                        Step::End => g.live[k] = false,
                        Step::Cancelled => {
                            g.streams[k] = Some(s);
                            self.cv.notify_all();
                            return Some(ItemResult::Cancelled);
                        }
                    }
                    self.cv.notify_all();
                    continue;
                }
                if !g.live.iter().any(|&l| l) {
                    // Потоки кончились, а файла не было: обычным путём.
                    g.wanted.remove(&i);
                    return None;
                }
            }
            // Файл пишет другой воркер или все потоки заняты.
            g = self
                .cv
                .wait_timeout(g, WAIT)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
            if cx.checkpoint().is_err() {
                return Some(ItemResult::Cancelled);
            }
        }
    }

    /// Один файл потока `s`: записать, если его работа ждёт, иначе пропустить.
    fn drive(
        &self,
        s: &mut dyn ExtractStream,
        cx: &ExecCx<'_>,
        write: &dyn Fn(&I, &mut dyn Read) -> Outcome,
    ) -> Step {
        let next = match s.next(&cx.cancel_token()) {
            Ok(Some(n)) => n,
            Ok(None) => return Step::End,
            Err(VfsError::Cancelled) => return Step::Cancelled,
            Err(e) => {
                // Остаток потока — обычным путём (там же ошибка уйдёт в политику).
                tracing::info!(target: "ops", error = %e, "extract stream failed");
                return Step::End;
            }
        };
        let (j, mut r) = next;
        let item = {
            let mut g = self.lock();
            let it = g.wanted.remove(&j);
            if it.is_some() {
                g.flight.insert(j);
            }
            it
        };
        let Some(item) = item else {
            return Step::More;
        };
        let out = write(&item, &mut r);
        drop(r);
        let cancelled = matches!(out, Outcome::Done(ItemResult::Cancelled));
        let mut g = self.lock();
        g.flight.remove(&j);
        g.done.insert(j, out);
        drop(g);
        if cancelled {
            Step::Cancelled
        } else {
            Step::More
        }
    }
}
