//! Замер редактора на движке (S7-16, S7-34, `--bench-editor <файл>`): открытие до
//! первого экрана, память, кадр прокрутки, нажатие и прыжок. Нажатие — правка через API
//! документа (`InboxMsg::Input`, как commit IME), прокрутка — `InboxMsg::Scroll`, прыжок
//! — `InboxMsg::Caret` на долю документа; не синтетический ввод в окно. Подсветка
//! (S7-35): первые цвета, память после них и время до новых цветов после правки.
//! Предпросмотр (S7-24, `.md` при `editor.preview = true`): нажатия идут с ним, `preview_ms`
//! — от последнего нажатия до нового документа. Итог — строка `key=value` в stdout, затем
//! выход.
//!
//! Файл открывается уже на рабочем окне (`screen_ms` — от команды до нарисованного первого
//! экрана), нажатия — в середине документа (S7-36: начало большого файла не показательно).
//! Сценарии, меняющие файл или тяжёлые, — только по переменным окружения (S7-36):
//! `URFIN_BENCH_PASTE_MB=<n>` — вставка n мебибайт одной правкой (`InboxMsg::Input`: те же
//! разбор концов строк, кодирование и правка, что у буфера обмена; буфер владельца не
//! трогается), `URFIN_BENCH_REPLACE=<что>=<чем>` — «Заменить все», `URFIN_BENCH_SAVE=1` —
//! сохранение поверх файла. `URFIN_PROF=<файл>` и
//! `URFIN_PROF_PHASE=open|scroll|type|jump|paste|replace|save` — профиль UI-потока одной
//! фазы (`open` — от команды до первого экрана). `frames_ms` — начала кадров от команды до
//! первого экрана: куда ушло время открытия.

use std::time::{Duration, Instant};

use urfin_core::Command;

/// Кадров прогрева после передачи текста, прокрутки, нажатий и прыжков.
const WARMUP: usize = 20;
const SCROLL: usize = 120;
const TYPE: usize = 300;
const JUMP: usize = 40;
/// Кадров после перехода курсора в середину документа до первого нажатия.
const TYPE_SETTLE: usize = 4;
/// Предел ожидания вставки, замены и сохранения.
const JOB_LIMIT: Duration = Duration::from_mins(30);

/// Что сделать в этом кадре.
#[derive(Clone, Debug)]
pub(super) enum Act {
    None,
    /// Экран на столько строк вниз.
    Down(u32),
    /// Символ в позицию курсора.
    Type,
    /// Курсор на долю документа (холодный экран).
    Jump(u32),
    /// Вставка одной правкой (текст — [`EdBench::take_paste`]).
    Paste,
    /// Команда редактора: сохранение, замена.
    Cmd(Box<Command>),
    /// Отчёт напечатан: выйти.
    Exit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ph {
    Load,
    Warmup,
    Scroll,
    Type,
    /// Набор кончился: ждём документ предпросмотра.
    Settle,
    Jump,
    Paste,
    Replace,
    Save,
    Done,
}

impl Ph {
    fn name(self) -> &'static str {
        match self {
            Ph::Scroll => "scroll",
            Ph::Type => "type",
            Ph::Jump => "jump",
            Ph::Paste => "paste",
            Ph::Replace => "replace",
            Ph::Save => "save",
            _ => "",
        }
    }
}

/// Долгая работа документа (вставка, замена, сохранение): время и кадры UI-потока за неё.
#[derive(Default)]
struct Job {
    /// От команды до конца работы, мс; `None` — не шла, `-1` — не дождались.
    ms: Option<f64>,
    frames: Vec<f32>,
    /// Рост пика private за работу, МБ.
    mem_mb: f64,
}

impl Job {
    fn report(&self, name: &str) -> String {
        let Some(ms) = self.ms else {
            return String::new();
        };
        format!(
            " {name}_ms={ms:.0} {name}_p50={:.2} {name}_p99={:.2} {name}_max={:.2} {name}_mem_mb={:.0}",
            pct(&self.frames, 0.5),
            pct(&self.frames, 0.99),
            pct(&self.frames, 1.0),
            self.mem_mb,
        )
    }
}

