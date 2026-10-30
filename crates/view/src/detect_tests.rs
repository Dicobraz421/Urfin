//! Корпус V5 (S7 §9): кодировки и EOL фикстурой; тексты — свои.

#![allow(
    clippy::many_single_char_names,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::type_complexity,
    reason = "тесты: короткие имена, доли длины, таблицы случаев"
)]

use encoding_rs::{
    Encoding, IBM866, KOI8_R, KOI8_U, SHIFT_JIS, UTF_8, UTF_16BE, UTF_16LE, WINDOWS_1251,
    WINDOWS_1252,
};

use super::*;

const RU: &str = "Вечером над рекой поднимался туман, и старый паромщик зажигал фонарь на \
корме. Он помнил каждый камень на дне и каждую мель у берега, потому что сорок лет подряд \
перевозил людей с одной стороны на другую. Летом через реку шли косцы с косами на плечах, \
осенью везли телеги с картошкой и капустой, зимой река вставала, и паром отдыхал у причала. \
Дети из деревни прибегали слушать его рассказы о том, как однажды весной лёд пошёл ночью и \
унёс мостки вместе с лодкой, а утром лодку нашли за три версты, целую и сухую. Паромщик \
говорил, что река никогда не берёт чужого, она только проверяет, крепко ли ты держишь своё. \
Внук его учился в городе на инженера и писал письма про мосты, которые скоро построят \
повсюду, так что паромы станут не нужны. Старик читал эти письма вслух, щурясь у окна, и \
улыбался в бороду: мост, конечно, дело хорошее, но кто же тогда будет зажигать фонарь, когда \
над водой поднимется туман? Ответа он не ждал, просто ставил чайник и садился чинить сеть, \
пока за окном темнело и где-то далеко лаяли собаки. ";

const UK: &str = "Їжак ішов стежкою через ґанок до саду, де під яблунею росла густа трава. \
Він знав, що там є смачні жуки, і що увечері прийде сусідський кіт, який завжди вдавав, ніби \
не помічає їжака. Бабуся виносила миску з молоком і казала, що це для кота, але їжак теж \
пив, коли ніхто не бачив. Восени листя падало на ґанок, і їжак збирав його на спині, щоб \
утеплити свою нору під старою дровітнею. ";

const FR: &str = "Le marché du samedi s'installe très tôt sur la place de l'église: les \
maraîchers déchargent les cagettes de pêches, de cerises et de fèves, la fromagère dispose \
ses tommes à côté des œufs frais, et le boulanger crie que la fournée de pain de campagne \
est déjà presque épuisée. Vers midi, les cafés débordent sur le trottoir et l'on entend des \
conversations animées sur la météo, la récolte et les élections municipales. Über die Straße \
hinweg grüßt der Nachbar mit einem fröhlichen Lächeln, während die Kinder Eis essen. ";

const JA: &str = "今日は朝から雨が降っていたので、私は図書館へ行って本を読みました。\
午後になると空が明るくなり、公園を散歩してから家に帰りました。夕方には友達から電話があり、\
来週の日曜日に一緒に山へ行く約束をしました。天気が良ければ、お弁当を持って頂上まで登る\
つもりです。";

fn enc(text: &str, e: &'static Encoding) -> Vec<u8> {
    let (b, _, bad) = e.encode(text);
    assert!(!bad, "{} не кодирует текст", e.name());
    b.into_owned()
}

fn utf16(text: &str, be: bool) -> Vec<u8> {
    text.encode_utf16()
        .flat_map(|u| if be { u.to_be_bytes() } else { u.to_le_bytes() })
        .collect()
}

fn ru(n: usize) -> String {
    RU.repeat(n)
}

fn guess(b: &[u8], tld: Option<&[u8]>) -> &'static Encoding {
    detect(b, true, tld).enc.enc
}

