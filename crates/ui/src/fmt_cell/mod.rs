//! Тексты ячеек списка (S2-08): имя, расширение, тип, размер, даты, атрибуты — в
//! стековый [`CellText`], без кучи. Всё, что зависит от языка (единицы, шаблон даты,
//! «Файл {ext}»), разбирается один раз в [`FmtCfg`] при смене языка.

use std::fmt::{self, Write as _};

use crate::sys::{LocalTime, Zone};
use urfin_core::{Attrs, Entry, EntryKind};
use urfin_i18n::{FluentArgs, I18n};
use urfin_vfs::icons::{IconClass, icon_class};

/// Строка фиксированной ёмкости на стеке (как `arrayvec::ArrayString`). Не влезло —
/// обрезается по границе символа многоточием.
#[derive(Clone)]
pub struct CellText<const N: usize> {
    buf: [u8; N],
    len: usize,
    /// Последняя запись не влезла.
    truncated: bool,
}

impl<const N: usize> CellText<N> {
    pub const fn new() -> Self {
        Self {
            buf: [0; N],
            len: 0,
            truncated: false,
        }
    }

    pub fn as_str(&self) -> &str {
        // Пишем только целые символы — всегда UTF-8.
        std::str::from_utf8(&self.buf[..self.len]).unwrap_or_default()
    }

    pub fn clear(&mut self) {
        self.len = 0;
        self.truncated = false;
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn is_truncated(&self) -> bool {
        self.truncated
    }

    /// Дописать, сколько влезет; при обрезке последний символ — `…`.
    fn push_str_lossy(&mut self, s: &str) {
        if self.truncated {
            return;
        }
        if self.len + s.len() <= N {
            self.buf[self.len..self.len + s.len()].copy_from_slice(s.as_bytes());
            self.len += s.len();
            return;
        }
        self.truncated = true;
        let room = N.saturating_sub(self.len + ELLIPSIS.len());
        let mut cut = room.min(s.len());
        while !s.is_char_boundary(cut) {
            cut -= 1;
        }
        self.buf[self.len..self.len + cut].copy_from_slice(&s.as_bytes()[..cut]);
        self.len += cut;
        if self.len + ELLIPSIS.len() <= N {
            self.buf[self.len..self.len + ELLIPSIS.len()].copy_from_slice(ELLIPSIS.as_bytes());
            self.len += ELLIPSIS.len();
        }
    }
}

impl<const N: usize> Default for CellText<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> fmt::Write for CellText<N> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.push_str_lossy(s);
        Ok(())
    }
}

impl<const N: usize> fmt::Debug for CellText<N> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self.as_str(), f)
    }
}

impl<const N: usize> PartialEq<&str> for CellText<N> {
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == *other
    }
}

const ELLIPSIS: &str = "…";

/// Ячейка строки списка.
pub type Cell = CellText<128>;

/// Колонка (Full, Wide, Custom; S2 §2.1–2.3).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum ColKind {
    Name,
    Ext,
    /// «Папка», «Файл TOML», «Приложение», «Архив 7z».
    Type,
    Size,
    Mtime,
    /// Создан и последний доступ (Custom columns, S2-19).
    Ctime,
    Atime,
    /// `RHSDA…`, как колонка атрибутов Проводника.
    Attrs,
}

/// Шаблон Fluent с одним аргументом, разобранный на «до» и «после».
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Template {
    pre: String,
    post: String,
}

/// Метка аргумента при разборе шаблона: частная зона Unicode, в переводах её нет.
const HOLE: &str = "\u{E000}";

impl Template {
    fn new(i18n: &I18n, key: &'static str, arg: &'static str) -> Self {
        let mut args = FluentArgs::new();
        args.set(arg, HOLE);
        let text = i18n.fmt(key, &args);
        let (pre, post) = text.split_once(HOLE).unwrap_or((&text, ""));
        Self {
            pre: pre.to_owned(),
            post: post.to_owned(),
        }
    }