/// Что документ делает сейчас: конец вставки, замены и сохранения.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Doc {
    /// Длина текста.
    pub len: u64,
    /// Идёт фоновая работа (запись, замена, досчёт).
    pub busy: bool,
    /// Документ ждёт ответа на вопрос: работа не кончится.
    pub ask: bool,
}

/// Замер одного файла.
pub(super) struct EdBench {
    started: Instant,
    ph: Ph,
    n: usize,
    /// Пик private до открытия, МБ.
    mem0: f64,
    pub bytes: u64,
    pub lines: u64,
    /// Документ готов, страницы на месте, мс от команды открытия.
    load_ms: f64,
    /// Первый экран нарисован, мс от команды открытия.
    screen_ms: f64,
    /// Пик private после открытия, МБ.
    mem1: f64,
    /// Начала кадров открытия, мс от команды.
    opening: Vec<f32>,
    /// Первый кадр после передачи (UI CPU), мс.
    first_ms: f64,
    last_cycles: u64,
    last_at: Option<Instant>,
    scroll: Vec<f32>,
    typing: Vec<f32>,
    typing_wall: Vec<f32>,
    jump: Vec<f32>,
    /// Подсветка (S7-35): первые цвета от старта, мс; пик private после них, МБ; сколько
    /// ждали новые цвета после правок, мс; счётчик учтённых.
    hl_first_ms: f64,
    mem_hl: f64,
    hl: Vec<f32>,
    hl_seen: u64,
    /// Предпросмотра нет или он догнал текст.
    preview_idle: bool,
    typed_at: Option<Instant>,
    preview_ms: f64,
    /// Текст вставки (`URFIN_BENCH_PASTE_MB`).
    paste_text: Option<String>,
    /// Замена (`URFIN_BENCH_REPLACE`): что, чем.
    replace_with: Option<(String, String)>,
    /// Какие работы идут после прыжков, по порядку.
    jobs: Vec<Ph>,
    paste: Job,
    replace: Job,
    save: Job,
    job_at: Option<Instant>,
    job_mem: f64,
    job_len: u64,
    job_seen: bool,
    doc: Doc,
    #[cfg(windows)]
    prof: Option<crate::sys::prof::Sampler>,
}

pub(super) fn peak_private_mb() -> f64 {
    let (ws, private) = urfin_vfs::diag::peak_memory().unwrap_or((0, None));
    #[allow(clippy::cast_precision_loss, reason = "МБ")]
    let mb = private.unwrap_or(ws) as f64 / 1_048_576.0;
    mb
}

pub(super) fn pct(v: &[f32], q: f32) -> f32 {
    if v.is_empty() {
        return 0.0;
    }
    let mut v = v.to_vec();
    v.sort_by(f32::total_cmp);
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss,
        reason = "индекс перцентиля"
    )]
    let i = ((v.len() - 1) as f32 * q).round() as usize;
    v[i.min(v.len() - 1)]
}

/// Текст вставки: строки по 64 байта, всего `mb` мебибайт.
fn paste_text(mb: usize) -> String {
    const LINE: &str = "2026-11-05 12:00:00 INFO paste line of sixty-four bytes total..\n";
    LINE.repeat((mb << 20) / LINE.len())
}

impl EdBench {
    pub(super) fn new() -> EdBench {
        // Калибровка счётчика циклов — занятый цикл 30 мс: не в замере открытия.
        crate::sys::cpu::cycles_per_ms();
        let paste_text = std::env::var("URFIN_BENCH_PASTE_MB")
            .ok()
            .and_then(|v| v.parse().ok())
            .map(paste_text);
        let replace_with = std::env::var("URFIN_BENCH_REPLACE").ok().and_then(|v| {
            let (a, b) = v.split_once('=')?;
            Some((a.to_owned(), b.to_owned()))
        });
        let save = std::env::var_os("URFIN_BENCH_SAVE").is_some();
        let jobs = [
            (Ph::Paste, paste_text.is_some()),
            (Ph::Replace, replace_with.is_some()),
            (Ph::Save, save),
        ]
        .into_iter()
        .filter_map(|(ph, on)| on.then_some(ph))
        .rev()
        .collect();
        EdBench {
            started: Instant::now(),
            ph: Ph::Load,
            n: 0,
            mem0: peak_private_mb(),
            bytes: 0,
            lines: 0,
            load_ms: 0.0,
            screen_ms: 0.0,
            mem1: 0.0,
            opening: Vec::new(),
            first_ms: 0.0,
            last_cycles: 0,
            last_at: None,
            scroll: Vec::new(),
            typing: Vec::new(),
            typing_wall: Vec::new(),
            jump: Vec::new(),
            hl_first_ms: 0.0,
            mem_hl: 0.0,
            hl: Vec::new(),
            hl_seen: 0,
            preview_idle: true,
            typed_at: None,
            preview_ms: 0.0,
            paste_text,
            replace_with,
            jobs,
            paste: Job::default(),
            replace: Job::default(),
            save: Job::default(),
            job_at: None,
            job_mem: 0.0,
            job_len: 0,
            job_seen: false,
            doc: Doc::default(),
            #[cfg(windows)]
            prof: None,
        }
    }

