//! Приёмка S7-40: удаление, копирование и вставка блока = модель по строкам
//! `Vec<String>` (табы, CJK, CRLF, 1251, UTF-16); отмена возвращает исходник; копия
//! блока → вставка колонкой в другое место даёт тот же прямоугольник.

use encoding_rs::{UTF_8, UTF_16LE, WINDOWS_1251};
use proptest::prelude::*;
use unicode_width::UnicodeWidthChar;

use super::super::{Act, Block, Done, Engine, Env, Move, Sel, Selections, Unit};
use super::{BlockOut, clip_lines};
use crate::detect::Eol;
use crate::edit::open::Guard;
use crate::edit::piece::tests::{doc_of, read_fwd};
use crate::enc::Enc;
use crate::index::Sep;

const TAB: u32 = 4;

fn enc_of(i: usize) -> Enc {
    Enc {
        enc: [UTF_8, WINDOWS_1251, UTF_16LE][i],
        auto: false,
        bom_len: 0,
    }
}

fn bytes_of(enc: &Enc, s: &str) -> Vec<u8> {
    super::super::encode(enc, s, false).expect("символ есть в кодировке")
}

fn eol_of(crlf: bool) -> Eol {
    if crlf { Eol::CrLf } else { Eol::Lf }
}

fn eol_s(crlf: bool) -> &'static str {
    if crlf { "\r\n" } else { "\n" }
}

/// Документ движка над текстом в памяти, окно 200×50.
fn engine(text: &str, enc: Enc, crlf: bool) -> Engine {
    let bytes = bytes_of(&enc, text);
    let eol = eol_of(crlf);
    let doc = doc_of(&bytes, Sep::of(&enc, eol));
    let len = bytes.len() as u64;
    let mut e = Engine::new(doc, enc, eol, Guard::Memory, 0..len);
    (e.cols, e.rows) = (200, 50);
    e.tab = TAB as u8;
    e
}

fn env(crlf: bool) -> Env {
    Env {
        eol_new: eol_of(crlf),
        readonly: false,
        now: std::time::Instant::now(),
    }
}

fn width(c: char, col: u32) -> u32 {
    if c == '\t' {
        TAB - col % TAB
    } else {
        u32::try_from(c.width().unwrap_or(1).min(2)).unwrap()
    }
}

/// Модель `col_span`: символы `[s, e)` строки, задевающие колонки `[c0, c1)`, и ширина.
fn span(line: &[char], c0: u32, c1: u32) -> (usize, usize, u32) {
    let (mut s, mut e) = (line.len(), line.len());
    let (mut started, mut ended) = (false, false);
    let mut col = 0;
    for (i, &c) in line.iter().enumerate() {
        let w = width(c, col);
        if w > 0 {
            if !started && c0 < c1 && col + w > c0 && col < c1 {
                started = true;
                s = i;
            }
            if !ended && col >= c1 {
                ended = true;
                e = i;
            }
        }
        col += w;
    }
    if !started {
        s = e;
    }
    (s, e, col)
}

/// Символ, покрывающий колонку (`off_at_col`); дальше конца — длина.
fn at_col(line: &[char], c: u32) -> usize {
    let mut col = 0;
    for (i, &ch) in line.iter().enumerate() {
        let w = width(ch, col);
        if col + w > c {
            return i;
        }
        col += w;
    }
    line.len()
}

fn col_at(line: &[char], k: usize) -> u32 {
    let mut col = 0;
    for &ch in &line[..k] {
        col += width(ch, col);
    }
    col
}

/// Смещение символа `k` строки `i` в байтах документа.
fn off(lines: &[Vec<char>], i: usize, k: usize, enc: &Enc, crlf: bool) -> u64 {
    let eol = bytes_of(enc, eol_s(crlf)).len();
    let mut n = 0;
    for l in &lines[..i] {
        n += bytes_of(enc, &l.iter().collect::<String>()).len() + eol;
    }
    let head: String = lines[i][..k].iter().collect();
    (n + bytes_of(enc, &head).len()) as u64
}

