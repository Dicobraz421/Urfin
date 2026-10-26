//! Блоки команд (S6-21, best effort): `term.prev_prompt` / `term.next_prompt` — вид к
//! промпту выше или ниже верхнего края, `term.copy_output` — вывод последней
//! законченной команды в буфер, `term.find {pattern, back?}` — regex по скроллбэку с
//! выделением найденного. Метки — из интеграции shell (`A`, `B`, `D`, [`crate::manager::Block`]).
//!
//! Clink: своего скрипта не делаем. Clink перетирает `PROMPT` своим фильтром — наши
//! маркеры можно печатать из Lua-фильтра `clink.promptfilter` (~20 строк), если на это
//! будет спрос; до тех пор cmd с Clink работает без интеграции (тост S6-12).

use alacritty_terminal::term::search::RegexSearch;
use urfin_core::{Args, CmdError, CommandId, Cx, UiEvent};

use crate::TermState;

/// Состояние поиска: шаблон, автомат, начало последнего найденного.
#[derive(Clone, Debug)]
pub(crate) struct Find {
    pattern: String,
    regex: RegexSearch,
    last: Option<alacritty_terminal::index::Point>,
}

fn active(st: &TermState) -> Option<&crate::manager::Slot> {
    st.sessions.active().filter(|s| s.shared().is_some())
}

/// Абсолютные строки промптов (меток `A`) активного сеанса по возрастанию.
fn prompts(st: &TermState) -> Vec<i64> {
    active(st).map_or_else(Vec::new, |s| s.blocks.iter().map(|b| b.a.abs()).collect())
}

/// `term.prev_prompt` / `term.next_prompt`: промпт — к верху вида; ниже последнего —
/// вид к низу.
pub(crate) fn jump(st: &TermState, up: bool) {
    let Some(sh) = active(st).and_then(|s| s.shared().cloned()) else {
        return;
    };
    let top = sh.top_abs();
    let marks = prompts(st);
    let target = if up {
        marks.iter().rev().find(|&&a| a < top).copied()
    } else {
        marks.iter().find(|&&a| a > top).copied()
    };
    match target {
        Some(a) => sh.scroll_to_abs(a),
        None if !up => sh.scroll(crate::vt::select::ScrollBy::Bottom),
        None => {}
    }
}

/// Текст вывода последней законченной команды: строки после строки ввода (`B`) до
/// конца (`D`).
pub(crate) fn last_output(st: &TermState) -> Option<String> {
    let slot = active(st)?;
    let b = slot
        .blocks
        .iter()
        .rev()
        .find(|b| b.b.is_some() && b.d.is_some())?;
    let (start, end) = (b.b?.abs() + 1, b.d?.abs());
    Some(slot.shared()?.rows_text(start, end))
}

/// `term.copy_output`.
pub(crate) fn copy_output(cx: &mut Cx<'_>) {
    let Some(text) = cx.state.ext.get::<TermState>().and_then(last_output) else {
        return;
    };
    cx.ui(UiEvent::Ext {
        id: CommandId::from_static("clip.write"),
        args: Args::default().with("text", text),
    });
}

/// `term.find {pattern, back?}`: тот же шаблон — следующее совпадение от прошлого,
/// новый — с края буфера (назад — от низа). По умолчанию — назад, к старому выводу.
pub(crate) fn find(st: &mut TermState, a: &Args) -> Result<(), CmdError> {
    let pattern = a.str("pattern").unwrap_or_default().to_owned();
    if pattern.is_empty() {
        st.find = None;
        if let Some(sh) = active(st).and_then(|s| s.shared().cloned()) {
            sh.clear_selection();
        }
        return Ok(());
    }
    let back = a.bool("back").unwrap_or(true);
    let Some(sh) = active(st).and_then(|s| s.shared().cloned()) else {
        return Ok(());
    };
    let mut f = match st.find.take().filter(|f| f.pattern == pattern) {
        Some(f) => f,
        None => Find {
            regex: RegexSearch::new(&pattern).map_err(|e| CmdError::Arg {
                name: "pattern".into(),
                msg: e.to_string().into(),
            })?,
            pattern,
            last: None,
        },
    };
    let m = sh.find(&mut f.regex, back, f.last);
    f.last = m.map(|m| *m.start());
    st.find = Some(f);
    Ok(())
}
