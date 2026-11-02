//! Сравнение версий пакетов (F02 §2.4): сегменты через `.` `-` `+` `_`, числа — как
//! числа, остальное — лексикографически, пре-релиз (`-beta`, `-rc1`) ниже релиза.
//! `Unknown`, `< x`, `> x` и строки без цифр не сравниваются.

use std::cmp::Ordering;
use std::fmt;

use compact_str::CompactString;

/// Сегмент версии. Буквы ниже любого числа: `1.0-rc1` < `1.0`, `1.0.1` > `1.0-beta`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Seg {
    /// Буквы, в нижнем регистре.
    Text(CompactString),
    /// Цифры без ведущих нулей; сравнение — по длине, затем по тексту (без переполнения).
    Num(NumStr),
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct NumStr(CompactString);

impl Ord for NumStr {
    fn cmp(&self, other: &Self) -> Ordering {
        self.0
            .len()
            .cmp(&other.0.len())
            .then_with(|| self.0.cmp(&other.0))
    }
}

impl PartialOrd for NumStr {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

static ZERO: Seg = Seg::Num(NumStr(CompactString::const_new("0")));

/// Версия, которую можно сравнивать. Хвостовые нули не значат: `1.0` == `1.0.0` == `1`.
#[derive(Clone, PartialEq, Eq)]
pub struct Version {
    /// Без хвостовых `0`: сравнение дополняет короткую нулями.
    segs: Vec<Seg>,
}

impl Version {
    /// `None` — версию нельзя сравнивать (`Unknown`, `< 1.2`, без цифр).
    pub fn parse(text: &str) -> Option<Version> {
        let t = text.trim();
        if t.starts_with(['<', '>']) || !t.contains(|c: char| c.is_ascii_digit()) {
            return None;
        }
        let t = match t.strip_prefix(['v', 'V']) {
            Some(rest) if rest.starts_with(|c: char| c.is_ascii_digit()) => rest,
            _ => t,
        };
        let mut segs = Vec::new();
        for part in t.split(['.', '-', '+', '_']) {
            split_alnum(part, &mut segs);
        }
        while segs.last() == Some(&ZERO) {
            segs.pop();
        }
        Some(Version { segs })
    }
}

/// Части сегмента по границе букв и цифр: `rc10` → `rc`, `10`. Прочие символы — границы.
fn split_alnum(part: &str, out: &mut Vec<Seg>) {
    let mut cur = String::new();
    let mut digits = false;
    let flush = |cur: &mut String, digits: bool, out: &mut Vec<Seg>| {
        if cur.is_empty() {
            return;
        }
        let seg = if digits {
            let n = cur.trim_start_matches('0');
            Seg::Num(NumStr(if n.is_empty() { "0" } else { n }.into()))
        } else {
            Seg::Text(cur.to_lowercase().into())
        };
        out.push(seg);
        cur.clear();
    };
    for c in part.chars() {
        if !c.is_alphanumeric() {
            flush(&mut cur, digits, out);
            continue;
        }
        let d = c.is_ascii_digit();
        if !cur.is_empty() && d != digits {
            flush(&mut cur, digits, out);
        }
        digits = d;
        cur.push(c);
    }
    flush(&mut cur, digits, out);
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        let n = self.segs.len().max(other.segs.len());
        (0..n)
            .map(|i| {
                let a = self.segs.get(i).unwrap_or(&ZERO);
                let b = other.segs.get(i).unwrap_or(&ZERO);
                a.cmp(b)
            })
            .find(|o| o.is_ne())
            .unwrap_or(Ordering::Equal)
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl fmt::Debug for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let parts: Vec<&str> = self
            .segs
            .iter()
            .map(|s| match s {
                Seg::Text(t) | Seg::Num(NumStr(t)) => t.as_str(),
            })
            .collect();
        write!(f, "Version({})", parts.join("."))
    }
}

/// Сравнение текстов версий; `None` — хотя бы одна не сравнивается.
pub fn compare(a: &str, b: &str) -> Option<Ordering> {
    Some(Version::parse(a)?.cmp(&Version::parse(b)?))
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    #[test]
    fn order() {
        use Ordering::{Equal, Greater, Less};
        let cases = [
            ("1.10", "1.9", Greater),
            ("1.0", "1.0.0", Equal),
            ("1", "1.0.0.0", Equal),
            ("24.08", "24.8", Equal),
            ("1.0-beta", "1.0", Less),
            ("1.0-rc1", "1.0-rc2", Less),
            ("1.0-rc10", "1.0-rc2", Greater),
            ("1.0-alpha", "1.0-beta", Less),
            ("1.0.1", "1.0-beta", Greater),
            ("1.0.0.1", "1", Greater),
            ("v2.47.0", "2.47.0", Equal),
            ("2.47.0.windows.2", "2.47.0.windows.1", Greater),
            ("2.47.0.2", "2.47.0", Greater),
            ("132.0", "131.0.2", Greater),
            ("14.42.34433.0", "14.40.33810.0", Greater),
            ("3.12.10", "3.12.8", Greater),
            ("20.18.0", "20.18.0", Equal),
            ("1_2", "1.2", Equal),
            ("1.2+build5", "1.2+build4", Greater),
            (
                "99999999999999999999999.1",
                "99999999999999999999998.9",
                Greater,
            ),
            ("1.0RC", "1.0rc", Equal),
        ];
        for (a, b, want) in cases {
            assert_eq!(compare(a, b), Some(want), "{a} vs {b}");
            assert_eq!(compare(b, a), Some(want.reverse()), "{b} vs {a}");
        }
    }

    #[test]
    fn incomparable() {
        for v in [
            "Unknown", "unknown", "< 1.2.3", "> 4.0", "", "  ", "latest", "-",
        ] {
            assert!(Version::parse(v).is_none(), "{v:?}");
            assert_eq!(compare(v, "1.0"), None, "{v:?}");
        }
    }

    fn version_text() -> impl Strategy<Value = String> {
        proptest::string::string_regex("v?[0-9a-cA-C]{0,3}([.+_-][0-9a-c]{0,3}){0,4}").unwrap()
    }

    proptest! {
        /// Антисимметрия и согласованность с `Eq`.
        #[test]
        fn antisymmetric(a in version_text(), b in version_text()) {
            if let (Some(x), Some(y)) = (Version::parse(&a), Version::parse(&b)) {
                prop_assert_eq!(x.cmp(&y), y.cmp(&x).reverse());
                prop_assert_eq!(x.cmp(&y) == Ordering::Equal, x == y);
            }
        }

        /// Транзитивность: a ≤ b ≤ c ⇒ a ≤ c.
        #[test]
        fn transitive(a in version_text(), b in version_text(), c in version_text()) {
            if let (Some(x), Some(y), Some(z)) =
                (Version::parse(&a), Version::parse(&b), Version::parse(&c))
            {
                if x <= y && y <= z {
                    prop_assert!(x <= z);
                }
                if x >= y && y >= z {
                    prop_assert!(x >= z);
                }
                if x == y && y == z {
                    prop_assert!(x == z);
                }
            }
        }
    }
}