    fn write(&self, out: &mut impl fmt::Write, arg: impl fmt::Display) {
        let _ = write!(out, "{}{arg}{}", self.pre, self.post);
    }
}

/// Всё языковое для ячеек; строится при смене языка (`urfin_i18n::epoch`) и зоны.
#[derive(Clone, Debug)]
pub struct FmtCfg {
    decimal: char,
    units: [String; 7],
    /// Шаблон strftime: `fmt-datetime` или ISO (`[ui] date_format = "iso"`).
    datetime: String,
    zone: Zone,
    folder: String,
    file: String,
    app: String,
    link: String,
    file_ext: Template,
    archive: Template,
}

/// ISO-шаблон даты (`[ui] date_format = "iso"`).
const ISO: &str = "%Y-%m-%d %H:%M";

impl FmtCfg {
    /// `iso` — `[ui] date_format = "iso"`, иначе — по языку.
    pub fn new(i18n: &I18n, iso: bool, zone: Zone) -> Self {
        let units = urfin_i18n::fmt::UNIT_KEYS.map(|k| i18n.get(k));
        Self {
            decimal: i18n.num_fmt().decimal,
            units,
            datetime: if iso {
                ISO.to_owned()
            } else {
                i18n.get("fmt-datetime")
            },
            zone,
            folder: i18n.get("ui-type-folder"),
            file: i18n.get("ui-type-file"),
            app: i18n.get("ui-type-app"),
            link: i18n.get("ui-type-link"),
            file_ext: Template::new(i18n, "ui-type-file-ext", "ext"),
            archive: Template::new(i18n, "ui-type-archive", "ext"),
        }
    }

    /// Размер как [`urfin_i18n::fmt::bytes`], без кучи.
    pub fn write_size(&self, out: &mut impl fmt::Write, n: u64) {
        let units = [0, 1, 2, 3, 4, 5, 6].map(|i| self.units[i].as_str());
        let _ = urfin_i18n::fmt::write_bytes(out, n, self.decimal, &units);
    }

    /// Дата по шаблону в поясе ОС; `i64::MIN` и вне диапазона — ничего.
    /// Время в ISO (`2026-10-21 18:05`) — в поле, которое разбирает `ops.touch`.
    pub fn write_iso(&self, out: &mut Cell, mtime: i64) {
        if mtime == i64::MIN {
            return;
        }
        if let Some(t) = self.zone.local(mtime) {
            let _ = write_pattern(out, ISO, &t);
        }
    }

    pub fn write_datetime(&self, out: &mut Cell, mtime: i64) {
        if mtime == i64::MIN {
            return;
        }
        let Some(t) = self.zone.local(mtime) else {
            return;
        };
        if write_pattern(out, &self.datetime, &t).is_err() {
            // Неизвестный спецификатор в пользовательском шаблоне — ISO.
            out.clear();
            let _ = write_pattern(out, ISO, &t);
        }
    }
}

/// Текст ячейки `col` элемента `e` в `out` (очищается).
pub fn cell_text(col: ColKind, e: &Entry, cfg: &FmtCfg, out: &mut Cell) {
    out.clear();
    let dir = e.kind.is_dir_like();
    match col {
        ColKind::Name => {
            let _ = out.write_str(&e.name);
        }
        ColKind::Ext => {
            if let Some(ext) = ext(e) {
                let _ = out.write_str(ext);
            }
        }
        ColKind::Type => write_type(e, cfg, out),
        // У каталога размер пуст, пока не посчитан (S2 §2.1, S0 v2 №5).
        ColKind::Size if dir && !e.attrs.has(Attrs::SIZE_KNOWN) => {}
        ColKind::Size => cfg.write_size(out, e.size),
        ColKind::Mtime => cfg.write_datetime(out, e.mtime),
        ColKind::Ctime => cfg.write_datetime(out, e.ctime),
        ColKind::Atime => cfg.write_datetime(out, e.atime),
        ColKind::Attrs => write_attrs(e, out),
    }
}