fn join(lines: &[Vec<char>], crlf: bool) -> String {
    lines
        .iter()
        .map(|l| l.iter().collect::<String>())
        .collect::<Vec<_>>()
        .join(eol_s(crlf))
}

/// Блок между углами `(строка, колонка)`.
fn select(
    e: &mut Engine,
    lines: &[Vec<char>],
    a: (usize, u32),
    b: (usize, u32),
    enc: &Enc,
    crlf: bool,
) {
    let anchor = off(lines, a.0, at_col(&lines[a.0], a.1), enc, crlf);
    let caret = off(lines, b.0, at_col(&lines[b.0], b.1), enc, crlf);
    e.sel = Selections {
        caret,
        sel: Sel::Block(Block {
            anchor,
            anchor_col: a.1,
            col: b.1,
        }),
    };
}

fn chars(enc_i: usize) -> Vec<char> {
    if enc_i == 1 {
        vec!['a', 'b', ' ', '\t', 'д']
    } else {
        vec!['a', 'b', ' ', '\t', 'д', '日']
    }
}

fn lines_strategy(enc_i: usize) -> impl Strategy<Value = Vec<Vec<char>>> {
    let cs = chars(enc_i);
    prop::collection::vec(prop::collection::vec(prop::sample::select(cs), 0..9), 1..7)
}

/// Документ: строки и признак конечного перевода строки (тогда последняя — пустая).
fn doc_strategy() -> impl Strategy<Value = (usize, bool, Vec<Vec<char>>)> {
    (0usize..3, any::<bool>(), any::<bool>()).prop_flat_map(|(enc_i, crlf, tail)| {
        lines_strategy(enc_i).prop_map(move |mut l| {
            if tail {
                l.push(Vec::new());
            }
            (enc_i, crlf, l)
        })
    })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(160))]

    /// Удаление блока (Del) = модель по строкам; копия — строки блока; отмена — исходник.
    #[test]
    fn delete_and_copy_match_model(
        (enc_i, crlf, lines) in doc_strategy(),
        a in 0usize..8, b in 0usize..8, ca in 0u32..14, cb in 0u32..14,
    ) {
        let n = lines.len();
        let (a, b) = (a % n, b % n);
        prop_assume!(a != b || ca != cb);
        let enc = enc_of(enc_i);
        let text = join(&lines, crlf);
        let mut e = engine(&text, enc, crlf);
        let env = env(crlf);
        select(&mut e, &lines, (a, ca), (b, cb), &enc, crlf);
        let sel0 = e.sel;
        let (c0, c1) = (ca.min(cb), ca.max(cb));
        let (top, bot) = (a.min(b), a.max(b));
        // Копия.
        prop_assert_eq!(e.act(Act::BlockCopy { cut: false, limit: u64::MAX }, &env), Ok(Done::Same));
        let want: Vec<String> = (top..=bot)
            .map(|i| {
                let (s, t, _) = span(&lines[i], c0, c1);
                lines[i][s..t].iter().collect()
            })
            .collect();
        prop_assert_eq!(e.block_out.take(), Some(BlockOut::Copy(want.join(eol_s(crlf)))));
        // Удаление.
        let mut model = lines.clone();
        for l in &mut model[top..=bot] {
            let (s, t, _) = span(l, c0, c1);
            l.drain(s..t);
        }
        let changed = model != lines;
        let d = e.act(Act::Delete(Move::Right), &env);
        prop_assert_eq!(d, Ok(if changed { Done::Edited } else { Done::Moved }));
        prop_assert_eq!(read_fwd(&e.text.view()), bytes_of(&enc, &join(&model, crlf)));
        if changed {
            e.undo(false);
            prop_assert_eq!(read_fwd(&e.text.view()), bytes_of(&enc, &text));
            prop_assert_eq!(e.sel, sel0, "отмена — и блок");
        }
    }

    /// Вставка колонкой от курсора = модель: короткие строки добиваются пробелами,
    /// недостающие — дописываются; одна группа отмены.
    #[test]
    fn paste_column_matches_model(
        (enc_i, crlf, lines) in doc_strategy(),
        clip in lines_strategy(0),
        p in 0usize..8, k in 0usize..10,
    ) {
        let n = lines.len();
        let p = p % n;
        let k = k % (lines[p].len() + 1);
        let enc = enc_of(enc_i);
        let clip: Vec<Vec<char>> = clip
            .into_iter()
            .map(|l| l.into_iter().filter(|c| enc_i != 1 || *c != '日').collect())
            .collect();
        let text = join(&lines, crlf);
        let mut e = engine(&text, enc, crlf);
        let env = env(crlf);
        e.sel = Selections::at(off(&lines, p, k, &enc, crlf));
        let col = col_at(&lines[p], k);
        // Конечный перевод строки буфера — не строка (как у VS и Notepad++).
        let clip_text = join(&clip, false);
        let clip: Vec<Vec<char>> = clip_lines(&clip_text).iter().map(|l| l.chars().collect()).collect();
        let mut model = lines.clone();
        for (i, l) in clip.iter().enumerate() {
            let li = p + i;
            if li < model.len() {
                let (s, _, w) = span(&model[li], col, col);
                let mut ins: Vec<char> = std::iter::repeat_n(' ', col.saturating_sub(w) as usize).collect();
                ins.extend(l);
                let at = if w < col { model[li].len() } else { s };
                model[li].splice(at..at, ins);
            } else {
                let mut nl: Vec<char> = std::iter::repeat_n(' ', col as usize).collect();
                nl.extend(l);
                model.push(nl);
            }
        }
        let d = e.act(Act::Paste { text: clip_text, replace: false, column: true }, &env);
        prop_assert_eq!(d, Ok(if model == lines { Done::Same } else { Done::Edited }));
        prop_assert_eq!(read_fwd(&e.text.view()), bytes_of(&enc, &join(&model, crlf)));
        if model != lines {
            e.undo(false);
            prop_assert_eq!(read_fwd(&e.text.view()), bytes_of(&enc, &text), "одна группа");
        }
    }
}

