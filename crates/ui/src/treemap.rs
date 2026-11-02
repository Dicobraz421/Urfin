//! Карта анализа места (F01 §3.4, `screen-dumap`): squarified-раскладка, два уровня,
//! «прочее» и кэш раскладок. Без gpui — рисует `gpui_shell::du_map`.
//!
//! Раскладка — Bruls, Huizing, van Wijk (2000): элементы по убыванию; строка кладётся
//! вдоль короткой стороны остатка; элемент добавляется в строку, пока худшее отношение
//! сторон строки не ухудшается. Вход уже по размеру ↓ — O(n).

use std::collections::VecDeque;
use std::rc::Rc;

use compact_str::{CompactString, format_compact};
use urfin_vfs::du::{DuType, MapItem, MapKind};

/// Плитки меньше — сливаются в «прочее» (px²).
pub const MIN_AREA: f32 = 16.0;
/// Полоса подписи каталога первого уровня над его детьми.
pub const HEAD_H: f32 = 18.0;
/// Раскладок в кэше.
const CACHE_LEN: usize = 8;

/// Прямоугольник в px.
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub fn new(x: f32, y: f32, w: f32, h: f32) -> Rect {
        Rect { x, y, w, h }
    }

    pub fn area(&self) -> f32 {
        self.w * self.h
    }

    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && y >= self.y && x < self.x + self.w && y < self.y + self.h
    }
}

/// Худшее отношение сторон строки: сумма `s`, крупнейший и мельчайший элементы, длина
/// короткой стороны `side` (всё — площади).
fn worst(s: f64, max: f64, min: f64, side: f64) -> f64 {
    if min <= 0.0 || s <= 0.0 {
        return f64::INFINITY;
    }
    let (s2, w2) = (s * s, side * side);
    (w2 * max / s2).max(s2 / (w2 * min))
}

/// Squarified-раскладка: плитка на каждый элемент `sizes` (по порядку) в `out`; площадь
/// плитки ∝ размеру, плитки не пересекаются и покрывают `rect`. `sizes` — по убыванию
/// (иначе раскладка верна, но плитки менее квадратные).
#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    reason = "пиксели: f64 для точности суммы, f32 на выходе"
)]
#[allow(
    clippy::many_single_char_names,
    reason = "геометрия: x, y, w, h, строка i..j"
)]
pub fn squarify(sizes: &[u64], rect: Rect, out: &mut Vec<Rect>) {
    let total: f64 = sizes.iter().map(|&s| s as f64).sum();
    if total <= 0.0 || rect.w <= 0.0 || rect.h <= 0.0 {
        out.extend(sizes.iter().map(|_| Rect::new(rect.x, rect.y, 0.0, 0.0)));
        return;
    }
    let scale = f64::from(rect.w) * f64::from(rect.h) / total;
    let area = |k: usize| sizes[k] as f64 * scale;
    let (mut x, mut y) = (f64::from(rect.x), f64::from(rect.y));
    let (mut w, mut h) = (f64::from(rect.w), f64::from(rect.h));
    let n = sizes.len();
    let mut i = 0;
    while i < n {
        let side = w.min(h);
        let (mut sum, mut max, mut min) = (area(i), area(i), area(i));
        let mut ratio = worst(sum, max, min, side);
        let mut j = i + 1;
        while j < n {
            let a = area(j);
            let next = worst(sum + a, max.max(a), min.min(a), side);
            if next > ratio {
                break;
            }
            (sum, max, min, ratio) = (sum + a, max.max(a), min.min(a), next);
            j += 1;
        }
        let last = j == n;
        if w >= h {
            // Столбец слева во всю высоту остатка.
            let t = if last { w } else { (sum / h).min(w) };
            let mut at = y;
            for k in i..j {
                let len = if k + 1 == j {
                    y + h - at
                } else if sum > 0.0 {
                    area(k) / sum * h
                } else {
                    0.0
                };
                out.push(Rect::new(x as f32, at as f32, t as f32, len as f32));
                at += len;
            }
            x += t;
            w -= t;
        } else {
            // Строка сверху во всю ширину остатка.
            let t = if last { h } else { (sum / w).min(h) };
            let mut at = x;
            for k in i..j {
                let len = if k + 1 == j {
                    x + w - at
                } else if sum > 0.0 {
                    area(k) / sum * w
                } else {
                    0.0
                };
                out.push(Rect::new(at as f32, y as f32, len as f32, t as f32));
                at += len;
            }
            y += t;
            h -= t;
        }
        i = j;
    }
}