/// Корпус V5: (имя, байты, ожидаемая кодировка, tld).
fn corpus() -> Vec<(String, Vec<u8>, &'static Encoding, Option<&'static [u8]>)> {
    let ru_tld = Some(b"ru".as_slice());
    let mut v = Vec::new();
    for (name, text) in [("ru", ru(2)), ("ru-lines", ru(2).replace(". ", ".\r\n"))] {
        for e in [WINDOWS_1251, IBM866, KOI8_R] {
            v.push((format!("{name} {}", e.name()), enc(&text, e), e, ru_tld));
            v.push((
                format!("{name} {} no-tld", e.name()),
                enc(&text, e),
                e,
                None,
            ));
        }
        v.push((
            format!("{name} utf-8"),
            text.as_bytes().to_vec(),
            UTF_8,
            ru_tld,
        ));
        let mut bom = vec![0xEF, 0xBB, 0xBF];
        bom.extend_from_slice(text.as_bytes());
        v.push((format!("{name} utf-8 bom"), bom, UTF_8, None));
        let mut le = vec![0xFF, 0xFE];
        le.extend(utf16(&text, false));
        v.push((format!("{name} utf-16le bom"), le, UTF_16LE, None));
        let mut be = vec![0xFE, 0xFF];
        be.extend(utf16(&text, true));
        v.push((format!("{name} utf-16be bom"), be, UTF_16BE, None));
    }
    let uk = UK.repeat(5);
    v.push(("uk koi8-u".into(), enc(&uk, KOI8_U), KOI8_U, ru_tld));
    v.push((
        "uk 1251".into(),
        enc(&uk, WINDOWS_1251),
        WINDOWS_1251,
        ru_tld,
    ));
    let fr = FR.repeat(3);
    v.push(("fr 1252".into(), enc(&fr, WINDOWS_1252), WINDOWS_1252, None));
    v.push(("fr utf-8".into(), fr.as_bytes().to_vec(), UTF_8, None));
    let ja = JA.repeat(4);
    v.push(("ja shift_jis".into(), enc(&ja, SHIFT_JIS), SHIFT_JIS, None));
    v.push(("ja utf-8".into(), ja.as_bytes().to_vec(), UTF_8, None));
    // UTF-16 без BOM (§3.2 п. 2: нули на одной чётности) — латиница.
    v.push(("fr utf-16le".into(), utf16(&fr, false), UTF_16LE, None));
    v.push(("fr utf-16be".into(), utf16(&fr, true), UTF_16BE, None));
    v.push((
        "ascii".into(),
        b"plain ascii text\n".repeat(50),
        UTF_8,
        None,
    ));
    v
}

/// Приёмка S7-04: корпус V5 ≥ 95 % верных; 1251/866/KOI8 на русском ≥ 2 КБ — 100 %.
#[test]
fn corpus_accuracy() {
    let all = corpus();
    let mut wrong = Vec::new();
    for (name, bytes, want, tld) in &all {
        let got = guess(bytes, *tld);
        if got != *want {
            wrong.push(format!("{name}: {} вместо {}", got.name(), want.name()));
        }
    }
    let ok = all.len() - wrong.len();
    println!("V5: {ok}/{} верно; {wrong:?}", all.len());
    assert!(
        ok * 100 >= all.len() * 95,
        "{ok}/{} верно:\n{}",
        all.len(),
        wrong.join("\n")
    );
    let cyr: Vec<_> = wrong
        .iter()
        .filter(|w| w.starts_with("ru") && !w.contains("utf"))
        .collect();
    assert!(cyr.is_empty(), "русский однобайтовый:\n{cyr:#?}");
    for (name, bytes, ..) in &all {
        if name.starts_with("ru ") && !name.contains("utf") {
            assert!(bytes.len() >= 2048, "{name}: {} байт", bytes.len());
        }
    }
}

#[test]
fn bom_variants_and_utf32() {
    let d = detect(&[0xEF, 0xBB, 0xBF, b'a', b'\n'], true, None);
    assert_eq!((d.enc.enc, d.enc.bom_len, d.eol), (UTF_8, 3, Eol::Lf));
    assert!(d.enc.auto && !d.binary);
    let d = detect(&[0xFE, 0xFF, 0, b'a', 0, b'\r', 0, b'\n'], true, None);
    assert_eq!((d.enc.enc, d.enc.bom_len, d.eol), (UTF_16BE, 2, Eol::CrLf));
    let d = detect(&[0xFF, 0xFE, 0, 0, b'a', 0, 0, 0], true, None);
    assert!(d.utf32 && d.binary);
}

#[test]
fn binary_detection() {
    assert!(detect(b"MZ\x90\x00\x03\x00", true, None).binary, "NUL");
    let ctl: Vec<u8> = (0..200u8)
        .map(|i| if i % 5 == 0 { 0x01 } else { b'a' })
        .collect();
    assert!(detect(&ctl, true, None).binary, "20 % управляющих");
    let esc = b"\x1b[31mred\x1b[0m\ttab\x0cpage\n".repeat(10);
    assert!(!detect(&esc, true, None).binary, "ESC, таб, FF — текст");
    let d = detect(b"\x00\x01\x02", true, Some(b"ru"));
    assert_eq!(d.enc.enc, WINDOWS_1251, "hex-текст — ANSI по языку");
}