/// Копия блока → вставка колонкой в другое место → тот же прямоугольник там.
#[test]
fn copy_then_paste_same_rectangle() {
    let text = "abcdef\nghijkl\nmnopqr\n\nxy\n";
    let crlf = false;
    let enc = Enc::utf8();
    let lines: Vec<Vec<char>> = text.split('\n').map(|l| l.chars().collect()).collect();
    let mut e = engine(text, enc, crlf);
    let env = env(crlf);
    select(&mut e, &lines, (0, 1), (2, 4), &enc, crlf);
    e.act(
        Act::BlockCopy {
            cut: false,
            limit: u64::MAX,
        },
        &env,
    )
    .unwrap();
    let Some(BlockOut::Copy(clip)) = e.block_out.take() else {
        panic!("копия блока");
    };
    assert_eq!(clip, "bcd\nhij\nnop");
    // Курсор — начало пустой строки 3: строки 3…5 получают по строке блока.
    e.sel = Selections::at(off(&lines, 3, 0, &enc, crlf));
    e.act(
        Act::Paste {
            text: clip.clone(),
            replace: false,
            column: true,
        },
        &env,
    )
    .unwrap();
    let got = String::from_utf8(read_fwd(&e.text.view())).unwrap();
    assert_eq!(got, "abcdef\nghijkl\nmnopqr\nbcd\nhijxy\nnop");
    // Тот же прямоугольник на новом месте.
    let after: Vec<Vec<char>> = got.split('\n').map(|l| l.chars().collect()).collect();
    select(&mut e, &after, (3, 0), (5, 3), &enc, crlf);
    e.act(
        Act::BlockCopy {
            cut: false,
            limit: u64::MAX,
        },
        &env,
    )
    .unwrap();
    assert_eq!(e.block_out.take(), Some(BlockOut::Copy(clip)));
}

