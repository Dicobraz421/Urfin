//! Правка пользовательского слоя из раздела «Клавиатура» (решение 2026-11-05): план из
//! [`Edit`] и его применение к тексту `keymaps/user.toml` через `toml_edit` — комментарии,
//! порядок и оформление остаются. Диск — у handler (на `Lane::Bulk`, атомарно).

use compact_str::CompactString;
use smallvec::SmallVec;
use toml_edit::{Array, DocumentMut, InlineTable, Item, Table, TableLike, Value};

use crate::catalog::{clashes, live, place};
use crate::key::{Chord, Os};
use crate::keymap::{Binding, Keymap, norm};

/// Новый файл: шапка с подсказкой.
pub const USER_TEMPLATE: &str = "\
# Свои клавиши Urfin (your own key bindings). Поверх пресета: \"Сочетание\" = \"команда\",
# \"Сочетание\" = false снимает привязку. Секции — контексты: [context.panel], [context.viewer]…
# Раздел настроек «Клавиатура» пишет сюда же и сохраняет ваши комментарии.
format = 1
name = \"user\"
";

/// Что вызывает привязка: команды (`id арг=значение`) и `_repeat`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Target {
    pub cmds: SmallVec<[CompactString; 1]>,
    pub repeat: Option<bool>,
}

impl Target {
    pub fn of(b: &Binding) -> Target {
        Target {
            cmds: b.cmds.iter().map(|c| c.to_string().into()).collect(),
            repeat: b.repeat,
        }
    }

    /// Команда без аргументов.
    pub fn command(id: &str) -> Target {
        Target {
            cmds: std::iter::once(CompactString::from(id)).collect(),
            repeat: None,
        }
    }

    /// Ключ строки таблицы.
    pub fn key(&self) -> String {
        self.cmds.join("; ")
    }

    /// Значение TOML (форма S3 §2.2).
    fn value(&self) -> Value {
        let one = |i: usize, c: &str| -> Value {
            match self.repeat {
                Some(r) if i == 0 => {
                    let mut t = InlineTable::new();
                    t.insert("cmd", c.into());
                    t.insert("_repeat", r.into());
                    Value::InlineTable(t)
                }
                _ => c.into(),
            }
        };
        match self.cmds.as_slice() {
            [c] => one(0, c),
            list => {
                let mut a = Array::new();
                for (i, c) in list.iter().enumerate() {
                    a.push(one(i, c));
                }
                Value::Array(a)
            }
        }
    }
}

/// Одна правка файла.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Edit {
    /// `"chord" = target` в контексте: новая строка или новое значение прежней.
    Bind {
        context: CompactString,
        chord: Chord,
        target: Target,
    },
    /// `"chord" = false`: снять привязку нижнего слоя.
    Off {
        context: CompactString,
        chord: Chord,
    },
    /// Убрать строку файла (свою привязку или снятие).
    Forget {
        context: CompactString,
        chord: Chord,
    },
    /// Убрать все строки привязок; шапка и комментарии файла остаются.
    Clear,
}

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum EditError {
    /// Файл с ошибкой TOML не переписывается: потеряются правки пользователя.
    #[error("{0}")]
    Parse(String),
    /// `context` — не обычная таблица (`context = { … }`): правка только руками.
    #[error("[context] is not a plain table")]
    Layout,
    #[error("{0}")]
    Io(#[from] std::io::Error),
}

/// Таблицы контекстов файла: общая и оверлей текущей ОС.
fn roots<'d>(doc: &'d mut DocumentMut, os: Os) -> Vec<&'d mut dyn TableLike> {
    let os_name = match os {
        Os::Windows => "windows",
        Os::Macos => "macos",
        Os::Linux => "linux",
    };
    let mut out: Vec<&'d mut dyn TableLike> = Vec::new();
    let table = doc.as_table_mut();
    let mut overlay = None;
    let mut plain = None;
    for (k, v) in table.iter_mut() {
        match k.get() {
            "context" => plain = v.as_table_like_mut(),
            "os" => overlay = v.as_table_like_mut(),
            _ => {}
        }
    }
    if let Some(p) = plain {
        out.push(p);
    }
    if let Some(c) = overlay
        .and_then(|o| o.get_mut(os_name))
        .and_then(Item::as_table_like_mut)
        .and_then(|o| o.get_mut("context"))
        .and_then(Item::as_table_like_mut)
    {
        out.push(c);
    }
    out
}

/// Ключ секции `context` в таблице `root` (пробелы не важны).
fn section(root: &dyn TableLike, context: &str) -> Option<String> {
    let want = norm(context);
    root.iter()
        .map(|(k, _)| k)
        .find(|k| norm(k) == want)
        .map(str::to_owned)
}

/// Ключ строки с этим сочетанием (`ctrl+k` и `Ctrl+K` — одно).
fn line(sec: &dyn TableLike, chord: &Chord, os: Os) -> Option<String> {
    sec.iter()
        .map(|(k, _)| k)
        .find(|k| Chord::parse(k, os).is_ok_and(|c| c == *chord))
        .map(str::to_owned)
}

/// Убрать строку; опустевшая секция тоже убирается.
fn forget(doc: &mut DocumentMut, context: &str, chord: &Chord, os: Os) {
    for root in roots(doc, os) {
        let Some(name) = section(&*root, context) else {
            continue;
        };
        let Some(sec) = root.get_mut(&name).and_then(Item::as_table_like_mut) else {
            continue;
        };
        if let Some(key) = line(&*sec, chord, os) {
            sec.remove(&key);
        }
        if sec.is_empty() {
            root.remove(&name);
        }
    }
}

