//! Поля ввода окна GPUI (G-05): черновик команды (`ui.draft`: назначение копирования
//! и переноса, имя каталога, S2-15) и переименование на месте (`ui.rename`, S2-16).
//! Поле — `InputState` gpui-kit (IME, выделение, буфер обмена); Enter и Esc разбирает
//! хост, остальные клавиши — поле.

use gpui_kit::component::Sizable as _;
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::prelude::*;
use gpui_kit::{Context, Div, Entity, SharedString, Window, div, px};
use urfin_core::{AppState, Command, Row, Tab};
use urfin_i18n::FluentArgs;

use std::cell::Cell;
use std::rc::Rc;

use gpui_kit::{Bounds, Pixels};

use super::theme::Theme;
use super::{CoreHost, overlays};
use crate::UiState;
use crate::field_history::FieldKind;
use crate::tokens::Role;

/// Что сейчас редактируется.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Key {
    /// Черновик команды (`ops.copy`, `ops.mkdir`…).
    Draft(String),
    /// Переименование в табе (id таба, старое имя).
    Rename(u32, String),
    /// Правка пути панели (`ui.path_edit`, ADR-0014).
    Path(urfin_core::Side),
}

pub(super) struct Field {
    pub key: Key,
    pub input: Entity<InputState>,
    /// Подписка на правку поля (превью группового переименования, дополнение).
    pub _sub: Option<gpui_kit::Subscription>,
    /// Где поле на экране (прошлый кадр): от него открывается дополнение (№70).
    pub bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    /// Где кнопка выпадающего списка строки («Если файл есть ▾»).
    pub drop: Rc<Cell<Option<Bounds<Pixels>>>>,
}

impl Field {
    /// Вид поля для дополнения и истории (№70); переименование — без них (Tab там —
    /// имя ↔ расширение).
    pub(super) fn kind(&self) -> Option<FieldKind> {
        match &self.key {
            Key::Draft(cmd) => FieldKind::of_draft(cmd),
            Key::Path(_) => Some(FieldKind::Path),
            Key::Rename(..) => None,
        }
    }

    /// Пустой элемент на всю площадь обёртки поля: запоминает её положение.
    pub(super) fn probe(&self) -> impl IntoElement + use<> {
        probe(Rc::clone(&self.bounds))
    }

    /// То же для кнопки выпадающего списка.
    pub(super) fn probe_drop(&self) -> impl IntoElement + use<> {
        probe(Rc::clone(&self.drop))
    }
}

fn probe(cell: Rc<Cell<Option<Bounds<Pixels>>>>) -> impl IntoElement {
    gpui_kit::canvas(move |b, _, _| cell.set(Some(b)), |_, (), _, _| {})
        .absolute()
        .top_0()
        .left_0()
        .size_full()
}

/// Черновик группового переименования (S5-23): поле — шаблон, под ним превью.
pub(super) const MULTI_RENAME: &str = "ops.multi_rename";

/// Превью плана группового переименования от handler ops.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct MrPreview {
    /// (было, станет, проблема: `""`, `invalid`, `duplicate`, `exists`).
    pub rows: Vec<(String, String, String)>,
    pub changed: i64,
    pub problems: i64,
    pub error: Option<String>,
}

impl MrPreview {
    pub(super) fn from_args(a: &urfin_core::Args) -> MrPreview {
        let rows = a
            .list("rows")
            .map(|l| {
                l.iter()
                    .filter_map(|v| match v {
                        urfin_core::Value::Str(s) => Some(s.as_str()),
                        _ => None,
                    })
                    .map(|r| {
                        let mut it = r.splitn(3, '\t');
                        let mut next = || it.next().unwrap_or_default().to_owned();
                        (next(), next(), next())
                    })
                    .collect()
            })
            .unwrap_or_default();
        MrPreview {
            rows,
            changed: a.int("changed").unwrap_or(0),
            problems: a.int("problems").unwrap_or(0),
            error: a.str("error").map(str::to_owned),
        }
    }
}

/// Команда превью (или применения) по тексту шаблона. Шаблон с `/` — regex
/// «найти/заменить» (в имени файла `/` не бывает), это решает handler.
pub(super) fn mr_command(pattern: &str, apply: bool) -> Command {
    let mut c = Command::new(MULTI_RENAME).arg("pattern", pattern);
    if apply {
        c = c.arg("apply", true);
    }
    c
}

/// Черновик с полем ввода (у удаления поля нет — кнопки).
fn has_field(cmd: &str) -> bool {
    cmd != "ops.delete"
}

/// Путь таба для поля: локальный — путь ОС, иначе uri.
fn path_text(t: &Tab) -> String {
    if t.loc.is_local() {
        t.loc.path.display().to_string()
    } else {
        t.loc.to_uri()
    }
}

/// Нужное поле и его начальный текст по состоянию UI.
pub(super) fn wanted(s: &AppState) -> Option<(Key, String)> {
    let st = s.ext.get::<UiState>()?;
    if let Some(d) = &st.draft {
        if !has_field(&d.cmd) {
            return None;
        }
        let init = if d.text.is_empty() && matches!(d.cmd.as_str(), "ops.copy" | "ops.move") {
            path_text(s.panel(s.active.other()).tab())
        } else if d.text.is_empty() && d.cmd == MULTI_RENAME {
            "[N].[E]".to_owned()
        } else {
            d.text.to_string()
        };
        return Some((Key::Draft(d.cmd.to_string()), init));
    }
    if let Some(side) = st.path_edit {
        return Some((Key::Path(side), path_text(s.panel(side).tab())));
    }
    let (tab, name) = st.rename.as_ref()?;
    Some((Key::Rename(tab.0, name.to_string()), name.to_string()))
}

