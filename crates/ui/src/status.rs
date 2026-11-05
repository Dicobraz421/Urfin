//! Статус-строка (D-02, №61): три зоны — свойства файла под курсором · выделение ·
//! свободно на томе активной панели. Свободное место читает задача Interactive при смене
//! каталога или листинга (после операций); в кадре I/O нет.

use urfin_core::{Cx, Gen, Lane, Location, Msg, Row};
use urfin_i18n::FluentArgs;

use crate::fmt_cell::{Cell, ColKind, FmtCfg, cell_text};
use crate::handler::ui_state;

/// Свободное место тома: для какого каталога и поколения листинга, и что прочитано.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FreeSpace {
    pub loc: Location,
    pub generation: Gen,
    /// Корень тома (`C:`) и свободные байты; `None` — ещё читается или не локальный.
    pub value: Option<(String, u64)>,
}

/// Запросить свободное место, если каталог или листинг активной панели сменились.
pub fn sync(cx: &mut Cx<'_>) {
    let t = cx.state.active_tab();
    let same = cx
        .state
        .ext
        .get::<crate::UiState>()
        .and_then(|u| u.free.as_ref())
        .is_some_and(|f| f.loc == t.loc && f.generation == t.listing.generation);
    if same {
        return;
    }
    let (loc, generation) = (t.loc.clone(), t.listing.generation);
    // Вкладка анализа места — свободно на томе её каталога.
    let disk = urfin_vfs::du::dvfs::localize(cx.state, &loc).unwrap_or_else(|| loc.clone());
    let local = disk.is_local();
    let st = ui_state(cx);
    st.free = Some(FreeSpace {
        loc: loc.clone(),
        generation,
        value: None,
    });
    if !local {
        return;
    }
    cx.rt
        .spawn(Lane::Interactive, "ui.free", cx.rt.token(), move |t| {
            let value = urfin_vfs::volume::space(&disk.path).ok().map(|sp| {
                let root = urfin_vfs::volume::of(&disk.path)
                    .map(|v| v.root.display().to_string())
                    .unwrap_or_default();
                (root.trim_end_matches(['\\', '/']).to_owned(), sp.free)
            });
            let _ = t.sink.send(
                Box::new(FreeRead {
                    loc,
                    generation,
                    value,
                }),
                &t.cancel,
            );
        });
}

struct FreeRead {
    loc: Location,
    generation: Gen,
    value: Option<(String, u64)>,
}

impl Msg for FreeRead {
    fn name(&self) -> &'static str {
        "ui.free"
    }

    fn apply(self: Box<Self>, cx: &mut Cx<'_>) {
        if let Some(f) = ui_state(cx).free.as_mut()
            && f.loc == self.loc
            && f.generation == self.generation
        {
            f.value = self.value;
        }
    }
}