/// Alt+Shift+стрелки: колонка за концом строки сохраняется; ввод при блоке — блок
/// прочь, текст у курсора, одна группа; Alt+мышь — блок до колонки за концом.
#[test]
fn block_moves_typing_and_mouse() {
    let text = "abcdef\nab\nabcdef";
    let mut e = engine(text, Enc::utf8(), false);
    let env = env(false);
    let blk = |e: &mut Engine, m| {
        e.act(Act::Move(m, super::super::Extend::Block), &env)
            .unwrap();
    };
    // Курсор на колонке 4 первой строки, блок вниз на две строки и вправо.
    e.sel = Selections::at(4);
    blk(&mut e, Move::Down);
    blk(&mut e, Move::Down);
    blk(&mut e, Move::Right);
    let a = e.block_area().unwrap();
    assert_eq!((a.top, a.c0, a.c1), (0, 4, 5));
    assert_eq!(a.bottom, 10, "третья строка");
    // Вторая строка короче — курсор у её конца, колонка помнится.
    let Sel::Block(b) = e.sel.sel else { panic!() };
    assert_eq!(b.col, 5);
    // Ввод: колонки 4..5 прочь (вторая строка не тронута), «X» — на строке курсора.
    e.act(
        Act::Input {
            text: "X".into(),
            replace: false,
        },
        &env,
    )
    .unwrap();
    assert_eq!(read_fwd(&e.text.view()), b"abcdf\nab\nabcdXf");
    e.undo(false);
    assert_eq!(
        read_fwd(&e.text.view()),
        text.as_bytes(),
        "ввод — одна группа с удалением"
    );
    // Alt+мышь: от второй строки колонка 6 (за концом) до третьей колонка 2.
    e.act(
        Act::Caret {
            off: 8,
            extend: false,
            unit: Unit::Char,
            block: Some(6),
        },
        &env,
    )
    .unwrap();
    e.act(
        Act::Caret {
            off: 12,
            extend: true,
            unit: Unit::Char,
            block: Some(2),
        },
        &env,
    )
    .unwrap();
    let a = e.block_area().unwrap();
    assert_eq!((a.top, a.bottom, a.c0, a.c1), (7, 10, 2, 6));
    e.act(Act::Delete(Move::Left), &env).unwrap();
    assert_eq!(read_fwd(&e.text.view()), b"abcdef\nab\nab");
}

#[test]
fn clip_text_lines() {
    assert_eq!(clip_lines("a\r\nb\r\n"), ["a", "b"]);
    assert_eq!(clip_lines("a\nb"), ["a", "b"]);
    assert_eq!(clip_lines("a\rb\n\n"), ["a", "b", ""]);
    assert_eq!(clip_lines(""), [""]);
}

/// Через core: Alt+Shift (`editor.move select=block`), `editor.copy` — текст в буфер с
/// признаком блока, `editor.paste` такого буфера — колонкой; вырезание — одна группа.
#[test]
fn copy_paste_through_commands() {
    use super::super::tests::{cmd, editor, eng, pump_until, text};
    use urfin_core::Command;
    let dir = tempfile::tempdir().unwrap();
    let mut c = editor(
        dir.path(),
        "b.txt",
        b"abcd\r\nefgh\r\nijkl\r\n",
        80,
        crate::EditorCfg::default(),
    );
    let mv = |c: &mut urfin_core::Core, to: &str| {
        c.dispatch(
            Command::new("editor.move")
                .arg("to", to)
                .arg("select", "block"),
        );
        c.pump(std::time::Duration::from_millis(1));
    };
    c.dispatch(Command::new("editor.move").arg("to", "right"));
    c.pump(std::time::Duration::from_millis(1));
    mv(&mut c, "down");
    mv(&mut c, "right");
    mv(&mut c, "right");
    assert!(eng(&c).block_area().is_some());
    cmd(&mut c, "editor.cut");
    pump_until(&mut c, |_| {
        crate::sys::clip_read().unwrap() == ("bc\r\nfg".into(), true)
    });
    assert_eq!(text(&c), b"ad\r\neh\r\nijkl\r\n");
    // Курсор после вырезания — на строке курсора блока (вторая), колонка 1; в конец
    // первой строки — вставка колонкой: «bc» и «fg» — у колонки 2.
    assert_eq!(eng(&c).sel.caret, 5);
    c.dispatch(Command::new("editor.move").arg("to", "up"));
    c.pump(std::time::Duration::from_millis(1));
    c.dispatch(Command::new("editor.move").arg("to", "end"));
    c.pump(std::time::Duration::from_millis(1));
    cmd(&mut c, "editor.paste");
    pump_until(&mut c, |c| text(c).len() > 14);
    assert_eq!(text(&c), b"adbc\r\nehfg\r\nijkl\r\n");
    cmd(&mut c, "editor.undo");
    assert_eq!(text(&c), b"ad\r\neh\r\nijkl\r\n", "вставка — одна группа");
    cmd(&mut c, "editor.undo");
    assert_eq!(
        text(&c),
        b"abcd\r\nefgh\r\nijkl\r\n",
        "вырезание — одна группа"
    );
}

