//! Флаги командной строки (ADR-0022 §4): таблица [`FLAGS`] — по строке на флаг, разбор —
//! одной функцией [`parse`]. Аргументы без `--` — пути (табы); значения флагов путями не
//! считаются. Неизвестный `--флаг` пропускается.

/// Значение флага.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Takes {
    No,
    /// Следующий аргумент — значение.
    Value,
    /// Следующий аргумент, если он не `--…`.
    Optional,
}

/// Флаг: имя, значение, только в debug.
#[derive(Debug)]
pub(crate) struct Flag {
    pub name: &'static str,
    pub takes: Takes,
    pub debug_only: bool,
}

const fn flag(name: &'static str, takes: Takes) -> Flag {
    Flag {
        name,
        takes,
        debug_only: false,
    }
}

const fn debug(name: &'static str) -> Flag {
    Flag {
        name,
        takes: Takes::No,
        debug_only: true,
    }
}

/// Все флаги `urfin`.
pub(crate) const FLAGS: &[Flag] = &[
    // Отдельный экземпляр: пути не уходят запущенному.
    flag("--new-instance", Takes::No),
    // Без окна: Core, 1000 команд, shutdown, выход 0 (CI).
    flag("--smoke", Takes::No),
    // Окно, JSON с длительностями этапов старта после первого кадра и выход (S0 v5 №10).
    flag("--bench-startup", Takes::No),
    // Замер кадров на датасете (G-07): каталог, по умолчанию `gen-100000-s1`.
    flag("--bench", Takes::Optional),
    // Снимки сцен в каталог (проверка дизайна).
    flag("--screens", Takes::Value),
    // Паника для проверки crash-файла и баннера (S1-12).
    debug("--crash-test"),
    // Паника не в UI-потоке — ошибка в логе, не сбой (S1 §1).
    debug("--thread-panic-test"),
    // Ошибка в лог (и в журнал Windows).
    debug("--error-test"),
    // Первый экземпляр без окна печатает принятый запрос.
    debug("--ipc-test-server"),
];

/// Разобранная командная строка.
#[derive(Debug, Default)]
pub(crate) struct Cli {
    set: Vec<(&'static str, Option<String>)>,
    /// Пути: каждый — новый таб.
    pub paths: Vec<String>,
}

impl Cli {
    /// Флаг задан (debug-флаги в release не задаются).
    pub(crate) fn has(&self, name: &str) -> bool {
        self.set.iter().any(|(n, _)| *n == name)
    }

    /// Значение флага, если оно есть.
    pub(crate) fn value(&self, name: &str) -> Option<&str> {
        self.set
            .iter()
            .find(|(n, _)| *n == name)
            .and_then(|(_, v)| v.as_deref())
    }
}

/// Аргументы процесса (без имени exe) по таблице `flags`.
pub(crate) fn parse(args: &[String], flags: &[Flag]) -> Cli {
    let mut cli = Cli::default();
    let mut it = args.iter().peekable();
    while let Some(a) = it.next() {
        if !a.starts_with("--") {
            cli.paths.push(a.clone());
            continue;
        }
        let Some(f) = flags.iter().find(|f| f.name == a) else {
            continue;
        };
        let value = match f.takes {
            Takes::No => None,
            Takes::Value => it.next().cloned(),
            Takes::Optional => it.next_if(|v| !v.starts_with("--")).cloned(),
        };
        if !f.debug_only || cfg!(debug_assertions) {
            cli.set.push((f.name, value));
        }
    }
    cli
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn values_are_not_paths() {
        let cli = parse(
            &args(&[
                "x",
                "--screens",
                "out",
                "--bench",
                "--new-instance",
                "--what",
                "y",
            ]),
            FLAGS,
        );
        assert_eq!(cli.paths, ["x", "y"]);
        assert_eq!(cli.value("--screens"), Some("out"));
        assert!(cli.has("--bench") && cli.value("--bench").is_none());
        assert!(cli.has("--new-instance") && !cli.has("--what"));
        let cli = parse(&args(&["--bench", "data", "--smoke"]), FLAGS);
        assert_eq!(cli.value("--bench"), Some("data"));
        assert!(cli.paths.is_empty() && cli.has("--smoke"));
        assert_eq!(
            parse(&args(&["--crash-test"]), FLAGS).has("--crash-test"),
            cfg!(debug_assertions)
        );
    }
}
