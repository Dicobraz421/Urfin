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
    let local = loc.is_local();
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
            let value = urfin_vfs::volume::space(&loc.path).ok().map(|sp| {
                let root = urfin_vfs::volume::of(&loc.path)
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
pub fn zones(s: &urfin_core::AppState, fmt: &FmtCfg) -> [String; 3] {
    let i18n = urfin_i18n::current();
    // Настройки (D-08): файл конфига слева, «Применяется сразу» справа.
    if let Some(u) = s
        .ext
        .get::<crate::UiState>()
        .filter(|u| u.settings.is_some())
    {
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
        return [file, String::new(), i18n.get("ui-set-applies-now")];
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
            if !entry.kind.is_dir_like() {
                cell_text(ColKind::Size, entry, fmt, &mut cell);
                parts.push(cell.as_str().trim().to_owned());
            }
            cell_text(ColKind::Mtime, entry, fmt, &mut cell);
            parts.push(cell.as_str().trim().to_owned());
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
    [file, sel, free]
}

/// Каталог shell, если таб нелокальный (архив, sftp) и shell с интеграцией.
fn shell_elsewhere(s: &urfin_core::AppState) -> Option<String> {
    if s.active_tab().loc.is_local() {
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