/// Вид плитки.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum TileKind {
    Dir,
    File(DuType),
    /// Мелкие файлы каталога одним блоком.
    Small,
    /// «ещё N» — слитые плитки меньше [`MIN_AREA`].
    Rest {
        count: u32,
    },
}

/// Плитка раскладки.
#[derive(Clone, Debug, PartialEq)]
pub struct Tile {
    pub rect: Rect,
    /// 1 — дети каталога карты, 2 — их дети.
    pub depth: u8,
    pub kind: TileKind,
    pub name: CompactString,
    /// Путь от каталога карты через `\` (строка списка — у первого уровня); у «мелких»
    /// и «прочего» — путь их каталога (пусто — сам каталог карты).
    pub path: CompactString,
    pub size: u64,
    /// Внутри совпадения «Временных».
    pub temp: bool,
    /// Каталог первого уровня с полосой подписи и детьми под ней.
    pub head: bool,
}

/// Раскладка уровня со слиянием мелочи: плитки, сколько элементов осталось своими
/// плитками и сумма слитых в «прочее» (его плитка — последняя).
fn place(items: &[MapItem], rect: Rect) -> (Vec<Rect>, usize, u64) {
    let mut sizes: Vec<u64> = items.iter().map(|i| i.size).collect();
    let mut out = Vec::with_capacity(sizes.len());
    squarify(&sizes, rect, &mut out);
    let Some(cut) = out.iter().position(|r| r.area() < MIN_AREA) else {
        return (out, items.len(), 0);
    };
    let merged: u64 = sizes[cut..].iter().sum();
    sizes.truncate(cut);
    sizes.push(merged);
    out.clear();
    squarify(&sizes, rect, &mut out);
    (out, cut, merged)
}

fn tile(m: &MapItem, rect: Rect, depth: u8, path: CompactString) -> Tile {
    Tile {
        rect,
        depth,
        kind: match m.kind {
            MapKind::Dir => TileKind::Dir,
            MapKind::File(t) => TileKind::File(t),
            MapKind::Small { .. } => TileKind::Small,
        },
        name: m.name.clone(),
        path,
        size: m.size,
        temp: m.temp,
        head: false,
    }
}

/// Карта двух уровней в `area`: дети каталога и, у подкаталогов с местом под подпись,
/// их дети под полосой подписи.
pub fn layout(items: &[MapItem], area: Rect) -> Vec<Tile> {
    let mut out = Vec::new();
    let (rects, kept, rest) = place(items, area);
    for (k, r) in rects.into_iter().enumerate() {
        let Some(m) = items.get(k).filter(|_| k < kept) else {
            out.push(rest_tile(
                items.len() - kept,
                rest,
                r,
                1,
                CompactString::default(),
            ));
            continue;
        };
        let path = m.name.clone();
        let mut t = tile(m, r, 1, path.clone());
        let room = m.kind == MapKind::Dir
            && !m.kids.is_empty()
            && r.h >= 2.0 * HEAD_H
            && r.w >= 3.0 * HEAD_H;
        t.head = room;
        out.push(t);
        if !room {
            continue;
        }
        let inner = Rect::new(r.x + 1.0, r.y + HEAD_H, r.w - 2.0, r.h - HEAD_H - 1.0);
        let (kids, kept2, rest2) = place(&m.kids, inner);
        for (q, kr) in kids.into_iter().enumerate() {
            match m.kids.get(q).filter(|_| q < kept2) {
                Some(c) => {
                    let p = if c.name.is_empty() {
                        path.clone()
                    } else {
                        format_compact!("{path}\\{}", c.name)
                    };
                    out.push(tile(c, kr, 2, p));
                }
                None => out.push(rest_tile(m.kids.len() - kept2, rest2, kr, 2, path.clone())),
            }
        }
    }
    out
}

