//! Документ редактора без текста UI (S7 §2 «Редактор»): загрузка, сохранение, diff,
//! поиск и замена, внешний редактор — S7-15…S7-19. Текст правки живёт в
//! `Entity<EditorState>` gpui-kit в UI.

use urfin_core::{JobId, Location};

use crate::{DocId, Placement};

/// Фаза документа.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EdState {
    Loading { done: u64, total: u64 },
    Ready,
    Saving(JobId),
    Failed,
}

/// Документ редактора. Кодировка, EOL, отметка диска и полоса изменений — S7-15.
#[derive(Debug)]
pub struct EditorDoc {
    pub id: DocId,
    pub loc: Location,
    pub placement: Placement,
    pub state: EdState,
    /// Есть несохранённые правки (из `InboxMsg::Edited`).
    pub dirty: bool,
    /// Строка и колонка курсора, с 0.
    pub cursor: (u32, u32),
    pub rev: u64,
}

impl EditorDoc {
    pub fn new(id: DocId, loc: Location, placement: Placement) -> EditorDoc {
        EditorDoc {
            id,
            loc,
            placement,
            state: EdState::Loading { done: 0, total: 0 },
            dirty: false,
            cursor: (0, 0),
            rev: 0,
        }
    }
}