// ------------------------------------------------------------ S7-42: задачей

/// Задача блока движка `e` (из `block_out`) здесь же; отмена — `cancel`.
fn run_job(e: &mut Engine, cancel: bool) -> Result<Done, crate::edit::bigblock::BlockError> {
    use crate::edit::bigblock::{BlockRes, run};
    let Some(BlockOut::Job(j)) = e.block_out.take() else {
        panic!("ждали задачу: {:?}", e.block_out);
    };
    let dir = tempfile::tempdir().unwrap();
    let temp = crate::edit::replace::TempAt {
        dir: dir.path().to_path_buf(),
        name: "block".into(),
    };
    let done = std::sync::atomic::AtomicU64::new(0);
    let snap = e.text.snap();
    match run(&snap, &j, &e.seg_spec(), &temp, &done, &|| cancel)? {
        BlockRes::Done(d) => Ok(e.apply_block(d, std::time::Instant::now())),
        BlockRes::TooBig(n) => {
            e.block_out = Some(BlockOut::TooBig(n));
            Ok(Done::Same)
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(200))]

    /// S7-42 (малый объём той же логики, что у тяжёлого теста): блок задачей
    /// (`block_lines = 0`) = синхронный путь — текст, курсор, буфер обмена, лимит
    /// копирования; итог — одна группа отмены, отмена возвращает исходник и блок.
    #[test]
    fn job_matches_sync_path(
        (enc_i, crlf, lines) in doc_strategy(),
        a in 0usize..8, b in 0usize..8, ca in 0u32..14, cb in 0u32..14,
        op in 0usize..7,
        clip in lines_strategy(0),
    ) {
        let n = lines.len();
        let (a, b) = (a % n, b % n);
        prop_assume!(a != b || ca != cb);
        let enc = enc_of(enc_i);
        let text = join(&lines, crlf);
        let env = env(crlf);
        let clip: Vec<Vec<char>> = clip
            .into_iter()
            .map(|l| l.into_iter().filter(|c| enc_i != 1 || *c != '日').collect())
            .collect();
        let act = match op {
            0 => Act::Delete(Move::Right),
            1 => Act::Input { text: "z".into(), replace: false },
            2 => Act::BlockCopy { cut: false, limit: u64::MAX },
            3 => Act::BlockCopy { cut: true, limit: u64::MAX },
            4 => Act::BlockCopy { cut: true, limit: 0 },
            _ => Act::Paste { text: join(&clip, false), replace: false, column: true },
        };
        let mut sync = engine(&text, enc, crlf);
        let mut job = engine(&text, enc, crlf);
        job.block_lines = 0;
        for e in [&mut sync, &mut job] {
            select(e, &lines, (a, ca), (b, cb), &enc, crlf);
            if op == 6 {
                // Вставка колонкой без блока — от курсора.
                e.sel = Selections::at(e.sel.caret);
            }
        }
        let sel0 = job.sel;
        let want = sync.act(act.clone(), &env);
        prop_assert_eq!(job.act(act, &env), Ok(Done::Same));
        let got = run_job(&mut job, false).unwrap();
        prop_assert_eq!(Ok(got.clone()), want);
        prop_assert_eq!(read_fwd(&job.text.view()), read_fwd(&sync.text.view()));
        prop_assert_eq!(job.sel, sync.sel);
        prop_assert_eq!(job.block_out.take(), sync.block_out.take());
        if got == Done::Edited {
            prop_assert!(job.undo(false).is_some());
            prop_assert_eq!(read_fwd(&job.text.view()), bytes_of(&enc, &text), "одна группа");
            prop_assert_eq!(job.sel, sel0, "отмена — и блок");
        }
    }
}

