//! Перекодирование окна в UTF-8 с картой смещений (UTF-16 и многобайтовые `Shift_JIS`,
//! GBK, EUC-*; S7 §3.6): поиск идёт по UTF-8, совпадение переводится обратно в байты
//! источника. Недекодируемые байты — U+FFFD.

#![allow(
    clippy::many_single_char_names,
    reason = "u — UTF-8, s — источник, r — отрезок, k — символ"
)]

use crate::enc::{Enc, Step};

/// Отрезок, где каждый символ — `du` байт UTF-8 и `ds` байт источника.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
struct Run {
    u: usize,
    s: usize,
    du: u8,
    ds: u8,
}

/// Шаг опорных точек UTF-16 (байт источника).
const CP_STEP: usize = 256;

/// Карта «смещение в UTF-8 ↔ смещение в окне источника» по границам символов.
///
/// UTF-16 — опорные точки через [`CP_STEP`] байт и проход по UTF-8 от точки (символ
/// UTF-8 из 4 байт — 4 байта источника, остальные — 2); многобайтовые — отрезки
/// одинаковых по размеру символов.
#[derive(Clone, Debug, Default)]
pub struct OffMap {
    wide: bool,
    runs: Vec<Run>,
    cps: Vec<(usize, usize)>,
    /// Конец: длина UTF-8 и сколько байт источника декодировано.
    end_u: usize,
    end_s: usize,
}

fn utf8_len(lead: u8) -> usize {
    match lead {
        0..0x80 => 1,
        0xF0.. => 4,
        0xE0.. => 3,
        _ => 2,
    }
}

/// Байт источника UTF-16 на символ UTF-8 длины `l`.
fn wide_src(l: usize) -> usize {
    if l == 4 { 4 } else { 2 }
}

impl OffMap {
    /// Байт источника для границы символа `u` в `utf8` (внутри символа — его начало).
    pub fn to_src(&self, utf8: &[u8], u: usize) -> usize {
        if u >= self.end_u {
            return self.end_s;
        }
        if self.wide {
            let (mut p, mut s) = self.cps[self.cps.partition_point(|c| c.0 <= u) - 1];
            while p < u {
                let l = utf8_len(utf8[p]);
                if p + l > u {
                    break;
                }
                p += l;
                s += wide_src(l);
            }
            return s.min(self.end_s);
        }
        let i = self.runs.partition_point(|r| r.u <= u) - 1;
        let r = self.runs[i];
        r.s + (u - r.u) / usize::from(r.du) * usize::from(r.ds)
    }

    /// Граница символа в `utf8` для байта источника `s` (внутри символа — следующий).
    pub fn to_utf8(&self, utf8: &[u8], s: usize) -> usize {
        if s >= self.end_s {
            return self.end_u;
        }
        if self.wide {
            let (mut p, mut ss) = self.cps[self.cps.partition_point(|c| c.1 <= s) - 1];
            while ss < s && p < self.end_u {
                let l = utf8_len(utf8[p]);
                p += l;
                ss += wide_src(l);
            }
            return p.min(self.end_u);
        }
        let i = self.runs.partition_point(|r| r.s <= s) - 1;
        let r = self.runs[i];
        let k = (s - r.s).div_ceil(usize::from(r.ds));
        let u = r.u + k * usize::from(r.du);
        match self.runs.get(i + 1) {
            Some(n) => u.min(n.u),
            None => u.min(self.end_u),
        }
    }
}

/// Декодирует `src` в `out` (очищается) и строит карту.
pub fn decode(src: &[u8], enc: Enc, out: &mut Vec<u8>, map: &mut OffMap) {
    out.clear();
    map.runs.clear();
    map.cps.clear();
    if enc.is_utf16() {
        decode_utf16(src, enc.is_utf16be(), out, map);
        return;
    }
    map.wide = false;
    out.reserve(src.len() + src.len() / 2);
    let mut dec = enc.decoder();
    let mut i = 0;
    let mut last: Option<(u8, u8)> = None;
    while i < src.len() {
        let (c, n) = match dec.next(&src[i..]) {
            Step::Char(c, n) => (c, usize::from(n)),
            Step::Bad(n) => ('\u{FFFD}', usize::from(n)),
        };
        let mut b = [0u8; 4];
        let s = c.encode_utf8(&mut b);
        let shape = (s.len() as u8, n as u8);
        if last != Some(shape) {
            map.runs.push(Run {
                u: out.len(),
                s: i,
                du: shape.0,
                ds: shape.1,
            });
            last = Some(shape);
        }
        out.extend_from_slice(s.as_bytes());
        i += n;
    }
    map.end_u = out.len();
    map.end_s = i;
}