/// Записать значение строки: новая — в конец секции, прежняя — на своём месте, со своим
/// комментарием.
fn put(
    doc: &mut DocumentMut,
    context: &str,
    chord: &Chord,
    mut value: Value,
    os: Os,
) -> Result<(), EditError> {
    let keys = chord.display(os);
    // Строка уже есть (в том числе в оверлее ОС) — правим её.
    for root in roots(doc, os) {
        let Some(name) = section(&*root, context) else {
            continue;
        };
        let Some(sec) = root.get_mut(&name).and_then(Item::as_table_like_mut) else {
            continue;
        };
        if let Some(key) = line(&*sec, chord, os) {
            if let Some(old) = sec.get(&key).and_then(Item::as_value) {
                *value.decor_mut() = old.decor().clone();
            }
            sec.insert(&key, Item::Value(value));
            return Ok(());
        }
    }
    let table = doc.as_table_mut();
    let ctx = table.entry("context").or_insert_with(|| {
        let mut t = Table::new();
        t.set_implicit(true);
        Item::Table(t)
    });
    let Some(ctx) = ctx.as_table_mut() else {
        return Err(EditError::Layout);
    };
    let name = section(ctx, context).unwrap_or_else(|| context.to_owned());
    let sec = ctx
        .entry(&name)
        .or_insert_with(|| Item::Table(Table::new()));
    let Some(sec) = sec.as_table_like_mut() else {
        return Err(EditError::Layout);
    };
    sec.insert(keys.as_str(), Item::Value(value));
    Ok(())
}

/// Применить правки к тексту файла (пусто или нет файла — из [`USER_TEMPLATE`]).
pub fn apply(text: &str, edits: &[Edit], os: Os) -> Result<String, EditError> {
    let source = if text.trim().is_empty() {
        USER_TEMPLATE
    } else {
        text
    };
    let mut doc: DocumentMut = source
        .parse()
        .map_err(|e: toml_edit::TomlError| EditError::Parse(e.message().to_owned()))?;
    for e in edits {
        match e {
            Edit::Bind {
                context,
                chord,
                target,
            } => put(&mut doc, context, chord, target.value(), os)?,
            Edit::Off { context, chord } => put(&mut doc, context, chord, false.into(), os)?,
            Edit::Forget { context, chord } => forget(&mut doc, context, chord, os),
            Edit::Clear => {
                doc.as_table_mut().remove("context");
                doc.as_table_mut().remove("os");
            }
        }
    }
    Ok(doc.to_string())
}

/// Снять клавишу: своя строка убирается, привязка пресета закрывается `false`.
pub fn plan_unbind(km: &Keymap, context: &str, chord: &Chord) -> Vec<Edit> {
    let want = norm(context);
    let in_preset = km
        .preset_bindings()
        .iter()
        .any(|b| b.chord == *chord && norm(&b.src.context) == want);
    let e = if in_preset {
        Edit::Off {
            context: context.into(),
            chord: chord.clone(),
        }
    } else {
        Edit::Forget {
            context: context.into(),
            chord: chord.clone(),
        }
    };
    vec![e]
}

/// Что сделать при назначении клавиши.
#[derive(Debug)]
pub struct BindPlan<'a> {
    pub context: &'a str,
    pub chord: Chord,
    pub target: Target,
    /// Заменяемая клавиша строки: контекст и сочетание.
    pub old: Option<(&'a str, Chord)>,
    /// Конфликт подтверждён: снять клавишу с других команд.
    pub steal: bool,
}

/// Правки назначения клавиши.
pub fn plan_bind(km: &Keymap, p: &BindPlan<'_>) -> Vec<Edit> {
    let mut out = Vec::new();
    if let Some((ctx, old)) = &p.old
        && *old != p.chord
    {
        out.extend(plan_unbind(km, ctx, old));
    }
    if p.steal {
        let same = norm(p.context);
        for c in clashes(km, &p.chord, p.context, &p.target) {
            // В том же контексте новая строка сама перекрывает прежнюю.
            if norm(&c.context) != same {
                out.extend(plan_unbind(km, &c.context, &p.chord));
            }
        }
    }
    out.push(Edit::Bind {
        context: p.context.into(),
        chord: p.chord.clone(),
        target: p.target.clone(),
    });
    out
}

/// Правки сброса строки таблицы: клавиши строки в действующей раскладке и в пресете
/// сверяются, всё, чем файл их изменил, убирается.
pub fn plan_reset(km: &Keymap, key: &str, where_key: &str) -> Vec<Edit> {
    let of = |list: &[Binding]| -> Vec<(CompactString, Chord)> {
        live(list)
            .into_iter()
            .filter(|b| Target::of(b).key() == key && place(&b.src.context) == where_key)
            .map(|b| (b.src.context.clone(), b.chord.clone()))
            .collect()
    };
    let now = of(km.bindings());
    let was = of(km.preset_bindings());
    let same = |a: &(CompactString, Chord), b: &(CompactString, Chord)| {
        a.1 == b.1 && norm(&a.0) == norm(&b.0)
    };
    now.iter()
        .filter(|n| !was.iter().any(|w| same(n, w)))
        .chain(was.iter().filter(|w| !now.iter().any(|n| same(n, w))))
        .map(|(context, chord)| Edit::Forget {
            context: context.clone(),
            chord: chord.clone(),
        })
        .collect()
}

#[cfg(test)]
#[path = "edit_tests.rs"]
mod tests;
