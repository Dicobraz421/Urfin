//! Имена записей архива → путь внутри корня (S8 §3.2, §6, zip-slip).
//!
//! Нормализация лексическая, без ФС: `\` и `/` — разделители; пустые компоненты и `.`
//! отбрасываются; `..` снимает уровень и не поднимается выше корня; у первого компонента
//! отбрасываются префиксы `\\?\`, `\\.\` и буква диска (`C:`, `C:x` → `x`). Абсолютные
//! пути (`/abs`, `C:\x`) оказываются внутри корня. Ops при извлечении всё равно проверяет
//! цель (`..`, ADS, зарезервированные имена).

use std::ops::Range;

/// Компоненты имени `raw` после нормализации — диапазоны байт в `raw`, в `out` (буфер
/// очищается).
pub fn split(raw: &str, out: &mut Vec<Range<usize>>) {
    out.clear();
    let mut start = 0;
    let bytes = raw.as_bytes();
    for end in (0..=bytes.len()).filter(|&i| i == bytes.len() || is_sep(bytes[i])) {
        let mut r = start..end;
        start = end + 1;
        // Префиксы `\\?\`, `\\.\` и буква диска — только в начале пути (и подряд:
        // `C:C:x` → `x`), чтобы разбор был идемпотентен.
        while out.is_empty() {
            let c = raw[r.clone()].as_bytes();
            if c == b"?" {
                r.start = r.end;
            } else if c.len() >= 2 && c[0].is_ascii_alphabetic() && c[1] == b':' {
                r.start += 2;
                continue;
            }
            break;
        }
        match &raw[r.clone()] {
            "" | "." => {}
            ".." => {
                out.pop();
            }
            _ => out.push(r),
        }
    }
}

/// Компоненты имени как строки (тесты, отладка).
pub fn components(raw: &str) -> Vec<&str> {
    let mut v = Vec::new();
    split(raw, &mut v);
    v.into_iter().map(|r| &raw[r]).collect()
}

/// Разделитель пути внутри архива.
pub(crate) fn is_sep(b: u8) -> bool {
    b == b'/' || b == b'\\'
}

#[cfg(test)]
mod tests {
    use std::path::{Component, Path, PathBuf};

    use super::*;

    #[test]
    fn hostile_names_stay_inside_the_root() {
        assert_eq!(components("../x"), ["x"]);
        assert_eq!(components(r"C:\x"), ["x"]);
        assert_eq!(components("/abs"), ["abs"]);
        assert_eq!(components(r"\\?\C:\Windows\x"), ["Windows", "x"]);
        assert_eq!(components(r"\\.\pipe\x"), ["pipe", "x"]);
        assert_eq!(components("a/../../b"), ["b"]);
        assert_eq!(components("a/./b//c/"), ["a", "b", "c"]);
        assert_eq!(components(r"dir\sub/f.txt"), ["dir", "sub", "f.txt"]);
        assert_eq!(components("C:x"), ["x"]);
        assert_eq!(components("C:"), Vec::<&str>::new());
        assert_eq!(
            components("a/C:/b"),
            ["a", "C:", "b"],
            "диск — только в начале"
        );
        assert_eq!(components(".."), Vec::<&str>::new());
        assert_eq!(components(""), Vec::<&str>::new());
        assert_eq!(components("...").as_slice(), ["..."], "три точки — имя");
        assert_eq!(components("Отчёт/../лист.txt"), ["лист.txt"]);
    }

    /// Генератор xorshift: свойства — на тысячах случайных имён (`proptest` крейту не
    /// подключён, см. отчёт S8-24).
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }

        fn pick<'a>(&mut self, of: &[&'a str]) -> &'a str {
            of[(self.next() % of.len() as u64) as usize]
        }
    }

    const TOKENS: [&str; 16] = [
        "..", ".", "a", "b", "C:", "c:x", "/", "\\", "?", "", "ж", ":", "…", "a.b", "\\\\", "//",
    ];

    fn hostile(rng: &mut Rng) -> String {
        let n = 1 + rng.next() % 12;
        (0..n).map(|_| rng.pick(&TOKENS)).collect()
    }

    /// Модель: имя архива, приложенное к корню распаковки через разделитель, не выходит
    /// из него — ни `..`, ни корня, ни префикса диска; разбор идемпотентен. (`:` внутри
    /// имени — ADS — отвергает ops при извлечении, S8 §6.)
    #[test]
    fn any_name_joins_inside_the_root() {
        let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
        let root = PathBuf::from(r"R:\dest\root");
        for _ in 0..20_000 {
            let raw = hostile(&mut rng);
            let comps = components(&raw);
            let mut s = root.display().to_string();
            for c in &comps {
                assert!(
                    !c.is_empty() && *c != "." && *c != "..",
                    "{raw:?} → {comps:?}"
                );
                assert!(!c.bytes().any(is_sep), "{raw:?} → {comps:?}");
                s.push('\\');
                s.push_str(c);
            }
            if let Some(first) = comps.first() {
                let b = first.as_bytes();
                assert!(
                    !(b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':'),
                    "{raw:?} → {comps:?}"
                );
            }
            let p = PathBuf::from(s);
            assert!(p.starts_with(&root), "{raw:?} → {}", p.display());
            let tail = p.strip_prefix(&root).unwrap();
            assert!(
                Path::new(tail)
                    .components()
                    .all(|c| matches!(c, Component::Normal(_))),
                "{raw:?} → {}",
                p.display()
            );
            let joined = comps.join("/");
            let again = components(&joined);
            assert_eq!(again, comps, "{raw:?}");
        }
    }
}
