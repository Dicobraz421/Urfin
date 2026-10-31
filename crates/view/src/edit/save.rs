//! Кодирование текста редактора в байты файла (S7 §5.4): кодировка и BOM «как были».
//! Атомарная запись и подтверждения — S7-17.

use encoding_rs::{EncoderResult, Encoding, UTF_8, UTF_16BE, UTF_16LE};
use ropey::Rope;

/// Байты файла и сколько символов не представимы в кодировке (заменены на `?`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Encoded {
    pub bytes: Vec<u8>,
    pub unmappable: usize,
}

/// Текст в кодировке `enc`; `bom` — только у UTF-8 и UTF-16. Концы строк не трогаются:
/// документ хранит их как в файле (смешанные — тоже).
pub fn encode(text: &Rope, enc: &'static Encoding, bom: bool) -> Encoded {
    let mut out = Encoded {
        bytes: Vec::with_capacity(text.len() + 4),
        unmappable: 0,
    };
    if enc == UTF_16LE || enc == UTF_16BE {
        let be = enc == UTF_16BE;
        if bom {
            out.bytes
                .extend_from_slice(if be { &[0xFE, 0xFF] } else { &[0xFF, 0xFE] });
        }
        for c in text.chunks() {
            for u in c.encode_utf16() {
                out.bytes
                    .extend_from_slice(&if be { u.to_be_bytes() } else { u.to_le_bytes() });
            }
        }
        return out;
    }
    if enc == UTF_8 {
        if bom {
            out.bytes.extend_from_slice(&[0xEF, 0xBB, 0xBF]);
        }
        for c in text.chunks() {
            out.bytes.extend_from_slice(c.as_bytes());
        }
        return out;
    }
    let mut e = enc.new_encoder();
    for c in text.chunks() {
        feed(&mut e, c, false, &mut out);
    }
    feed(&mut e, "", true, &mut out);
    out
}

fn feed(e: &mut encoding_rs::Encoder, mut s: &str, last: bool, out: &mut Encoded) {
    loop {
        let need = e
            .max_buffer_length_from_utf8_without_replacement(s.len())
            .unwrap_or(s.len() * 4 + 16);
        out.bytes.reserve(need);
        let (r, read) = e.encode_from_utf8_to_vec_without_replacement(s, &mut out.bytes, last);
        s = &s[read..];
        match r {
            EncoderResult::InputEmpty => return,
            EncoderResult::OutputFull => {}
            EncoderResult::Unmappable(_) => {
                out.unmappable += 1;
                out.bytes.push(b'?');
            }
        }
    }
}