impl CoreHost {
    /// Поле — под текущее состояние: новое получает фокус и выделенный текст; без поля
    /// фокус возвращается окну.
    pub(super) fn sync_field(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let want = self.core.as_ref().and_then(|c| wanted(c.state()));
        match (&self.field, want) {
            (Some(f), Some((k, _))) if f.key == k => {}
            (_, Some((key, init))) => {
                // Новый черновик — опции строк по умолчанию.
                self.strip_opts = StripOpts::default();
                let mut init = init;
                let mut placeholder = None;
                if key == Key::Draft("ops.attrs".to_owned())
                    && let Some(core) = self.core.as_ref()
                {
                    let (now, mtime) = attrs_of(core.state().active_tab(), &self.theme.fmt);
                    self.strip_opts.attrs_now = now;
                    self.strip_opts.attrs = now;
                    if mtime.is_empty() {
                        placeholder = Some(urfin_i18n::current().get("ui-strip-attrs-varies"));
                    }
                    self.strip_opts.mtime_init.clone_from(&mtime);
                    init = mtime;
                }
                // Переименование: выделено имя без расширения (Tab — расширение).
                let range = match &key {
                    Key::Rename(_, name) => {
                        let dir = self.core.as_ref().is_some_and(|c| {
                            matches!(c.state().active_tab().current(),
                                Some(Row::Entry { entry, .. }) if entry.kind.is_dir_like())
                        });
                        Some(stem_range(name, dir))
                    }
                    _ => None,
                };
                let input = cx.new(|cx| {
                    let s = InputState::new(window, cx).default_value(init.clone());
                    match placeholder {
                        Some(p) => s.placeholder(p),
                        None => s,
                    }
                });
                input.update(cx, |s, cx| {
                    s.focus(window, cx);
                    match range {
                        Some(r) => s.set_selected_range(r, cx),
                        None => s.select_all(window, cx),
                    }
                });
                // Групповое переименование: превью на каждую правку шаблона.
                let multi = key == Key::Draft(MULTI_RENAME.to_owned());
                if multi {
                    self.run(mr_command(&init, false), window, cx);
                }
                // Правка поля: превью группового переименования и обновление открытого
                // списка дополнений (№70).
                let sub = cx.subscribe_in(
                    &input,
                    window,
                    move |h: &mut CoreHost,
                          input,
                          ev: &gpui_kit::component::input::InputEvent,
                          window,
                          cx| {
                        if !matches!(ev, gpui_kit::component::input::InputEvent::Change) {
                            return;
                        }
                        let text = input.read(cx).value().to_string();
                        if multi {
                            h.run(mr_command(&text, false), window, cx);
                        }
                        if let Some(c) = h.completion() {
                            h.run(
                                Command::new("ui.complete")
                                    .arg("kind", c.kind.as_str())
                                    .arg("text", text.as_str()),
                                window,
                                cx,
                            );
                        }
                    },
                );
                self.field = Some(Field {
                    key,
                    input,
                    _sub: Some(sub),
                    bounds: Rc::new(Cell::new(None)),
                    drop: Rc::new(Cell::new(None)),
                });
            }
            (Some(_), None) => {
                self.field = None;
                self.mr_preview = None;
                window.focus(&self.focus, cx);
            }
            (None, None) => {}
        }
    }

    /// Enter в поле или в черновике удаления: команды и закрытие.
    pub(super) fn field_commit(&mut self, shift: bool, cx: &mut Context<Self>) -> Vec<Command> {
        let st = self
            .core
            .as_ref()
            .and_then(|c| c.state().ext.get::<UiState>());
        let text = self
            .field
            .as_ref()
            .map(|f| f.input.read(cx).value().to_string())
            .unwrap_or_default();
        let mut out = Vec::with_capacity(2);
        if let Some(d) = st.and_then(|u| u.draft.as_ref()) {
            if d.cmd == "ops.delete" {
                let mut c = Command::new("ops.delete").arg("confirmed", true);
                if shift || self.ops_permanent {
                    c = c.arg("permanent", true);
                }
                out.push(c);
            } else if d.cmd == MULTI_RENAME {
                out.push(mr_command(&text, true));
            } else if d.cmd == "pkg.search" {
                // Строка остаётся: ответы менеджеров идут под ней (`screen-pkgsearch`).
                out.push(Command::new("pkg.search").arg("q", text.trim()));
                return out;
            } else if d.cmd == "ops.attrs" {
                return attrs_commands(&self.strip_opts, &text);
            } else if d.cmd == "ops.mkdir" {
                return mkdir_commands(self.strip_opts.many, &text);
            } else if let Ok(c) = Command::parse(&d.cmd) {
                let mut c = c.arg(&d.arg, text.as_str());
                if d.cmd == "ops.link" {
                    c = c.arg("kind", LINK_KINDS[self.strip_opts.link_kind % 3].0);
                }
                if matches!(d.cmd.as_str(), "ops.copy" | "ops.move") {
                    c = self.strip_opts.apply(c);
                    if shift {
                        c = c.arg("queue", "bg");
                    }
                }
                out.push(c);
            }
        } else if let Some(Key::Path(side)) = self.field.as_ref().map(|f| &f.key) {
            // Проверку и переход делает handler; он же закрывает поле.
            return vec![
                Command::new("ui.path_edit")
                    .arg("side", super::chrome::side_name(*side))
                    .arg("path", text.trim()),
            ];
        } else if let Some(Key::Rename(_, old)) = self.field.as_ref().map(|f| &f.key) {
            let to = text.trim();
            if !to.is_empty() && to != old {
                out.push(Command::new("ops.rename").arg("to", to));
            }
        }
        out.push(Command::new("ui.cancel"));
        out
    }
}

impl CoreHost {
    /// Открытый список дополнений поля (№70).
    pub(super) fn completion(&self) -> Option<&crate::state::Complete> {
        self.core
            .as_ref()
            .and_then(|c| c.state().ext.get::<UiState>())
            .and_then(|u| u.complete.as_ref())
            .filter(|_| self.field.as_ref().and_then(Field::kind).is_some())
    }

    /// `ui.field.remember` для Enter в поле: введённое — в историю его вида.
    pub(super) fn field_remember(&self, cx: &Context<Self>) -> Option<Command> {
        let f = self.field.as_ref()?;
        let kind = f.kind()?;
        let text = f.input.read(cx).value().to_string();
        (!text.trim().is_empty()).then(|| {
            Command::new("ui.field.remember")
                .arg("kind", kind.as_str())
                .arg("text", text.trim())
        })
    }