/// UTF-16: свой цикл по единицам (порча — U+FFFD), опорные точки через [`CP_STEP`].
fn decode_utf16(src: &[u8], be: bool, out: &mut Vec<u8>, map: &mut OffMap) {
    const BAD: [u8; 3] = [0xEF, 0xBF, 0xBD];
    map.wide = true;
    out.reserve(src.len() / 2 * 3 + 3);
    let n = src.len() & !1;
    let unit = |i: usize| {
        let p = [src[i], src[i + 1]];
        if be {
            u16::from_be_bytes(p)
        } else {
            u16::from_le_bytes(p)
        }
    };
    let mut i = 0;
    let mut next_cp = 0;
    while i < n {
        if i >= next_cp {
            map.cps.push((out.len(), i));
            next_cp = i + CP_STEP;
        }
        let u = unit(i);
        match u {
            0..0x80 => out.push(u as u8),
            0x80..0x800 => out.extend_from_slice(&[0xC0 | (u >> 6) as u8, 0x80 | (u & 0x3F) as u8]),
            0xD800..0xDC00 if i + 4 <= n && (0xDC00..0xE000).contains(&unit(i + 2)) => {
                let c =
                    0x10000 + ((u32::from(u) - 0xD800) << 10) + (u32::from(unit(i + 2)) - 0xDC00);
                out.extend_from_slice(&[
                    0xF0 | (c >> 18) as u8,
                    0x80 | ((c >> 12) & 0x3F) as u8,
                    0x80 | ((c >> 6) & 0x3F) as u8,
                    0x80 | (c & 0x3F) as u8,
                ]);
                i += 4;
                continue;
            }
            0xD800..0xE000 => out.extend_from_slice(&BAD),
            _ => out.extend_from_slice(&[
                0xE0 | (u >> 12) as u8,
                0x80 | ((u >> 6) & 0x3F) as u8,
                0x80 | (u & 0x3F) as u8,
            ]),
        }
        i += 2;
    }
    if map.cps.is_empty() {
        map.cps.push((0, 0));
    }
    if n < src.len() {
        out.extend_from_slice(&BAD);
    }
    map.end_u = out.len();
    map.end_s = src.len();
}

#[cfg(test)]
mod tests {
    use encoding_rs::{SHIFT_JIS, UTF_16BE, UTF_16LE};

    use super::*;

    #[test]
    fn utf16_and_sjis_maps() {
        for enc in [UTF_16LE, UTF_16BE, SHIFT_JIS] {
            let text = "ab Привет 日本 x😀y";
            let src: Vec<u8> = if enc == UTF_16LE {
                text.encode_utf16().flat_map(u16::to_le_bytes).collect()
            } else if enc == UTF_16BE {
                text.encode_utf16().flat_map(u16::to_be_bytes).collect()
            } else {
                enc.encode(&text.replace('😀', "?")).0.into_owned()
            };
            let mut out = Vec::new();
            let mut map = OffMap::default();
            decode(&src, Enc::new(enc), &mut out, &mut map);
            let s = std::str::from_utf8(&out).unwrap();
            assert!(s.contains("Привет 日本"), "{}", enc.name());
            let u = s.find("日本").unwrap();
            let a = map.to_src(&out, u);
            let b = map.to_src(&out, u + "日本".len());
            if enc == SHIFT_JIS {
                assert_eq!(&src[a..b], &*enc.encode("日本").0);
            } else {
                assert_eq!(b - a, 4);
                let x = s.find('😀').unwrap();
                let xs = map.to_src(&out, x);
                assert_eq!(map.to_src(&out, x + 4) - xs, 4, "суррогатная пара");
                assert_eq!(map.to_utf8(&out, xs), x);
            }
            assert_eq!(map.to_utf8(&out, a), u);
            assert_eq!(map.to_utf8(&out, src.len()), out.len());
            assert_eq!(map.to_src(&out, out.len()), src.len());
        }
    }

    /// Нечётный хвост и одинокий суррогат — U+FFFD; длинный текст — через опорные точки.
    #[test]
    fn utf16_bad_units_and_checkpoints() {
        let mut src: Vec<u8> = "абв"
            .repeat(200)
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect();
        src.extend_from_slice(&[0x00, 0xD8, b'x', 0, 0x41]);
        let mut out = Vec::new();
        let mut map = OffMap::default();
        decode(&src, Enc::new(UTF_16LE), &mut out, &mut map);
        let s = std::str::from_utf8(&out).unwrap();
        assert!(s.ends_with("\u{FFFD}x\u{FFFD}"));
        for (ci, (u, _)) in s.char_indices().enumerate().take(600) {
            assert_eq!(map.to_src(&out, u), ci * 2);
            assert_eq!(map.to_utf8(&out, ci * 2), u);
        }
    }
}