/// Тексты трёх зон: файл под курсором, выделение, свободное место.
#[allow(
    clippy::too_many_lines,
    reason = "зоны слева направо, особые случаи рядом"
)]
pub fn zones(s: &urfin_core::AppState, fmt: &FmtCfg) -> [String; 3] {
    let i18n = urfin_i18n::current();
    // Настройки (D-08): файл конфига слева, «Применяется сразу» справа.
    if let Some(u) = s
        .ext
        .get::<crate::UiState>()
        .filter(|u| u.settings.is_some())
    {
        // «Подключения» (F03-05): файл, сколько профилей и откуда, «Пароли не пишутся».
        if u.settings.as_deref() == Some("conn") {
            return crate::ssh::conn_status(s);
        }
        // «Политики» (ADR-0020): источники, сколько задано, только чтение.
        if u.settings.as_deref() == Some("policies") {
            let set = s.ext.get::<urfin_core::PolicySet>();
            let mut a = urfin_i18n::FluentArgs::new();
            a.set("set", set.map_or(0, |p| p.iter().count()));
            a.set("total", urfin_core::PolicyKey::ALL.len());
            return [
                r"policy.toml + HKLM\Software\Policies\Aahz\Urfin".into(),
                i18n.fmt("ui-set-policy-count", &a),
                i18n.get("ui-set-read-only"),
            ];
        }
        let file = u
            .config_dir
            .as_ref()
            .map_or_else(String::new, |d| d.join("config.toml").display().to_string());
        // «SSH» (F03-14): сколько строк раздела задано политикой.
        let locked = if u.settings.as_deref() == Some("ssh") {
            crate::ssh::policy_note(s)
        } else {
            String::new()
        };
        return [file, locked, i18n.get("ui-set-applies-now")];
    }
    // Терминал на весь экран пишется (F03 §3.5): запись одной группой слева (№92).
    if s.ext
        .get::<crate::UiState>()
        .is_some_and(|u| u.panels_hidden)
        && let Some(r) = s
            .ext
            .get::<urfin_term::TermState>()
            .and_then(urfin_term::TermState::active_rec)
    {
        return [crate::term_rec::status(r), String::new(), String::new()];
    }
    let t = s.active_tab();
    let mut cell = Cell::default();
    let file = match t.current() {
        Some(Row::Entry { entry, .. })
            if let Some(l) = s.ext.get::<crate::UiState>().and_then(|u| {
                u.links[usize::from(s.active == urfin_core::Side::Right)]
                    .items
                    .get(&entry.name)
            }) =>
        {
            // Ссылка (D-03): цель — только здесь; битая — «цель не найдена».
            let mut out = format!("{} → {}", entry.name, l.target.as_deref().unwrap_or("?"));
            if l.broken {
                out.push_str(" · ");
                out.push_str(&i18n.get("ui-link-broken"));
            }
            out
        }
        Some(Row::Entry { entry, .. }) => {
            let mut parts = vec![entry.name.to_string()];
            // Размер каталога — когда посчитан (анализ места, F3 на папке).
            if !entry.kind.is_dir_like() || entry.attrs.has(urfin_core::Attrs::SIZE_KNOWN) {
                cell_text(ColKind::Size, entry, fmt, &mut cell);
                parts.push(cell.as_str().trim().to_owned());
            }
            if crate::du_view::partial(entry) {
                // «target · ≥ 15,1 ГБ · считается» (`screen-duscan`).
                parts.push(i18n.get("du-counting"));
            } else {
                cell_text(ColKind::Mtime, entry, fmt, &mut cell);
                parts.push(cell.as_str().trim().to_owned());
            }
            parts.retain(|p| !p.is_empty());
            parts.join(" · ")
        }
        _ => String::new(),
    };
    let mut args = FluentArgs::new();
    let total = t.listing.stats.files + t.listing.stats.dirs;
    args.set("count", u64::from(t.selection.len()));
    args.set("total", u64::from(total));
    let failed = matches!(t.listing.status, urfin_core::ListingStatus::Failed(_));
    let sel = if failed {
        String::new()
    } else if t.selection.is_empty() {
        i18n.fmt("ui-status-selected", &args)
    } else {
        args.set(
            "bytes",
            urfin_i18n::fmt::bytes_with(&i18n, t.selection.bytes()),
        );
        i18n.fmt("ui-status-selected-bytes", &args)
    };
    let free = s
        .ext
        .get::<crate::UiState>()
        .and_then(|u| u.free.as_ref())
        .and_then(|f| f.value.as_ref())
        .map(|(root, bytes)| {
            let mut a = FluentArgs::new();
            a.set("root", root.clone());
            a.set("bytes", urfin_i18n::fmt::bytes_with(&i18n, *bytes));
            i18n.fmt("ui-status-free", &a)
        })
        .unwrap_or_default();
    // Таб на архиве или sftp: shell — в ближайшем локальном каталоге, а не «внутри»
    // (S6 §5.1) — показать, где он.
    let free = match shell_elsewhere(s) {
        Some(dir) => {
            let mut a = FluentArgs::new();
            a.set("dir", dir);
            let mark = i18n.fmt("ui-status-shell-cwd", &a);
            if free.is_empty() {
                mark
            } else {
                format!("{mark} · {free}")
            }
        }
        None => free,
    };
    // Вкладка анализа места, посчитанная сейчас: «посчитано за 1 мин 08 с» (`screen-dulist`).
    let sel = match crate::du_view::info(s, t) {
        Some(i) if !i.scanning && !i.from_cache && i.scanned_at != 0 => {
            crate::du_view::took(i.took_ms)
        }
        _ => sel,
    };
    // Виды фильтров (F01-08): у группы «Типов» — её расширения, в середине — «7 групп ·
    // N файлов» / «Найдено N · X» (без отметки).
    let (du_file, du_sel) = crate::du_view::status(s, t);
    let file = du_file.unwrap_or(file);
    // У «Дубликатов» отмеченное — в строке внизу вида, в статусе — «N групп · M файлов».
    let sel = match du_sel {
        Some(d)
            if t.selection.is_empty()
                || crate::du_view::filter(t) == Some(urfin_vfs::du::DuFilter::Dups) =>
        {
            d
        }
        _ => sel,
    };
    // Быстрый просмотр (S7 §4, `screen-quickview`): в середине — строки его экрана.
    let sel = quick_lines(s).unwrap_or(sel);
    // Вкладка пакетов (F02-08): пакет под курсором и время проверки.
    if let Some(z) = crate::pkg_view::status(s, t, fmt) {
        return z;
    }
    // Панель `sftp://` (F03-06, №92): справа — сеанс одной группой.
    let free = crate::ssh::panel_status(s, t).unwrap_or(free);
    [file, sel, free]
}

/// «стр 1–26 из 41»: строки на экране документа в области панели (быстрый просмотр,
/// `viewer.dock`); всего — по индексу, иначе оценка «≈N».
fn quick_lines(s: &urfin_core::AppState) -> Option<String> {
    let v = s.ext.get::<urfin_view::ViewState>()?;
    let d = [urfin_core::Side::Left, urfin_core::Side::Right]
        .into_iter()
        .filter(|side| s.panel(*side).visible && urfin_view::panel_shows(s, *side))
        .find_map(|side| v.in_panel(side))?;
    let (first, last) = urfin_view::visible_lines(d)?;
    let mut a = FluentArgs::new();
    let total = urfin_view::line_count(d).map_or_else(
        || format!("≈{}", d.index.line_of(d.len).get() + 1),
        |n| n.to_string(),
    );
    a.set("from", first + 1);
    a.set(
        "to",
        (last + 1)
            .min(urfin_view::line_count(d).unwrap_or(u64::MAX))
            .max(first + 1),
    );
    a.set("total", total);
    Some(urfin_i18n::current().fmt("ui-qv-lines", &a))
}

/// Каталог shell, если таб нелокальный (архив, sftp) и shell с интеграцией.
fn shell_elsewhere(s: &urfin_core::AppState) -> Option<String> {
    // Вкладка анализа места — те же папки на диске; у вкладки пакетов файлов нет.
    if s.active_tab().loc.is_local()
        || crate::du_view::is_du(s.active_tab())
        || crate::pkg_view::is_pkg(s.active_tab())
    {
        return None;
    }
    let slot = s
        .ext
        .get::<urfin_term::TermState>()?
        .sessions
        .active()
        .filter(|a| a.integrated)?;
    Some(slot.cwd.as_ref()?.display().to_string())
}