    /// Клавиша в поле при открытом или закрытом списке дополнений: `None` — не наша.
    /// Tab — открыть (или следующий вариант), Alt+↓ — история поля, ↑↓ — выбор,
    /// Enter — вставить, Esc — закрыть список (поле остаётся).
    pub(super) fn completion_key(
        &mut self,
        ks: &gpui_kit::Keystroke,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Vec<Command>> {
        let f = self.field.as_ref()?;
        let kind = f.kind()?;
        let text = f.input.read(cx).value().to_string();
        let open = self.completion().map(|c| (c.items.len(), c.generation));
        if let Some((_, generation)) = open
            && self.complete_sel.0 != generation
        {
            self.complete_sel = (generation, 0);
        }
        let ask = |history: bool| {
            let mut c = Command::new("ui.complete")
                .arg("kind", kind.as_str())
                .arg("text", text.as_str());
            if history {
                c = c.arg("history", true);
            }
            vec![c]
        };
        let alt = ks.modifiers.alt;
        match (ks.key.as_str(), open) {
            ("down", _) if alt => Some(ask(true)),
            ("tab", None) => Some(ask(false)),
            ("tab" | "down", Some((n, _))) if n > 0 => {
                self.complete_sel.1 = (self.complete_sel.1 + 1) % n;
                Some(Vec::new())
            }
            ("up", Some((n, _))) if n > 0 => {
                self.complete_sel.1 = (self.complete_sel.1 + n - 1) % n;
                Some(Vec::new())
            }
            ("escape", Some(_)) => Some(vec![Command::new("ui.complete.close")]),
            ("enter", Some((n, _))) if n > 0 => {
                let pick = self
                    .completion()
                    .and_then(|c| c.items.get(self.complete_sel.1))
                    .map(|c| c.text.clone())?;
                Some(self.complete_insert(&pick, kind, window, cx))
            }
            _ => None,
        }
    }

    /// Вставить вариант в поле; папка (`…\`) — список остаётся открытым для неё.
    pub(super) fn complete_insert(
        &mut self,
        pick: &str,
        kind: FieldKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<Command> {
        if let Some(f) = self.field.as_ref() {
            let value = pick.to_owned();
            f.input.update(cx, |s, cx| s.set_value(value, window, cx));
        }
        if kind == FieldKind::Path && pick.ends_with(['\\', '/']) {
            vec![
                Command::new("ui.complete")
                    .arg("kind", kind.as_str())
                    .arg("text", pick),
            ]
        } else {
            vec![Command::new("ui.complete.close")]
        }
    }

    /// «Сейчас» для поля времени (ISO, локальное).
    pub(super) fn now_iso(&self) -> String {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(i64::MIN, |d| {
                i64::try_from(d.as_nanos()).unwrap_or(i64::MIN)
            });
        let mut c = crate::fmt_cell::Cell::default();
        self.theme.fmt.write_iso(&mut c, nanos);
        c.as_str().to_owned()
    }

    /// Tab в поле переименования: выделение имени ↔ расширения. `false` — не наше поле.
    pub(super) fn field_tab(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(f) = self.field.as_ref() else {
            return false;
        };
        let Key::Rename(_, _) = &f.key else {
            return false;
        };
        f.input.update(cx, |s, cx| {
            let text = s.value().to_string();
            let stem = stem_range(&text, false);
            let next = if s.selected_range() == stem && stem.end < text.len() {
                stem.end + 1..text.len()
            } else {
                stem
            };
            s.set_selected_range(next, cx);
        });
        true
    }
}

/// Поле переименования для строки курсора таба `t` (если оно открыто для него).
pub(super) fn rename_input(field: Option<&Field>, t: &Tab) -> Option<Input> {
    let f = field?;
    match &f.key {
        // Высота поля — как у строки (24 px), не среднего поля gpui-kit (32 px).
        // Без своей рамки и фона: рамку и черту `accent` рисует строка (D-04).
        Key::Rename(tab, _) if *tab == t.id.0 => {
            Some(Input::new(&f.input).small().appearance(false))
        }
        _ => None,
    }
}

/// Сколько и какого объёма объектов затронет команда: выделение или строка курсора.
fn targets(t: &Tab) -> (u64, u64) {
    if !t.selection.is_empty() {
        return (u64::from(t.selection.len()), t.selection.bytes());
    }
    match t.current() {
        Some(Row::Entry { entry, .. }) => (1, entry.size),
        _ => (0, 0),
    }
}

/// Опции встроенных строк (D-04): F5/F6 — «Если файл есть» и проверка хэша; F7 —
/// несколько папок через `;`; Ctrl+A — флажки атрибутов и прежнее время.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct StripOpts {
    /// Индекс в [`CONFLICT`]: спросить / заменить / пропустить / если новее.
    pub conflict: usize,
    pub verify: bool,
    /// F7: имена через `;` — несколько папок.
    pub many: bool,
    /// Открыт список «Если файл есть».
    pub dropdown: bool,
    /// Alt+F6: вид ссылки по [`LINK_KINDS`].
    pub link_kind: usize,
    /// Ctrl+A: атрибуты целей по [`ATTRS`] (`None` — у целей разные) и выбранные.
    pub attrs_now: [Option<bool>; 4],
    pub attrs: [Option<bool>; 4],
    /// Ctrl+A: время изменения целей в поле (ISO); пусто — у целей разное.
    pub mtime_init: String,
}

/// Виды ссылки Alt+F6 (`ops.link kind=`) и подписи сегментов.
pub(super) const LINK_KINDS: [(&str, &str); 3] = [
    ("sym", "ui-link-sym"),
    ("hard", "ui-link-hard"),
    ("junction", "ui-link-junction"),
];

/// Вставки шаблона группового переименования (`screen-multirename.png`): что вставить
/// в поле и подпись.
pub(super) const MR_TOKENS: [(&str, &str); 5] = [
    ("[N]", "ui-mr-tok-name"),
    ("[E]", "ui-mr-tok-ext"),
    ("[C]", "ui-mr-tok-counter"),
    ("[Y][M][D]", "ui-mr-tok-date"),
    ("/", "ui-mr-tok-replace"),
];

/// Превью F7 (`screen-mkdirtree.png`): строки дерева — глубина, имя, «есть» (`Some(true)`),
/// «новая» (`Some(false)`) или неизвестно (глубже существующей папки, без чтения ФС).
pub(super) fn mkdir_tree(t: &Tab, many: bool, text: &str) -> Vec<(usize, String, Option<bool>)> {
    let names: Vec<&str> = if many {
        text.split(';')
            .map(str::trim)
            .filter(|n| !n.is_empty())
            .collect()
    } else {
        vec![text.trim()]
    };
    if !names.iter().any(|n| n.contains(['\\', '/'])) && names.len() < 2 {
        return Vec::new();
    }
    let here = |n: &str| {
        let low = n.to_lowercase();
        t.listing
            .entries
            .iter()
            .any(|e| e.kind.is_dir_like() && e.name.to_lowercase() == low)
    };
    let mut out: Vec<(usize, String, Option<bool>)> = Vec::new();
    for name in names {
        let mut parent_new = false;
        let parts: Vec<&str> = name.split(['\\', '/']).filter(|p| !p.is_empty()).collect();
        for (depth, part) in parts.iter().enumerate() {
            let state = if depth == 0 {
                Some(here(part))
            } else if parent_new {
                Some(false)
            } else {
                None
            };
            parent_new = state == Some(false);
            // Общие начала путей — одной строкой.
            let dup = out
                .iter()
                .rev()
                .take_while(|(d, _, _)| *d >= depth)
                .any(|(d, n, _)| *d == depth && n.eq_ignore_ascii_case(part));
            if !dup {
                out.push((depth, (*part).to_owned(), state));
            }
        }
    }
    out.truncate(MKDIR_ROWS);
    out
}

/// Строк превью F7 не больше.
pub(super) const MKDIR_ROWS: usize = 8;

/// Атрибуты строки Ctrl+A: бит `Attrs`, буква `ops.attrs`, подпись.
pub(super) const ATTRS: [(u32, char, &str); 4] = [
    (urfin_core::Attrs::READONLY, 'r', "ui-attr-readonly"),
    (urfin_core::Attrs::HIDDEN, 'h', "ui-attr-hidden"),
    (urfin_core::Attrs::SYSTEM, 's', "ui-attr-system"),
    (urfin_core::Attrs::ARCHIVE, 'a', "ui-attr-archive"),
];

/// Следующее значение флажка атрибута по клику: «разные» → да → нет → «разные»
/// (если у целей было по-разному), иначе да ↔ нет.
pub(super) fn next_attr(was: Option<bool>, now: Option<bool>) -> Option<bool> {
    match (now, was) {
        (None, _) | (Some(false), Some(_)) => Some(true),
        (Some(true), _) => Some(false),
        (Some(false), None) => None,
    }
}

/// Атрибуты целей активного таба и общее время изменения (ISO), если оно одно.
fn attrs_of(t: &Tab, fmt: &crate::fmt_cell::FmtCfg) -> ([Option<bool>; 4], String) {
    let Some(tg) = t.targets() else {
        return ([None; 4], String::new());
    };
    let items = &tg.items;
    let flags = ATTRS.map(|(bit, _, _)| {
        let on = items.iter().filter(|e| e.attrs.0 & bit != 0).count();
        match on {
            0 => Some(false),
            n if n == items.len() => Some(true),
            _ => None,
        }
    });
    let mut cell = crate::fmt_cell::Cell::default();
    if let Some(first) = items.first()
        && items.iter().all(|e| e.mtime == first.mtime)
    {
        fmt.write_iso(&mut cell, first.mtime);
    }
    (flags, cell.as_str().to_owned())
}

/// Ctrl+A: `ops.attrs` с изменёнными флажками и `ops.touch`, если время правили.
fn attrs_commands(o: &StripOpts, text: &str) -> Vec<Command> {
    let (mut set, mut clear) = (String::new(), String::new());
    for (k, (_, letter, _)) in ATTRS.iter().enumerate() {
        match (o.attrs[k], o.attrs_now[k]) {
            (Some(true), was) if was != Some(true) => set.push(*letter),
            (Some(false), was) if was != Some(false) => clear.push(*letter),
            _ => {}
        }
    }
    let mut out = Vec::with_capacity(3);
    if !set.is_empty() || !clear.is_empty() {
        let mut c = Command::new("ops.attrs");
        if !set.is_empty() {
            c = c.arg("set", set.as_str());
        }
        if !clear.is_empty() {
            c = c.arg("clear", clear.as_str());
        }
        out.push(c);
    }
    let t = text.trim();
    if !t.is_empty() && t != o.mtime_init {
        out.push(Command::new("ops.touch").arg("mtime", t));
    }
    out.push(Command::new("ui.cancel"));
    out
}

/// F7: одно имя или (флажок «несколько») по одному `ops.mkdir` на имя через `;`.
fn mkdir_commands(many: bool, text: &str) -> Vec<Command> {
    let names: Vec<&str> = if many {
        text.split(';')
            .map(str::trim)
            .filter(|n| !n.is_empty())
            .collect()
    } else {
        vec![text]
    };
    let mut out: Vec<Command> = names
        .into_iter()
        .map(|n| Command::new("ops.mkdir").arg("name", n))
        .collect();
    out.push(Command::new("ui.cancel"));
    out
}

/// Фильтр (Ctrl+I): сколько объектов останется видно с маской — папки остаются всегда.
fn filter_matches(t: &Tab, mask: &str) -> Option<(usize, usize)> {
    let m = urfin_core::Mask::parse(mask.trim()).ok()?;
    let all = &t.listing.entries;
    let n = all
        .iter()
        .filter(|e| e.kind.is_dir_like() || m.matches(&e.name))
        .count();
    Some((n, all.len()))
}

/// Выделение при переименовании: имя без расширения (у папки и `.файла` — всё).
fn stem_range(name: &str, dir: bool) -> std::ops::Range<usize> {
    match name.rfind('.') {
        Some(i) if i > 0 && !dir => 0..i,
        _ => 0..name.len(),
    }
}

/// Политики «Если файл есть» (`ops.copy conflict=`) и их подписи.
pub(super) const CONFLICT: [(&str, &str); 4] = [
    ("ask", "ui-strip-ifexists-ask"),
    ("overwrite", "ui-strip-ifexists-overwrite"),
    ("skip", "ui-strip-ifexists-skip"),
    ("newer", "ui-strip-ifexists-newer"),
];

impl StripOpts {
    /// Аргументы `ops.copy`/`ops.move` из опций.
    pub(super) fn apply(&self, mut c: Command) -> Command {
        if self.conflict != 0 {
            c = c.arg("conflict", CONFLICT[self.conflict % CONFLICT.len()].0);
        }
        if self.verify {
            c = c.arg("verify", "hash");
        }
        c
    }
}

/// Отметка группы по маске (`sel.add`/`sel.remove`): маска для подсветки совпавших
/// имён и их число в видимом списке.
pub(super) fn mask_matches(t: &Tab, mask: &str) -> Option<(urfin_core::Mask, usize)> {
    let m = urfin_core::Mask::parse(mask.trim()).ok()?;
    let n = t
        .listing
        .order
        .iter()
        .filter(|&&i| {
            t.listing
                .entries
                .get(i as usize)
                .is_some_and(|e| !e.kind.is_dir_like() && m.matches(&e.name))
        })
        .count();
    Some((m, n))
}

/// Встроенная строка черновика (D-04): F5/F6 — на всю ширину в две линии (куда; если
/// файл есть, даты, хэш, кнопки), F8 — с чертой `modified`, отметка группы — с числом
/// совпадений, групповое переименование — с превью; кнопки с клавишами.
#[allow(clippy::too_many_lines, reason = "строка черновика сверху вниз")]
#[allow(
    clippy::many_single_char_names,
    reason = "состояние, черновик, аргументы"
)]
#[allow(clippy::too_many_arguments, reason = "плоский вызов отрисовки")]
pub(super) fn draft_bar(
    s: &AppState,
    field: Option<&Field>,
    field_text: &str,
    permanent: bool,
    opts: &StripOpts,
    preview: Option<&MrPreview>,
    sync: Option<urfin_core::Side>,
    theme: &Theme,
    click: overlays::ClickAll<'_>,
    toggle: &dyn Fn(u8) -> overlays::Handler,
) -> Option<Div> {
    use super::strip::{Btn, Tone, button, check, check3, container};
    let d = s.ext.get::<UiState>()?.draft.as_ref()?;
    let i18n = urfin_i18n::current();
    let (count, bytes) = targets(s.active_tab());
    let mut a = FluentArgs::new();
    a.set("count", count);
    a.set("size", urfin_i18n::fmt::bytes_with(&i18n, bytes));
    let fallback = || {
        let key = format!("ui-draft-{}", d.cmd.replace('.', "-"));
        if i18n.has(&key) {
            i18n.get(&key)
        } else {
            i18n.get(&format!("cmd-{}", d.cmd.replace('.', "-")))
        }
    };
    let cancel = || {
        button(
            theme,
            "draft-cancel".into(),
            i18n.get("ui-button-cancel"),
            "Esc",
            Btn::Normal,
        )
        .on_click(click(vec![Command::new("ui.cancel")]))
    };
    let input = field.filter(|f| matches!(f.key, Key::Draft(_))).map(|f| {
        div()
            .flex_1()
            .min_w_0()
            .relative()
            .child(Input::new(&f.input))
            .child(f.probe())
    });
    let title = |text: String| {
        div()
            .flex_none()
            .whitespace_nowrap()
            .font_weight(gpui_kit::FontWeight::MEDIUM)
            .child(SharedString::from(text))
    };
    let commit = |extra: Option<(&str, &str)>| {
        let mut c = match Command::parse(&d.cmd) {
            Ok(c) => c.arg(&d.arg, field_text),
            Err(_) => Command::new("ui.cancel"),
        };
        if matches!(d.cmd.as_str(), "ops.copy" | "ops.move") {
            c = opts.apply(c);
        }
        if d.cmd == "ops.link" {
            c = c.arg("kind", LINK_KINDS[opts.link_kind % 3].0);
        }
        if let Some((k, v)) = extra {
            c = c.arg(k, v);
        }
        vec![c, Command::new("ui.cancel")]
    };
    match d.cmd.as_str() {
        "pkg.search" => Some(super::pkg::search_bar(
            s, field, input, field_text, theme, click,
        )),
        "ops.copy" if sync.is_some() => {
            let side = sync.unwrap_or(urfin_core::Side::Left);
            Some(sync_bar(side, &a, theme, click, field_text, opts))
        }
        "ops.copy" | "ops.move" => {
            let copy = d.cmd == "ops.copy";
            let (text, go) = if copy {
                (i18n.fmt("ui-draft-copy", &a), i18n.get("ui-button-copy"))
            } else {
                (i18n.fmt("ui-draft-move", &a), i18n.get("ui-button-move"))
            };
            let mut top = div().flex().items_center().gap(px(12.0)).child(title(text));
            if let Some(i) = input {
                top = top.child(i);
            }
            let (_, label) = CONFLICT[opts.conflict % CONFLICT.len()];
            let row = div()
                .flex()
                .items_center()
                .gap(px(20.0))
                .child(
                    div()
                        .id("strip-ifexists")
                        .flex()
                        .items_center()
                        .gap(px(8.0))
                        .text_size(px(12.5))
                        .child(SharedString::from(i18n.get("ui-strip-ifexists")))
                        .child(
                            div()
                                .relative()
                                .h(px(26.0))
                                .px(px(10.0))
                                .flex()
                                .items_center()
                                .gap(px(6.0))
                                .rounded(px(4.0))
                                .border_1()
                                .border_color(theme.c(if opts.dropdown {
                                    Role::AccentFill
                                } else {
                                    Role::StrokeControl
                                }))
                                .bg(theme.c(Role::LayerPanel))
                                .children(field.map(Field::probe_drop))
                                .child(SharedString::from(i18n.get(label)))
                                .child(
                                    div()
                                        .text_size(px(10.0))
                                        .text_color(theme.c(Role::TextSecondary))
                                        .child(SharedString::new_static("▾")),
                                ),
                        )
                        .on_click(toggle(0)),
                )
                // Перенос даты сохраняет всегда — флажок только у копирования.
                .when(copy, |d| {
                    d.child(check(
                        theme,
                        "strip-dates".into(),
                        true,
                        i18n.get("ui-strip-keep-dates"),
                        false,
                    ))
                })
                .child(
                    check(
                        theme,
                        "strip-verify".into(),
                        opts.verify,
                        i18n.get(if copy {
                            "ui-strip-verify"
                        } else {
                            "ui-strip-verify-move"
                        }),
                        true,
                    )
                    .on_click(toggle(1)),
                )
                .child(
                    div()
                        .ml(px(8.0))
                        .flex()
                        .gap(px(8.0))
                        .child(
                            button(theme, "draft-go".into(), go, "Enter", Btn::Primary)
                                .on_click(click(commit(None))),
                        )
                        .child(
                            button(
                                theme,
                                "draft-bg".into(),
                                i18n.get("ui-strip-background"),
                                "Shift+Enter",
                                Btn::Normal,
                            )
                            .on_click(click(commit(Some(("queue", "bg"))))),
                        )
                        .child(cancel()),
                );
            Some(
                container(theme, Tone::Plain, 16.0, 12.0)
                    .child(top)
                    .child(row),
            )
        }
        "ops.delete" => {
            let confirm = |perm: bool| {
                let mut c = Command::new("ops.delete").arg("confirmed", true);
                if perm {
                    c = c.arg("permanent", true);
                }
                vec![c, Command::new("ui.cancel")]
            };
            let text = if permanent {
                i18n.fmt("ui-draft-delete-permanent", &a)
            } else {
                i18n.fmt("ui-draft-delete", &a)
            };
            let mut row = div()
                .flex()
                .items_center()
                .gap(px(10.0))
                .child(
                    div()
                        .font_family("Segoe Fluent Icons")
                        .text_color(theme.c(Role::Modified))
                        .child(SharedString::new_static("\u{E74D}")),
                )
                .child(title(text))
                .child(
                    button(
                        theme,
                        "draft-delete".into(),
                        i18n.get("ui-button-delete"),
                        "Enter",
                        Btn::Danger,
                    )
                    .on_click(click(confirm(permanent))),
                );
            if !permanent {
                row = row.child(
                    button(
                        theme,
                        "draft-permanent".into(),
                        i18n.get("ui-button-permanent"),
                        "Shift+Enter",
                        Btn::Normal,
                    )
                    .on_click(click(confirm(true))),
                );
            }
            Some(container(theme, Tone::Danger, 14.0, 10.0).child(row.child(cancel())))
        }
        "ops.mkdir" => {
            let row = div()
                .flex()
                .items_center()
                .gap(px(10.0))
                .child(icon(theme, "\u{E8F4}"))
                .child(title(i18n.get("ui-strip-mkdir")))
                .children(input)
                .child(
                    button(
                        theme,
                        "draft-go".into(),
                        i18n.get("ui-button-create"),
                        "Enter",
                        Btn::Primary,
                    )
                    .on_click(click(mkdir_commands(opts.many, field_text))),
                )
                .child(cancel());
            let more = div()
                .flex()
                .items_center()
                .gap(px(16.0))
                .child(
                    check(
                        theme,
                        "strip-many".into(),
                        opts.many,
                        i18n.get("ui-strip-mkdir-many"),
                        true,
                    )
                    .on_click(toggle(2)),
                )
                .child(note(theme, i18n.get("ui-strip-mkdir-nested")));
            let mut bar = container(theme, Tone::Plain, 12.0, 12.0)
                .child(row)
                .child(more);
            let tree = mkdir_tree(s.active_tab(), opts.many, field_text);
            if !tree.is_empty() {
                let mut list = div()
                    .flex()
                    .flex_col()
                    .pt(px(6.0))
                    .border_t_1()
                    .border_color(theme.c(Role::StrokeDivider));
                for (depth, name, state) in tree {
                    #[allow(clippy::cast_precision_loss, reason = "глубина единицы")]
                    let indent = depth as f32 * 18.0;
                    list = list.child(
                        div()
                            .h(px(22.0))
                            .pl(px(indent))
                            .flex()
                            .items_center()
                            .gap(px(6.0))
                            .text_size(px(12.5))
                            .child(
                                div()
                                    .font_family("Segoe Fluent Icons")
                                    .text_color(theme.c(if state == Some(true) {
                                        Role::TextSecondary
                                    } else {
                                        Role::AccentText
                                    }))
                                    .child(SharedString::new_static("\u{E8B7}")),
                            )
                            .child(
                                div()
                                    .when(state != Some(true), |d| {
                                        d.font_weight(gpui_kit::FontWeight::MEDIUM)
                                    })
                                    .child(SharedString::from(name)),
                            )
                            .children(state.map(|on| {
                                note(
                                    theme,
                                    i18n.get(if on {
                                        "ui-mkdir-exists"
                                    } else {
                                        "ui-mkdir-new"
                                    }),
                                )
                                .text_size(px(11.0))
                            })),
                    );
                }
                bar = bar.child(list);
            }
            Some(bar)
        }
        "panel.filter" => {
            let count = filter_matches(s.active_tab(), field_text).map(|(n, all)| {
                let mut m = FluentArgs::new();
                m.set("count", n as u64);
                m.set("total", all as u64);
                i18n.fmt("ui-strip-filter-matches", &m)
            });
            let row = div()
                .flex()
                .items_center()
                .gap(px(10.0))
                .child(icon(theme, "\u{E71C}"))
                .child(title(i18n.get("ui-strip-filter")))
                .children(input)
                .children(count.map(|c| note(theme, c)));
            let buttons = div()
                .flex()
                .items_center()
                .gap(px(8.0))
                .child(
                    button(
                        theme,
                        "draft-go".into(),
                        i18n.get("ui-button-apply"),
                        "Enter",
                        Btn::Primary,
                    )
                    .on_click(click(commit(None))),
                )
                .child(
                    button(
                        theme,
                        "draft-reset".into(),
                        i18n.get("ui-button-reset"),
                        "",
                        Btn::Normal,
                    )
                    .on_click(click(vec![
                        Command::new("panel.filter").arg("mask", ""),
                        Command::new("ui.cancel"),
                    ])),
                )
                .child(cancel());
            Some(
                container(theme, Tone::Plain, 12.0, 12.0)
                    .child(row)
                    .child(buttons),
            )
        }
        "ops.attrs" => {
            let mut a = FluentArgs::new();
            a.set("count", count);
            let mut flags = div()
                .flex()
                .items_center()
                .gap(px(16.0))
                .child(icon(theme, "\u{E8EC}"))
                .child(title(i18n.fmt("ui-strip-attrs", &a)));
            for (k, (_, _, label)) in ATTRS.iter().enumerate() {
                #[allow(clippy::cast_possible_truncation, reason = "четыре флажка")]
                let id = 10 + k as u8;
                flags = flags.child(
                    check3(
                        theme,
                        format!("strip-attr-{k}"),
                        opts.attrs[k],
                        i18n.get(label),
                    )
                    .on_click(toggle(id)),
                );
            }
            let time = div()
                .flex()
                .items_center()
                .gap(px(10.0))
                .child(note(theme, i18n.get("ui-strip-attrs-mtime")))
                .child(div().w(px(200.0)).flex_none().children(input))
                .child(
                    button(
                        theme,
                        "strip-now".into(),
                        i18n.get("ui-strip-attrs-now"),
                        "",
                        Btn::Normal,
                    )
                    .on_click(toggle(20)),
                );
            let buttons = div()
                .flex()
                .items_center()
                .gap(px(8.0))
                .child(
                    button(
                        theme,
                        "draft-go".into(),
                        i18n.get("ui-button-apply"),
                        "Enter",
                        Btn::Primary,
                    )
                    .on_click(click(attrs_commands(opts, field_text))),
                )
                .child(cancel());
            Some(
                container(theme, Tone::Plain, 12.0, 12.0)
                    .child(flags)
                    .child(time)
                    .child(buttons),
            )
        }
        "ops.link" => {
            let t = s.active_tab();
            let (target, dir) = match t.current() {
                Some(Row::Entry { entry, .. }) => (
                    path_text(t).trim_end_matches('\\').to_owned() + "\\" + entry.name.as_str(),
                    entry.kind.is_dir_like(),
                ),
                _ => (path_text(t), true),
            };
            let row = div()
                .flex()
                .items_center()
                .gap(px(10.0))
                .child(icon(theme, "\u{E71B}"))
                .child(title(i18n.get("ui-strip-link")))
                .children(input)
                .child(note(theme, i18n.get("ui-strip-link-to")))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .h(px(30.0))
                        .px(px(8.0))
                        .flex()
                        .items_center()
                        .rounded(px(4.0))
                        .border_1()
                        .border_color(theme.c(Role::StrokeControl))
                        .bg(theme.c(Role::LayerPanel))
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .child(SharedString::from(target)),
                );
            let mut seg = div()
                .flex()
                .items_center()
                .rounded(px(4.0))
                .border_1()
                .border_color(theme.c(Role::StrokeControl));
            for (k, (_, label)) in LINK_KINDS.iter().enumerate() {
                // Жёсткая ссылка на папку невозможна.
                let enabled = !(dir && k == 1);
                let on = opts.link_kind % 3 == k;
                #[allow(clippy::cast_possible_truncation, reason = "три вида")]
                let id = 40 + k as u8;
                let mut b = div()
                    .id(SharedString::from(format!("link-kind-{k}")))
                    .h(px(28.0))
                    .px(px(12.0))
                    .flex()
                    .items_center()
                    .text_size(px(12.5))
                    .when(k > 0, |d| {
                        d.border_l_1().border_color(theme.c(Role::StrokeControl))
                    })
                    .when(on, |d| d.bg(theme.c(Role::FillCursor)))
                    .when(!enabled, |d| d.text_color(theme.c(Role::TextDisabled)))
                    .child(SharedString::from(i18n.get(label)));
                if enabled {
                    b = b.on_click(toggle(id));
                }
                seg = seg.child(b);
            }
            let more = div()
                .flex()
                .items_center()
                .gap(px(12.0))
                .child(note(theme, i18n.get("ui-strip-link-kind")))
                .child(seg)
                .child(icon(theme, "\u{E946}"))
                .child(note(theme, i18n.get("ui-strip-link-hint")))
                .child(
                    button(
                        theme,
                        "draft-go".into(),
                        i18n.get("ui-button-create"),
                        "Enter",
                        Btn::Primary,
                    )
                    .on_click(click(commit(None))),
                )
                .child(cancel());
            Some(
                container(theme, Tone::Plain, 16.0, 12.0)
                    .child(row)
                    .child(more),
            )
        }
        "ops.touch" => {
            let row = div()
                .flex()
                .items_center()
                .gap(px(10.0))
                .child(icon(theme, "\u{E823}"))
                .child(title(i18n.get("ui-strip-touch")))
                .child(div().w(px(200.0)).flex_none().children(input))
                .child(
                    button(
                        theme,
                        "strip-now".into(),
                        i18n.get("ui-strip-attrs-now"),
                        "",
                        Btn::Normal,
                    )
                    .on_click(toggle(20)),
                )
                .child(
                    button(
                        theme,
                        "draft-go".into(),
                        i18n.get("ui-button-apply"),
                        "Enter",
                        Btn::Primary,
                    )
                    .on_click(click(commit(None))),
                )
                .child(cancel());
            let more = div()
                .flex()
                .items_center()
                .gap(px(16.0))
                .child(check(
                    theme,
                    "touch-deep".into(),
                    false,
                    i18n.get("ui-strip-touch-deep"),
                    false,
                ))
                .child(note(theme, i18n.get("ui-strip-touch-hint")));
            Some(
                container(theme, Tone::Plain, 12.0, 12.0)
                    .child(row)
                    .child(more),
            )
        }
        MULTI_RENAME => {
            let summary = preview.map(|p| {
                let mut m = FluentArgs::new();
                if let Some(e) = &p.error {
                    m.set("error", e.clone());
                    i18n.fmt("ui-mr-pattern-error", &m)
                } else {
                    m.set("changed", p.changed);
                    m.set("problems", p.problems);
                    i18n.fmt("ui-strip-mr-summary", &m)
                }
            });
            let row = div()
                .flex()
                .items_center()
                .gap(px(10.0))
                .child(icon(theme, "\u{E70F}"))
                .child(title(i18n.get("ui-strip-mr")))
                .children(input)
                .children(summary.map(|t| note(theme, t)));
            let mut chips = div().flex().flex_wrap().items_center().gap(px(6.0));
            for (k, (token, label)) in MR_TOKENS.iter().enumerate() {
                #[allow(clippy::cast_possible_truncation, reason = "пять вставок")]
                let id = 30 + k as u8;
                chips =
                    chips.child(
                        div()
                            .id(SharedString::from(format!("mr-tok-{k}")))
                            .h(px(26.0))
                            .px(px(10.0))
                            .flex()
                            .items_center()
                            .gap(px(6.0))
                            .rounded(px(4.0))
                            .border_1()
                            .border_color(theme.c(Role::StrokeControl))
                            .bg(theme.c(Role::LayerPanel))
                            .text_size(px(12.0))
                            .child(div().font_family("Cascadia Mono").child(
                                SharedString::new_static(if *token == "/" { "" } else { token }),
                            ))
                            .child(
                                div()
                                    .text_color(theme.c(Role::TextSecondary))
                                    .child(SharedString::from(i18n.get(label))),
                            )
                            .on_click(toggle(id)),
                    );
            }
            let buttons = div()
                .flex()
                .items_center()
                .gap(px(8.0))
                .child(
                    button(
                        theme,
                        "draft-go".into(),
                        i18n.get("ui-button-rename"),
                        "Enter",
                        Btn::Primary,
                    )
                    .on_click(click(vec![
                        mr_command(field_text, true),
                        Command::new("ui.cancel"),
                    ])),
                )
                .child(cancel());
            Some(
                container(theme, Tone::Plain, 12.0, 10.0)
                    .child(row)
                    .child(chips)
                    .child(buttons),
            )
        }
        cmd => {
            let multi = cmd == MULTI_RENAME;
            let select = matches!(cmd, "sel.add" | "sel.remove");
            let mut row = div()
                .flex()
                .items_center()
                .gap(px(10.0))
                .child(title(fallback()));
            if let Some(i) = input {
                row = row.child(i);
            }
            let hint = if select {
                mask_matches(s.active_tab(), field_text).map(|(_, n)| {
                    let mut m = FluentArgs::new();
                    m.set("count", n as u64);
                    i18n.fmt("ui-strip-matches", &m)
                })
            } else if multi {
                preview.map(|p| {
                    let mut m = FluentArgs::new();
                    m.set("changed", p.changed);
                    m.set("problems", p.problems);
                    i18n.fmt("ui-strip-mr-summary", &m)
                })
            } else {
                None
            };
            if let Some(n) = hint {
                row = row.child(note(theme, n));
            }
            let ok = if select {
                i18n.get(if cmd == "sel.add" {
                    "ui-button-mark"
                } else {
                    "ui-button-unmark"
                })
            } else if multi {
                i18n.get("ui-button-rename")
            } else {
                i18n.get("ui-button-ok")
            };
            let go = if multi {
                vec![mr_command(field_text, true), Command::new("ui.cancel")]
            } else {
                commit(None)
            };
            row = row
                .child(
                    button(theme, "draft-go".into(), ok, "Enter", Btn::Primary).on_click(click(go)),
                )
                .child(cancel());
            Some(container(theme, Tone::Plain, 12.0, 10.0).child(row))
        }
    }
}