    /// Предпросмотр догнал текст (или его нет).
    pub(super) fn preview(&mut self, idle: bool) {
        self.preview_idle = idle;
    }

    /// Что документ делает: конец вставки, замены и сохранения.
    pub(super) fn doc(&mut self, doc: Doc) {
        self.doc = doc;
    }

    /// Текст вставки: забирает фича на [`Act::Paste`].
    pub(super) fn take_paste(&mut self) -> String {
        self.paste_text.take().unwrap_or_default()
    }

    /// Кадр открытия.
    pub(super) fn opening(&mut self) {
        #[cfg(windows)]
        if self.opening.is_empty() && std::env::var("URFIN_PROF_PHASE").is_ok_and(|v| v == "open") {
            self.prof = crate::sys::prof::start();
        }
        #[allow(clippy::cast_possible_truncation, reason = "мс")]
        self.opening
            .push((self.started.elapsed().as_secs_f64() * 1000.0) as f32);
    }

    /// Документ открыт, первый экран на месте.
    pub(super) fn opened(&mut self) {
        if self.ph != Ph::Load {
            return;
        }
        self.load_ms = self.started.elapsed().as_secs_f64() * 1000.0;
        self.mem1 = peak_private_mb();
        self.ph = Ph::Warmup;
        self.n = 0;
        self.last_cycles = crate::sys::cpu::thread_cycles();
        self.last_at = Some(Instant::now());
    }

    /// Подсветка в кадре: прогрев ждёт первых цветов (`true` — кадр пропустить), при
    /// наборе и прыжках — время до новых цветов после каждой догонки.
    pub(super) fn hl(&mut self, busy: bool, fresh_n: u64, last_ms: Option<f32>) -> bool {
        // Кадр с текстом уже нарисован: первые цвета экран не ждёт.
        if self.ph == Ph::Warmup && self.screen_ms == 0.0 {
            self.screen_ms = self.started.elapsed().as_secs_f64() * 1000.0;
            self.opening();
        }
        match self.ph {
            Ph::Warmup if busy => {
                // Кадры ожидания не в замере: отсчёт — с последнего из них.
                self.last_cycles = crate::sys::cpu::thread_cycles();
                self.last_at = Some(Instant::now());
                return true;
            }
            Ph::Warmup if self.hl_first_ms == 0.0 => {
                self.hl_first_ms = self.started.elapsed().as_secs_f64() * 1000.0;
                self.mem_hl = peak_private_mb();
                self.hl_seen = fresh_n;
            }
            Ph::Type | Ph::Jump if fresh_n != self.hl_seen => {
                self.hl_seen = fresh_n;
                self.hl.extend(last_ms);
            }
            _ => {}
        }
        false
    }

    /// Ещё загружается (кадры не нужны).
    pub(super) fn waiting(&self) -> bool {
        self.ph == Ph::Load
    }

    /// Смена фазы: профиль — только выбранной (`URFIN_PROF_PHASE`).
    fn enter(&mut self, ph: Ph) {
        #[cfg(windows)]
        {
            if let Some(p) = self.prof.take() {
                p.finish();
            }
            if std::env::var("URFIN_PROF_PHASE").is_ok_and(|v| v == ph.name()) {
                self.prof = crate::sys::prof::start();
            }
        }
        self.ph = ph;
        self.n = 0;
    }