/// Крайние случаи: ASCII, короткий файл, обрезанный UTF-8.
#[test]
fn edge_cases() {
    assert_eq!(guess(b"", None), UTF_8);
    assert_eq!(guess(b"a", None), UTF_8);
    assert_eq!(guess(b"hello world", None), UTF_8);
    let text = "строка";
    let bytes = text.as_bytes();
    // Выборка режет двухбайтовый символ: не конец файла — всё ещё UTF-8.
    let cut = bytes.len() - 1;
    assert!(std::str::from_utf8(&bytes[..cut]).is_err());
    assert_eq!(detect(&bytes[..cut], false, None).enc.enc, UTF_8);
    // В конце файла обрезанный хвост — не UTF-8.
    assert_ne!(detect(&bytes[..cut], true, Some(b"ru")).enc.enc, UTF_8);
    // UTF-16 без BOM — только от 512 байт.
    assert!(
        detect(&utf16("ab", false), true, None).binary,
        "короткий UTF-16 без BOM — NUL"
    );
}

#[test]
fn koi8u_shown_as_koi8r_without_ukrainian_letters() {
    let b = enc(&ru(2), KOI8_R);
    assert_eq!(guess(&b, Some(b"ru")), KOI8_R);
    let b = enc(&UK.repeat(5), KOI8_U);
    assert_eq!(guess(&b, Some(b"ru")), KOI8_U);
}

#[test]
fn eol_kinds() {
    let e = Enc::utf8();
    assert_eq!(detect_eol(b"a\nb\n", &e, true), Eol::Lf);
    assert_eq!(detect_eol(b"a\r\nb\r\n", &e, true), Eol::CrLf);
    assert_eq!(detect_eol(b"a\rb\r", &e, true), Eol::Cr);
    assert_eq!(detect_eol(b"a\r\nb\nc", &e, true), Eol::Mixed);
    assert_eq!(detect_eol(b"a\rb\nc", &e, true), Eol::Mixed);
    assert_eq!(
        detect_eol(b"abc", &e, true),
        Eol::CrLf,
        "без переводов — CRLF"
    );
    assert_eq!(
        detect_eol(b"a\r\nb\r", &e, false),
        Eol::CrLf,
        "CR в конце выборки"
    );
    let le = Enc::new(UTF_16LE);
    assert_eq!(
        detect_eol(&utf16("a\r\nb\r\n", false), &le, true),
        Eol::CrLf
    );
    assert_eq!(detect_eol(&utf16("a\nb\n", false), &le, true), Eol::Lf);
}

#[test]
fn tld_by_ui_language() {
    for l in ["ru", "ru-RU", "uk", "be_BY", "kk"] {
        assert_eq!(tld_for_lang(l), Some(b"ru".as_slice()), "{l}");
    }
    assert_eq!(tld_for_lang("en-US"), None);
}

/// Таблица 256 однобайтовой кодировки и посимвольный декодер.
#[test]
fn byte_tables_and_decoder() {
    let t = Enc::new(WINDOWS_1251).table().unwrap();
    assert_eq!(t.0[0xC0], 'А');
    assert_eq!(t.0[0x98], '\u{98}', "0x98 в 1251 по WHATWG — C1");
    assert!(Enc::new(UTF_8).table().is_none());
    let mut d = Enc::new(SHIFT_JIS).decoder();
    let b = enc("日本", SHIFT_JIS);
    assert_eq!(d.next(&b), crate::enc::Step::Char('日', 2));
    assert_eq!(d.next(&b[2..]), crate::enc::Step::Char('本', 2));
    assert_eq!(d.next(&[0x81]), crate::enc::Step::Bad(1), "обрезанная пара");
    let mut d = Enc::new(UTF_16LE).decoder();
    assert_eq!(d.next(&utf16("😀", false)), crate::enc::Step::Char('😀', 4));
    assert_eq!(d.next(&[0x00, 0xDC]), crate::enc::Step::Bad(2));
    let mut d = Enc::utf8().decoder();
    assert_eq!(d.next("ж".as_bytes()), crate::enc::Step::Char('ж', 2));
    assert_eq!(d.next(&[0xD0]), crate::enc::Step::Bad(1));
}