/// Синхронизация после сравнения (`screen-syncplan.png`): направление → / ← (⇄ и
/// «Удалять лишнее» — нет в ops), объём того, что скопировать, Синхронизировать (копия
/// с «Заменить, если новее»).
fn sync_bar(
    side: urfin_core::Side,
    a: &FluentArgs<'_>,
    theme: &Theme,
    click: overlays::ClickAll<'_>,
    field_text: &str,
    opts: &StripOpts,
) -> Div {
    use super::strip::{Btn, Tone, button, check, container};
    let i18n = urfin_i18n::current();
    let left = side == urfin_core::Side::Left;
    let arrow = if left { "→" } else { "←" };
    let mut seg = div()
        .flex()
        .items_center()
        .rounded(px(4.0))
        .border_1()
        .border_color(theme.c(Role::StrokeControl));
    for (k, (glyph, dir)) in [("→", Some("lr")), ("←", Some("rl")), ("⇄", None)]
        .into_iter()
        .enumerate()
    {
        let on = dir == Some(if left { "lr" } else { "rl" });
        let mut b = div()
            .id(SharedString::from(format!("sync-dir-{k}")))
            .w(px(38.0))
            .h(px(28.0))
            .flex()
            .items_center()
            .justify_center()
            .when(k > 0, |d| {
                d.border_l_1().border_color(theme.c(Role::StrokeControl))
            })
            .when(on, |d| d.bg(theme.c(Role::FillCursor)))
            .when(dir.is_none(), |d| d.text_color(theme.c(Role::TextDisabled)))
            .child(SharedString::new_static(glyph));
        if let Some(dir) = dir.filter(|_| !on) {
            b = b.on_click(click(vec![
                Command::new("ui.cancel"),
                Command::new("ops.sync").arg("dir", dir),
            ]));
        }
        seg = seg.child(b);
    }
    let what = format!("{arrow} {}", i18n.fmt("ui-strip-sync-what", a));
    let go = Command::new("ops.copy")
        .arg("dest", field_text)
        .arg("conflict", "newer");
    let go = opts.apply(go).arg("conflict", "newer");
    let row = div()
        .flex()
        .flex_wrap()
        .items_center()
        .gap(px(12.0))
        .child(
            div()
                .font_family("Segoe Fluent Icons")
                .text_color(theme.c(Role::AccentText))
                .child(SharedString::new_static("\u{E8AB}")),
        )
        .child(
            div()
                .font_weight(gpui_kit::FontWeight::MEDIUM)
                .child(SharedString::from(i18n.get("ui-strip-sync"))),
        )
        .child(SharedString::from(what))
        .child(seg)
        .child(check(
            theme,
            "sync-mirror".into(),
            false,
            i18n.get("ui-strip-sync-mirror"),
            false,
        ))
        .child(
            button(
                theme,
                "draft-go".into(),
                i18n.get("ui-strip-sync-go"),
                "Enter",
                Btn::Primary,
            )
            .on_click(click(vec![go, Command::new("ui.cancel")])),
        )
        .child(
            button(
                theme,
                "draft-cancel".into(),
                i18n.get("ui-button-cancel"),
                "Esc",
                Btn::Normal,
            )
            .on_click(click(vec![Command::new("ui.cancel")])),
        );
    container(theme, Tone::Plain, 16.0, 12.0).child(row)
}