/// Расширение файла (без точки), как в Проводнике: у `.gitignore` — `gitignore`;
/// у каталогов — нет.
fn ext(e: &Entry) -> Option<&str> {
    if e.kind.is_dir_like() {
        return None;
    }
    let dot = e.name.rfind('.')?;
    (dot + 1 < e.name.len()).then(|| &e.name[dot + 1..])
}

fn write_type(e: &Entry, cfg: &FmtCfg, out: &mut Cell) {
    let class = icon_class(&e.name, e.kind, e.attrs);
    match (class, ext(e)) {
        (IconClass::Dir | IconClass::DirLink, _) => {
            let _ = out.write_str(&cfg.folder);
        }
        (_, _) if e.kind == EntryKind::LinkBroken => {
            let _ = out.write_str(&cfg.link);
        }
        (IconClass::Exec, Some(x)) if x.eq_ignore_ascii_case("exe") => {
            let _ = out.write_str(&cfg.app);
        }
        (IconClass::Archive, Some(x)) => cfg.archive.write(out, Lower(x)),
        (_, Some(x)) => cfg.file_ext.write(out, Upper(x)),
        (_, None) => {
            let _ = out.write_str(&cfg.file);
        }
    }
}

/// Буквы атрибутов в порядке Проводника: R H S D A C E O L.
fn write_attrs(e: &Entry, out: &mut Cell) {
    let a = e.attrs;
    let flags = [
        (a.has(Attrs::READONLY), 'R'),
        (a.has(Attrs::HIDDEN), 'H'),
        (a.has(Attrs::SYSTEM), 'S'),
        (e.kind.is_dir_like(), 'D'),
        (a.has(Attrs::ARCHIVE), 'A'),
        (a.has(Attrs::COMPRESSED), 'C'),
        (a.has(Attrs::ENCRYPTED), 'E'),
        (a.has(Attrs::OFFLINE), 'O'),
        (a.has(Attrs::REPARSE), 'L'),
    ];
    for (on, c) in flags {
        if on {
            let _ = out.write_char(c);
        }
    }
}

/// Мини-strftime без кучи: `%Y %y %m %-m %d %-d %H %-H %I %-I %M %S %p %%`.
/// Неизвестный спецификатор — ошибка (вызывающий берёт ISO).
fn write_pattern(out: &mut impl fmt::Write, pattern: &str, t: &LocalTime) -> fmt::Result {
    let hour12 = match t.hour % 12 {
        0 => 12,
        h => h,
    };
    let mut chars = pattern.chars();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.write_char(c)?;
            continue;
        }
        let mut spec = chars.next().ok_or(fmt::Error)?;
        let pad = if spec == '-' {
            spec = chars.next().ok_or(fmt::Error)?;
            false
        } else {
            true
        };
        let two = |out: &mut dyn fmt::Write, v: u8| {
            if pad {
                write!(out, "{v:02}")
            } else {
                write!(out, "{v}")
            }
        };
        match spec {
            'Y' => write!(out, "{:04}", t.year)?,
            'y' => write!(out, "{:02}", t.year.rem_euclid(100))?,
            'm' => two(out, t.month)?,
            'd' => two(out, t.day)?,
            'H' => two(out, t.hour)?,
            'I' => two(out, hour12)?,
            'M' => two(out, t.minute)?,
            'S' => two(out, t.second)?,
            'p' => out.write_str(if t.hour < 12 { "AM" } else { "PM" })?,
            '%' => out.write_char('%')?,
            _ => return Err(fmt::Error),
        }
    }
    Ok(())
}

/// Верхний регистр без аллокации.
struct Upper<'a>(&'a str);

impl fmt::Display for Upper<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0
            .chars()
            .flat_map(char::to_uppercase)
            .try_for_each(|c| f.write_char(c))
    }
}

/// Нижний регистр без аллокации.
struct Lower<'a>(&'a str);

impl fmt::Display for Lower<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0
            .chars()
            .flat_map(char::to_lowercase)
            .try_for_each(|c| f.write_char(c))
    }
}

#[cfg(test)]
mod tests;