    /// Следующая работа или конец замера.
    fn next_job(&mut self) -> Act {
        let Some(ph) = self.jobs.pop() else {
            self.enter(Ph::Done);
            let report = self.report();
            println!("bench editor {report}");
            println!("{}", crate::bench::json_line("editor", &report, &[]));
            return Act::Exit;
        };
        self.enter(ph);
        self.job_at = None;
        self.job_seen = false;
        self.job_len = self.doc.len;
        self.job_mem = peak_private_mb();
        Act::None
    }

    /// Кадр работы: кончилась — итог и следующая фаза.
    fn job_frame(&mut self, now: Instant) -> Act {
        let at = self.job_at.unwrap_or(now);
        self.job_seen |= self.doc.busy || self.doc.len != self.job_len;
        let waited = now.duration_since(at);
        let done = self.job_seen && !self.doc.busy && !self.doc.ask;
        if !done && !self.doc.ask && waited < JOB_LIMIT {
            return Act::None;
        }
        let ms = if done {
            waited.as_secs_f64() * 1000.0
        } else {
            -1.0
        };
        let mem_mb = peak_private_mb() - self.job_mem;
        let job = match self.ph {
            Ph::Paste => &mut self.paste,
            Ph::Replace => &mut self.replace,
            _ => &mut self.save,
        };
        job.ms = Some(ms);
        job.mem_mb = mem_mb;
        self.next_job()
    }