/// Значок строки (Segoe Fluent Icons) цветом `secondary`.
fn icon(theme: &Theme, glyph: &'static str) -> Div {
    div()
        .flex_none()
        .font_family("Segoe Fluent Icons")
        .text_color(theme.c(Role::TextSecondary))
        .child(SharedString::new_static(glyph))
}

/// Пояснение в строке: мелко, `secondary`, в одну строку.
fn note(theme: &Theme, text: String) -> Div {
    div()
        .flex_none()
        .whitespace_nowrap()
        .text_size(px(12.0))
        .text_color(theme.c(Role::TextSecondary))
        .child(SharedString::from(text))
}

/// Поле правки пути панели `side` (если оно открыто).
pub(super) fn path_input(field: Option<&Field>, side: urfin_core::Side) -> Option<Div> {
    let f = field?;
    match &f.key {
        Key::Path(s) if *s == side => Some(
            div()
                .flex_1()
                .relative()
                .child(Input::new(&f.input))
                .child(f.probe()),
        ),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attr_click_cycles() {
        // Было «разные»: да → нет → снова «разные».
        assert_eq!(next_attr(None, None), Some(true));
        assert_eq!(next_attr(None, Some(true)), Some(false));
        assert_eq!(next_attr(None, Some(false)), None);
        // Было одно значение: да ↔ нет.
        assert_eq!(next_attr(Some(false), Some(false)), Some(true));
        assert_eq!(next_attr(Some(false), Some(true)), Some(false));
    }

    #[test]
    fn attrs_only_changed_flags_and_time() {
        let o = StripOpts {
            attrs_now: [Some(false), None, Some(true), Some(true)],
            attrs: [Some(true), None, Some(false), Some(true)],
            mtime_init: "2026-10-21 18:05".into(),
            ..StripOpts::default()
        };
        let c = attrs_commands(&o, "2026-10-21 18:05");
        assert_eq!(c.len(), 2, "{c:?}");
        assert_eq!(c[0].id.as_str(), "ops.attrs");
        assert_eq!(c[0].args.str("set"), Some("r"));
        assert_eq!(c[0].args.str("clear"), Some("s"));
        assert_eq!(c[1].id.as_str(), "ui.cancel");
        let c = attrs_commands(&StripOpts::default(), " 2026-01-02 ");
        assert_eq!(c[0].id.as_str(), "ops.touch");
        assert_eq!(c[0].args.str("mtime"), Some("2026-01-02"));
    }

    #[test]
    fn mkdir_many_splits_by_semicolon() {
        let c = mkdir_commands(true, "a; b\\c ;;");
        let names: Vec<_> = c.iter().filter_map(|c| c.args.str("name")).collect();
        assert_eq!(names, ["a", "b\\c"]);
        assert_eq!(
            mkdir_commands(false, "a;b")[0].args.str("name"),
            Some("a;b")
        );
    }

    #[test]
    fn rename_selects_stem() {
        assert_eq!(stem_range("CHANGELOG.md", false), 0..9);
        assert_eq!(stem_range(".gitignore", false), 0..10);
        assert_eq!(stem_range("v1.2", true), 0..4);
        assert_eq!(stem_range("noext", false), 0..5);
    }
}
