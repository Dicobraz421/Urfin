//! Запись сеанса терминала без gpui (F03 §3.5, `screen-sshrec`): какая запись видна,
//! время чипа, строка статуса одной группой (№92), клавиша в keybar (№88). Данные —
//! `TermState` (`urfin-term`); рисуют `gpui_shell::term_view` и `gpui_shell::ssh_pair`.

use std::time::Duration;

use urfin_core::AppState;
use urfin_i18n::FluentArgs;
use urfin_term::rec::Recording;

/// Запись терминала на весь экран: пары SSH (её сеанс), иначе активного сеанса.
pub fn visible(s: &AppState) -> Option<&Recording> {
    crate::ssh_pair::rec(s).or_else(|| s.ext.get::<urfin_term::TermState>()?.active_rec())
}

/// Время чипа: `04:12`, с часа — `1:04:12`.
pub fn clock(d: Duration) -> String {
    let t = d.as_secs();
    let (h, m, sec) = (t / 3600, t / 60 % 60, t % 60);
    if h > 0 {
        format!("{h}:{m:02}:{sec:02}")
    } else {
        format!("{m:02}:{sec:02}")
    }
}

/// «Запись 04:12».
pub fn chip_text(r: &Recording) -> String {
    let mut a = FluentArgs::new();
    a.set("time", clock(r.elapsed()));
    urfin_i18n::current().fmt("ui-term-rec-chip", &a)
}

/// «обязательна · политика · TOML» — обязательная по политике.
pub fn required_text(r: &Recording) -> Option<String> {
    let src = r.required?;
    let mut a = FluentArgs::new();
    a.set("source", src.label());
    Some(urfin_i18n::current().fmt("ui-term-rec-required", &a))
}

/// Статус одной группой: «Запись в %LOCALAPPDATA%\…\srv1-2026-10-23-1402.cast ·
/// asciicast v2, 1,2 МБ»; файл ещё не создан — «Запись · asciicast v2».
pub fn status(r: &Recording) -> String {
    let i18n = urfin_i18n::current();
    match r.shared.shown() {
        Some(path) => {
            let mut a = FluentArgs::new();
            a.set("path", path);
            a.set("size", urfin_i18n::fmt::bytes(r.shared.bytes()));
            i18n.fmt("ui-term-rec-status", &a)
        }
        None => i18n.get("ui-term-rec-starting"),
    }
}

/// Ctrl+Alt+R в keybar: команда доступна и запись не обязательна (стоп недоступен).
pub fn key(s: &AppState, ssh: bool) -> Option<(String, String)> {
    let st = s.ext.get::<urfin_term::TermState>()?;
    let rec = if ssh {
        crate::ssh_pair::rec(s)
    } else {
        st.active_rec()
    };
    if rec.is_some_and(|r| r.required.is_some()) || !st.record_allowed(ssh) {
        return None;
    }
    Some((
        "Ctrl+Alt+R".to_owned(),
        urfin_i18n::current().get("ui-keys-term-rec"),
    ))
}

/// До смены секунды чипа (перерисовка).
pub fn next_tick(r: &Recording) -> Duration {
    let ms = u64::try_from(r.elapsed().as_millis() % 1000).unwrap_or(0);
    Duration::from_millis(1000 - ms + 5)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clock_and_status() {
        urfin_i18n::set_language("ru");
        assert_eq!(clock(Duration::from_secs(252)), "04:12");
        assert_eq!(clock(Duration::from_secs(3852)), "1:04:12");
        let r = Recording::demo(
            r"%LOCALAPPDATA%\Urfin\recordings\srv1-2026-10-23-1402.cast",
            1_258_291,
            Duration::from_secs(252),
            Some(urfin_core::PolicySource::Toml),
        );
        assert_eq!(chip_text(&r), "Запись 04:12");
        assert_eq!(
            required_text(&r).as_deref(),
            Some("обязательна · политика · TOML")
        );
        let st = status(&r);
        assert!(
            st.starts_with(
                r"Запись в %LOCALAPPDATA%\Urfin\recordings\srv1-2026-10-23-1402.cast · asciicast v2, 1,2"
            ),
            "{st}"
        );
    }
}