/// S7-42: отменённая задача документ не трогает — текст и блок как были.
#[test]
fn job_cancel_keeps_text() {
    let text = "abcd\nefgh\nijkl\n";
    let enc = Enc::utf8();
    let lines: Vec<Vec<char>> = text.split('\n').map(|l| l.chars().collect()).collect();
    let mut e = engine(text, enc, false);
    e.block_lines = 0;
    select(&mut e, &lines, (0, 1), (2, 3), &enc, false);
    let sel = e.sel;
    assert_eq!(e.act(Act::Delete(Move::Right), &env(false)), Ok(Done::Same));
    assert!(matches!(
        run_job(&mut e, true),
        Err(crate::edit::bigblock::BlockError::Cancelled)
    ));
    assert_eq!(read_fwd(&e.text.view()), text.as_bytes());
    assert_eq!(e.sel, sel);
    assert!(!e.undo.can_undo(), "журнал пуст");
}

/// S7-42 через core: блок шире 8 мебибайт (колонка 0 всех строк до конца документа) — Del
/// уходит задачей (документ только для чтения, прогресс), итог — одна группа отмены;
/// отмена задачи (`editor.search_cancel`) оставляет документ как был.
#[test]
fn big_block_through_core() {
    use super::super::tests::{cmd, ed, editor, eng, pump_until, text};
    use crate::edit::EdState;
    use urfin_core::Command;
    let dir = tempfile::tempdir().unwrap();
    let line = format!("ab{}\n", "c".repeat(4093));
    let body = line.repeat((9 << 20) / line.len() + 1);
    let mut c = editor(
        dir.path(),
        "big.txt",
        body.as_bytes(),
        80,
        crate::EditorCfg::default(),
    );
    let step = |c: &mut urfin_core::Core, cmd: Command| {
        c.dispatch(cmd);
        c.pump(std::time::Duration::from_millis(1));
    };
    // Курсор — колонка 1 первой строки, угол блока — пустая строка в конце: колонки [0, 1).
    step(&mut c, Command::new("editor.move").arg("to", "right"));
    let block = |c: &mut urfin_core::Core| {
        step(
            c,
            Command::new("editor.move")
                .arg("to", "doc_end")
                .arg("select", "block"),
        );
        assert!(eng(c).block_area().is_some());
    };
    block(&mut c);
    step(&mut c, Command::new("editor.delete").arg("to", "right"));
    assert!(ed(&c).replace.is_some(), "задача с прогрессом");
    assert_eq!(
        ed(&c).state,
        EdState::Replacing,
        "документ только для чтения"
    );
    pump_until(&mut c, |c| {
        ed(c).state == EdState::Ready && ed(c).replace.is_none()
    });
    let want: String = body.split_inclusive('\n').map(|l| &l[1..]).collect();
    assert!(
        text(&c) == want.as_bytes(),
        "колонка 0 убрана из каждой строки"
    );
    cmd(&mut c, "editor.undo");
    assert!(text(&c) == body.as_bytes(), "одна группа");
    // Отмена задачи.
    let caret = eng(&c).sel;
    step(&mut c, Command::new("editor.delete").arg("to", "right"));
    assert!(ed(&c).replace.is_some());
    cmd(&mut c, "editor.search_cancel");
    assert!(ed(&c).replace.is_none());
    assert_eq!(ed(&c).state, EdState::Ready);
    for _ in 0..50 {
        c.pump(std::time::Duration::from_millis(5));
    }
    assert!(text(&c) == body.as_bytes(), "отмена — текст как был");
    assert_eq!(eng(&c).sel, caret);
}
