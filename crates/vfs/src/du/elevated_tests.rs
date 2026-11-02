use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use super::*;

const NONCE: &str = "0123456789abcdef0123456789abcdef";

fn out_in(base: &Path) -> PathBuf {
    out_path(&base.join("du").join(SUBDIR), NONCE)
}

fn req(roots: &[&str]) -> Request {
    Request {
        out: out_in(Path::new(r"C:\Users\u\AppData\Local\Urfin\state")),
        roots: roots.iter().map(PathBuf::from).collect(),
    }
}

fn os(a: &[&str]) -> Vec<OsString> {
    a.iter().map(OsString::from).collect()
}

/// Итоговый путь временного каталога (`C:\…`, длинные имена): так его готовит
/// [`collect`], и только по нему повышенный процесс создаёт файл.
fn real(p: &Path) -> PathBuf {
    plain(fs::canonicalize(p).unwrap())
}

#[test]
fn args_round_trip() {
    let r = req(&[r"C:\System Volume Information", r"D:\Users\other"]);
    assert_eq!(Request::parse(&r.args()).unwrap(), r);
    assert_eq!(r.nonce(), NONCE);
}

#[test]
fn bad_roots_rejected() {
    let out = out_in(Path::new(r"C:\s")).to_string_lossy().into_owned();
    let with = |root: &str| Request::parse(&os(&[FLAG, OUT, &out, root]));
    for bad in [
        r"\\srv\share\x",
        r"\\?\C:\x",
        r"\\.\C:",
        r"C:\a\..\b",
        r"C:\a\.\b",
        r"C:\a:stream",
        r"C:/a",
        r"C:\a\",
        r"C:a",
        "a\\b",
        "",
        "C:\\a\u{1}",
    ] {
        assert!(with(bad).is_err(), "{bad:?}");
    }
    assert!(with(r"C:\").is_ok());
    assert!(with(r"C:\Program Files\WindowsApps").is_ok());
    // Повтор (регистр не важен) и превышение числа.
    assert!(Request::parse(&os(&[FLAG, OUT, &out, r"C:\a", r"c:\A"])).is_err());
    let many: Vec<String> = (0..=MAX_ROOTS).map(|i| format!(r"C:\d{i}")).collect();
    let mut a = os(&[FLAG, OUT, &out]);
    a.extend(many.iter().map(OsString::from));
    assert!(Request::parse(&a).is_err());
    assert!(Request::parse(&os(&[FLAG, OUT, &out])).is_err(), "no roots");
}

#[test]
fn bad_out_rejected() {
    for bad in [
        r"C:\Windows\System32\x.dures",
        r"C:\s\du\elevated\du-0123.dures",
        r"C:\s\du\other\du-0123456789abcdef0123456789abcdef.dures",
        r"C:\s\x\elevated\du-0123456789abcdef0123456789abcdef.dures",
        r"C:\s\du\elevated\du-0123456789ABCDEF0123456789abcdef.dures",
        r"C:\s\du\elevated\du-0123456789abcdef0123456789abcdef.txt",
        r"\\srv\s\du\elevated\du-0123456789abcdef0123456789abcdef.dures",
    ] {
        assert!(
            Request::parse(&os(&[FLAG, OUT, bad, r"C:\x"])).is_err(),
            "{bad}"
        );
    }
    assert!(Request::parse(&os(&[FLAG, r"C:\x"])).is_err(), "no --out");
    assert!(Request::parse(&os(&["--other", OUT])).is_err());
}

fn answer(r: &Request) -> Answer {
    Answer {
        nonce: r.nonce().to_owned(),
        roots: r
            .roots
            .iter()
            .map(|p| RootSum {
                path: p.to_string_lossy().into_owned(),
                ok: true,
                disk: 4096,
                logical: 1000,
                files: 2,
                dirs: 1,
                ..RootSum::default()
            })
            .collect(),
    }
}

#[test]
fn result_round_trip_and_checks() {
    let r = req(&[r"C:\a", r"C:\b"]);
    let a = answer(&r);
    let b = encode(&a);
    assert_eq!(decode(&b, &r).unwrap(), a);
    // Порча любого байта тела — контрольная сумма.
    let mut bad = b.clone();
    bad[12] ^= 1;
    assert!(matches!(
        decode(&bad, &r),
        Err(ElevError::Result("checksum"))
    ));
    // Другая версия.
    let mut v = b.clone();
    v[8] = 9;
    assert!(decode(&v, &r).is_err());
    assert!(decode(&b[..5], &r).is_err(), "short");
    assert!(decode(b"NOTURFIN0123456789", &r).is_err(), "magic");
    let long = vec![0u8; usize::try_from(MAX_RESULT).unwrap() + 1];
    assert!(decode(&long, &r).is_err(), "too long");
    // Чужой nonce.
    let mut other = r.clone();
    other.out = out_in(Path::new(r"C:\s")).with_file_name(format!("du-{}.{EXT}", "f".repeat(32)));
    assert!(matches!(
        decode(&b, &other),
        Err(ElevError::Result("nonce"))
    ));
    // Путь вне запрошенных корней и повтор.
    let mut stray = a.clone();
    stray.roots[1].path = r"C:\Windows".into();
    assert!(decode(&encode(&stray), &r).is_err());
    let mut twice = a.clone();
    twice.roots[1].path = r"c:\A".into();
    assert!(decode(&encode(&twice), &r).is_err());
    // Только часть корней — можно (остальные остаются «нет доступа»).
    let mut part = a;
    part.roots.pop();
    assert!(decode(&encode(&part), &r).is_ok());
}

/// Повышенный режим тем же кодом без `runas`: обход временного каталога, файл
/// результата в `du\elevated`, итоги — как у обычного обхода.
#[test]
fn serve_counts_temp_dir() {
    let tmp = tempfile::tempdir().unwrap();
    let base = real(tmp.path());
    let data = base.join("data");
    fs::create_dir_all(data.join("a").join("b")).unwrap();
    fs::write(data.join("a").join("x.bin"), vec![0u8; 3000]).unwrap();
    fs::write(data.join("a").join("b").join("y.txt"), vec![0u8; 1000]).unwrap();
    fs::write(data.join("z.mp4"), vec![0u8; 500]).unwrap();
    let dir = base.join("du").join(SUBDIR);
    fs::create_dir_all(&dir).unwrap();
    let missing = base.join("missing");
    let r = Request {
        out: out_path(&dir, NONCE),
        roots: vec![data.clone(), missing.clone()],
    };
    let r = Request::parse(&r.args()).unwrap();
    assert_eq!(serve(&r), exit::OK);
    let a = decode(&fs::read(&r.out).unwrap(), &r).unwrap();
    assert_eq!(a.roots.len(), 2);
    let s = &a.roots[0];
    assert!(s.ok);
    assert_eq!((s.files, s.logical, s.dirs, s.denied), (3, 4500, 2, 0));
    assert!(s.disk >= s.logical || s.disk == 0, "{s:?}");
    assert_eq!(s.types_files.iter().sum::<u64>(), 3);
    assert_eq!(s.types_files[DuType::Video as usize], 1);
    assert!(!a.roots[1].ok, "нет каталога — не досчитан");
    // Файл уже есть — не перезаписывается.
    assert_eq!(serve(&r), exit::WRITE);
}

#[test]
fn serve_cancel_writes_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let base = real(tmp.path());
    let dir = base.join("du").join(SUBDIR);
    fs::create_dir_all(&dir).unwrap();
    let r = Request {
        out: out_path(&dir, NONCE),
        roots: vec![base.clone()],
    };
    fs::write(cancel_path(&r.out), b"").unwrap();
    // Знак отмены есть до начала: поток наблюдения ставит останов при первом опросе.
    std::thread::sleep(Duration::from_millis(50));
    let code = serve(&r);
    assert!(
        code == exit::CANCELLED || code == exit::OK,
        "обход мог закончиться раньше опроса: {code}"
    );
    if code == exit::CANCELLED {
        assert!(!r.out.exists());
    }
}

#[test]
fn main_rejects_args_and_policy() {
    assert_eq!(main(&os(&[FLAG, OUT, r"C:\x"]), true), exit::BAD_ARGS);
    let r = req(&[r"C:\a"]);
    assert_eq!(main(&r.args(), false), exit::POLICY);
}

#[test]
fn nonce_and_limits() {
    let (a, b) = (new_nonce(), new_nonce());
    assert!(is_nonce(&a) && is_nonce(&b));
    assert_ne!(a, b);
    let roots: Vec<PathBuf> = (0..MAX_ROOTS - 1)
        .map(|i| PathBuf::from(format!(r"C:\d{i}")))
        .collect();
    assert!(fits(&roots, Path::new(r"C:\x")));
    let full: Vec<PathBuf> = (0..MAX_ROOTS)
        .map(|i| PathBuf::from(format!(r"C:\d{i}")))
        .collect();
    assert!(!fits(&full, Path::new(r"C:\x")));
    let long = PathBuf::from(format!(r"C:\{}", "a".repeat(MAX_ARGS)));
    assert!(!fits(&[], &long));
}
