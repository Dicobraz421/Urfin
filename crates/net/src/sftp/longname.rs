//! `longname` SFTP v3 (F03 §2.6): имена владельца и группы из строки вида `ls -l`;
//! при неудаче — `uid`/`gid` числом. Права — `mode` в виде `drwxr-xr-x`.

/// Владелец и группа для колонки «Владелец».
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Owner {
    pub user: String,
    pub group: String,
}

/// Владелец из `longname`: `perms nlink owner group size month day time|year name`.
/// Имена — любые не-пробельные (юникод); `NT AUTHORITY\SYSTEM` (сервер OpenSSH на
/// Windows) — одинаковые половины. Иначе — `uid`/`gid` числом (пусто, если их нет).
pub fn owner(longname: &str, filename: &str, uid: Option<u32>, gid: Option<u32>) -> Owner {
    parse(longname, filename).unwrap_or_else(|| Owner {
        user: uid.map(|u| u.to_string()).unwrap_or_default(),
        group: gid.map(|g| g.to_string()).unwrap_or_default(),
    })
}

fn parse(longname: &str, filename: &str) -> Option<Owner> {
    let head = longname
        .trim_end()
        .strip_suffix(filename)
        .unwrap_or(longname);
    let t: Vec<&str> = head.split_whitespace().collect();
    // perms nlink <owner…> <group…> size month day time
    if t.len() < 8 || !is_perms(t[0]) || !is_num(t[1]) {
        return None;
    }
    let n = t.len();
    let (size, day, time) = (t[n - 4], t[n - 2], t[n - 1]);
    if !is_num(size) || !is_day(day) || !is_time_or_year(time) || is_num(t[n - 3]) {
        return None;
    }
    let names = &t[2..n - 4];
    let (user, group) = match names.len() {
        2 => (names[0].to_owned(), names[1].to_owned()),
        k if k % 2 == 0 && names[..k / 2] == names[k / 2..] => {
            let half = names[..k / 2].join(" ");
            (half.clone(), half)
        }
        _ => return None,
    };
    Some(Owner { user, group })
}

fn is_perms(s: &str) -> bool {
    let b = s.as_bytes();
    // `drwxr-xr-x`, иногда с `+`/`@`/`.` (ACL, xattr).
    (10..=11).contains(&b.len())
        && b"-dlcbps".contains(&b[0])
        && b[1..10].iter().all(|c| b"rwxsStT-".contains(c))
}

fn is_num(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|c| c.is_ascii_digit())
}

fn is_day(s: &str) -> bool {
    s.parse::<u8>().is_ok_and(|d| (1..=31).contains(&d))
}

fn is_time_or_year(s: &str) -> bool {
    match s.split_once(':') {
        Some((h, m)) => is_num(h) && is_num(m) && h.len() <= 2 && m.len() == 2,
        None => is_num(s) && s.len() == 4,
    }
}

/// Тип файла из `mode` (`S_IFMT`).
fn type_char(mode: u32) -> char {
    match mode & 0o170_000 {
        0o040_000 => 'd',
        0o120_000 => 'l',
        0o020_000 => 'c',
        0o060_000 => 'b',
        0o010_000 => 'p',
        0o140_000 => 's',
        _ => '-',
    }
}

/// `mode` → `drwxr-xr-x` (setuid/setgid/sticky — `s`/`S`, `t`/`T`).
pub fn mode_string(mode: u32) -> String {
    let mut s = String::with_capacity(10);
    s.push(type_char(mode));
    let bit = |m: u32, c: char| if mode & m != 0 { c } else { '-' };
    let special = |x: u32, sp: u32, on: char, off: char| match (mode & x != 0, mode & sp != 0) {
        (true, true) => on,
        (false, true) => off,
        (true, false) => 'x',
        (false, false) => '-',
    };
    s.push(bit(0o400, 'r'));
    s.push(bit(0o200, 'w'));
    s.push(special(0o100, 0o4000, 's', 'S'));
    s.push(bit(0o040, 'r'));
    s.push(bit(0o020, 'w'));
    s.push(special(0o010, 0o2000, 's', 'S'));
    s.push(bit(0o004, 'r'));
    s.push(bit(0o002, 'w'));
    s.push(special(0o001, 0o1000, 't', 'T'));
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owners_snapshot() {
        let cases: Vec<(&str, &str, Option<u32>, Option<u32>)> = vec![
            (
                "drwxr-xr-x    2 deploy   www-data     4096 Oct 23 09:14 portal",
                "portal",
                Some(1000),
                Some(33),
            ),
            (
                "-rw-r--r--    1 администратор пользователи   12 Jan  1  2025 отчёт.txt",
                "отчёт.txt",
                Some(1001),
                Some(100),
            ),
            (
                "-rw-r--r--    1 中文用户 组  0 Mar 15 12:00 file with spaces.txt",
                "file with spaces.txt",
                None,
                None,
            ),
            (
                "-rw-r--r--    1 NT AUTHORITY\\SYSTEM NT AUTHORITY\\SYSTEM 5 Nov  1 10:00 a.log",
                "a.log",
                None,
                None,
            ),
            ("garbage", "x", Some(0), Some(0)),
            ("", "x", None, None),
            ("-rw-r--r-- 1 a b c 12 Jan 1 12:00 x", "x", Some(7), Some(8)),
            ("-rw-r--r-- one a b 12 Jan 1 12:00 x", "x", Some(7), None),
        ];
        let out: Vec<_> = cases
            .iter()
            .map(|(l, n, u, g)| (*l, owner(l, n, *u, *g)))
            .collect();
        insta::assert_debug_snapshot!(out);
    }

    #[test]
    fn modes() {
        assert_eq!(mode_string(0o040_755), "drwxr-xr-x");
        assert_eq!(mode_string(0o100_644), "-rw-r--r--");
        assert_eq!(mode_string(0o120_777), "lrwxrwxrwx");
        assert_eq!(mode_string(0o104_755), "-rwsr-xr-x");
        assert_eq!(mode_string(0o102_644), "-rw-r-Sr--");
        assert_eq!(mode_string(0o041_777), "drwxrwxrwt");
        assert_eq!(mode_string(0o041_776), "drwxrwxrwT");
        assert_eq!(mode_string(0), "----------");
    }
}