fn rest_tile(count: usize, size: u64, rect: Rect, depth: u8, path: CompactString) -> Tile {
    Tile {
        rect,
        depth,
        kind: TileKind::Rest {
            count: u32::try_from(count).unwrap_or(u32::MAX),
        },
        name: CompactString::default(),
        path,
        size,
        temp: false,
        head: false,
    }
}

/// Плитка под точкой: самая глубокая.
pub fn hit(tiles: &[Tile], x: f32, y: f32) -> Option<usize> {
    tiles.iter().rposition(|t| t.rect.contains(x, y))
}

/// Ключ раскладки: каталог, номер изменения снимка, размер и масштаб.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct Key {
    pub dir: u64,
    pub rev: u64,
    pub w: u32,
    pub h: u32,
    pub scale: u32,
}

impl Key {
    pub fn new(dir: u64, rev: u64, w: f32, h: f32, scale: f32) -> Key {
        Key {
            dir,
            rev,
            w: w.to_bits(),
            h: h.to_bits(),
            scale: scale.to_bits(),
        }
    }
}

/// LRU на 8 раскладок: пересчёт только при смене ключа; курсор, наведение и отметка —
/// слой поверх, без перераскладки.
#[derive(Debug, Default)]
pub struct Cache {
    slots: VecDeque<(Key, Rc<Vec<Tile>>)>,
}

impl Cache {
    /// Раскладка по ключу: из кэша или `make` (и в кэш).
    pub fn get_or(&mut self, key: Key, make: impl FnOnce() -> Vec<Tile>) -> Rc<Vec<Tile>> {
        if let Some(i) = self.slots.iter().position(|(k, _)| *k == key) {
            let hit = self.slots.remove(i).expect("position");
            let tiles = Rc::clone(&hit.1);
            self.slots.push_front(hit);
            return tiles;
        }
        let tiles = Rc::new(make());
        self.slots.push_front((key, Rc::clone(&tiles)));
        self.slots.truncate(CACHE_LEN);
        tiles
    }

    /// Раскладка по ключу, если есть (в начало LRU).
    pub fn get(&mut self, key: Key) -> Option<Rc<Vec<Tile>>> {
        let i = self.slots.iter().position(|(k, _)| *k == key)?;
        let hit = self.slots.remove(i)?;
        let tiles = Rc::clone(&hit.1);
        self.slots.push_front(hit);
        Some(tiles)
    }

    /// Положить готовую раскладку (посчитанную в фоне).
    pub fn put(&mut self, key: Key, tiles: Vec<Tile>) -> Rc<Vec<Tile>> {
        self.slots.retain(|(k, _)| *k != key);
        let tiles = Rc::new(tiles);
        self.slots.push_front((key, Rc::clone(&tiles)));
        self.slots.truncate(CACHE_LEN);
        tiles
    }

    /// Последняя раскладка того же каталога (любой размер и номер изменения) — её
    /// рисует кадр, пока новая считается в фоне.
    pub fn last_of(&self, dir: u64) -> Option<Rc<Vec<Tile>>> {
        self.slots
            .iter()
            .find(|(k, _)| k.dir == dir)
            .map(|(_, t)| Rc::clone(t))
    }

    pub fn len(&self) -> usize {
        self.slots.len()
    }

    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }
}

#[cfg(test)]
#[path = "treemap_tests.rs"]
mod tests;