    /// Кадр (зовёт фича в начале кадра): замер прошлого кадра и действие этого.
    pub(super) fn frame(&mut self) -> Act {
        if self.ph == Ph::Load {
            return Act::None;
        }
        let cycles = crate::sys::cpu::thread_cycles();
        #[allow(
            clippy::cast_precision_loss,
            clippy::cast_possible_truncation,
            reason = "мс"
        )]
        let ui = (cycles.saturating_sub(self.last_cycles) as f64 / crate::sys::cpu::cycles_per_ms())
            as f32;
        self.last_cycles = cycles;
        let now = Instant::now();
        let wall = self
            .last_at
            .replace(now)
            .map_or(0.0, |t| now.duration_since(t).as_secs_f32() * 1000.0);
        self.n += 1;
        match self.ph {
            Ph::Load | Ph::Done => Act::None,
            Ph::Warmup => {
                if self.n == 1 {
                    self.first_ms = f64::from(ui);
                    #[cfg(windows)]
                    if let Some(p) = self.prof.take() {
                        p.finish();
                    }
                }
                if self.n >= WARMUP {
                    self.enter(Ph::Scroll);
                }
                Act::None
            }
            Ph::Scroll => {
                if self.n > 1 {
                    self.scroll.push(ui);
                }
                if self.n >= SCROLL {
                    self.enter(Ph::Type);
                    return Act::None;
                }
                Act::Down(40)
            }
            Ph::Type => {
                // Первые кадры: курсор — в середину документа, экран дочитывается.
                if self.n > TYPE_SETTLE + 1 {
                    self.typing.push(ui);
                    self.typing_wall.push(wall);
                }
                if self.n >= TYPE + TYPE_SETTLE {
                    self.enter(Ph::Settle);
                    self.typed_at = Some(now);
                    return Act::None;
                }
                match self.n {
                    1 => Act::Jump(500),
                    n if n <= TYPE_SETTLE => Act::None,
                    _ => Act::Type,
                }
            }
            // Кадр после последнего нажатия: предпросмотр ещё не знает о нём.
            Ph::Settle => {
                if self.n > 2 && self.preview_idle {
                    self.preview_ms = self
                        .typed_at
                        .map_or(0.0, |t| now.duration_since(t).as_secs_f64() * 1000.0);
                    self.enter(Ph::Jump);
                }
                Act::None
            }
            Ph::Jump => {
                if self.n > 1 {
                    self.jump.push(ui);
                }
                if self.n >= JUMP {
                    return self.next_job();
                }
                #[allow(clippy::cast_possible_truncation, reason = "кадров немного")]
                Act::Jump((self.n as u32 * 7919) % 1000)
            }
            Ph::Paste | Ph::Replace | Ph::Save => self.job_step(ui, now),
        }
    }

    /// Первый кадр работы — команда, дальше — кадры до её конца.
    fn job_step(&mut self, ui: f32, now: Instant) -> Act {
        if self.n > 1 {
            let job = match self.ph {
                Ph::Paste => &mut self.paste,
                Ph::Replace => &mut self.replace,
                _ => &mut self.save,
            };
            job.frames.push(ui);
            return self.job_frame(now);
        }
        self.job_at = Some(now);
        match self.ph {
            Ph::Paste => Act::Paste,
            Ph::Replace => {
                let (text, with) = self.replace_with.clone().unwrap_or_default();
                let c = Command::new("editor.replace")
                    .arg("text", text)
                    .arg("with", with)
                    .arg("all", true);
                Act::Cmd(Box::new(c))
            }
            _ => Act::Cmd(Box::new(Command::new("editor.save"))),
        }
    }

    pub(super) fn report(&self) -> String {
        #[allow(clippy::cast_precision_loss, reason = "МБ")]
        let mb = self.bytes as f64 / 1_048_576.0;
        let frames: Vec<String> = self.opening.iter().map(|t| format!("{t:.0}")).collect();
        format!(
            "frames_ms={} size_mb={mb:.1} lines={} open_ms={:.1} screen_ms={:.1} first_frame_ms={:.1} mem_before_mb={:.0} mem_after_mb={:.0} hl_first_ms={:.0} mem_hl_mb={:.0} scroll_p50={:.2} scroll_p99={:.2} type_p50={:.2} type_p99={:.2} type_max={:.2} type_wall_p50={:.2} type_wall_p99={:.2} hl_n={} hl_p50={:.1} hl_p99={:.1} hl_max={:.1} jump_p50={:.2} jump_p99={:.2} preview_ms={:.0} mem_end_mb={:.0}{}{}{}",
            frames.join(","),
            self.lines,
            self.load_ms,
            self.screen_ms,
            self.first_ms,
            self.mem0,
            self.mem1,
            self.hl_first_ms,
            self.mem_hl,
            pct(&self.scroll, 0.5),
            pct(&self.scroll, 0.99),
            pct(&self.typing, 0.5),
            pct(&self.typing, 0.99),
            pct(&self.typing, 1.0),
            pct(&self.typing_wall, 0.5),
            pct(&self.typing_wall, 0.99),
            self.hl.len(),
            pct(&self.hl, 0.5),
            pct(&self.hl, 0.99),
            pct(&self.hl, 1.0),
            pct(&self.jump, 0.5),
            pct(&self.jump, 0.99),
            self.preview_ms,
            peak_private_mb(),
            self.paste.report("paste"),
            self.replace.report("replace"),
            self.save.report("save"),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phases_run_to_exit() {
        let mut b = EdBench::new();
        assert!(matches!(b.frame(), Act::None));
        assert!(b.waiting());
        b.opened();
        assert!(!b.waiting());
        let mut acts = 0;
        for _ in 0..10_000 {
            match b.frame() {
                Act::Exit => {
                    assert!(b.report().contains("type_p99"));
                    assert!(acts > TYPE);
                    return;
                }
                Act::None => {}
                _ => acts += 1,
            }
        }
        panic!("bench did not finish");
    }

    /// Работа кончается по документу; вопрос документа не вешает замер.
    #[test]
    fn job_waits_for_the_document() {
        let mut b = EdBench::new();
        b.opened();
        b.jobs = vec![Ph::Save];
        for _ in 0..10_000 {
            if b.ph == Ph::Save {
                break;
            }
            b.frame();
        }
        assert_eq!(b.ph, Ph::Save);
        assert!(matches!(b.frame(), Act::Cmd(_)));
        assert!(matches!(b.frame(), Act::None));
        b.doc(Doc {
            busy: true,
            ..Doc::default()
        });
        assert!(matches!(b.frame(), Act::None));
        b.doc(Doc::default());
        assert!(matches!(b.frame(), Act::Exit));
        assert!(b.save.ms.is_some_and(|ms| ms >= 0.0));
        assert!(b.report().contains("save_p99"));
    }
}
