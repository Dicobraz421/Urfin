//! Состояние UI в `AppState.ext` (S2 §3, S0 v2 №10). Меняет только handler `ui`;
//! фронтенд читает. Ширины колонок — в наборах `view_modes` (S2-19).

use compact_str::CompactString;
use smallvec::SmallVec;
use urfin_core::{Location, OverlayKind, PromptId, Side, TabId};

/// Тема, заданная политикой организации (ADR-0020, T-01-5): из [`urfin_core::PolicySet`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ThemePolicy {
    /// `theme`: тема по умолчанию или единственная при `locked`.
    pub theme: Option<CompactString>,
    /// `theme_locked = true`: сменить тему нельзя.
    pub locked: bool,
}

impl ThemePolicy {
    pub fn from_set(p: &urfin_core::PolicySet) -> Self {
        ThemePolicy {
            theme: p.theme().map(Into::into),
            locked: p.bool(urfin_core::PolicyKey::ThemeLocked) == Some(true),
        }
    }
}

/// Пользовательские темы (`themes/*.toml`): загружаются задачей, не в UI-потоке.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct UserThemes {
    pub themes: std::sync::Arc<Vec<crate::theme::Theme>>,
    /// Растёт при каждой загрузке — ключ пересборки токенов окна.
    pub generation: u64,
    /// mtime каталога при последней загрузке: реактивация окна грузит заново, только
    /// если он изменился (без файлового вотчера).
    pub mtime: Option<std::time::SystemTime>,
}

/// Строка ввода аргумента команды (`ui.draft`): F7 → `ops.mkdir name`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Draft {
    /// Целевая команда.
    pub cmd: CompactString,
    /// Аргумент, который заполняет ввод.
    pub arg: CompactString,
    pub text: CompactString,
}

/// Меню подкаталогов breadcrumb (`ui.menu menu=subdirs`, ADR-0014).
#[derive(Clone, Debug, PartialEq)]
pub struct Subdirs {
    pub side: Side,
    /// Каталог, чьи подкаталоги в меню.
    pub loc: Location,
    /// `None` — читается; ошибка — текстом.
    pub items: Option<Result<Vec<CompactString>, CompactString>>,
    /// Поколение запроса: старый ответ не применяется.
    pub generation: u64,
}

/// Вариант дополнения поля (№70).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cand {
    pub text: String,
    /// Из истории поля (иначе — из ФС или текущей папки).
    pub history: bool,
}

/// Открытый список дополнений поля: для какого текста и что предложено.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Complete {
    pub generation: u64,
    pub kind: crate::field_history::FieldKind,
    /// Текст поля, для которого список (дополнение заменяет его целиком).
    pub text: String,
    /// Только история (Alt+↓): без ФС и имён папки.
    pub history_only: bool,
    pub items: Vec<Cand>,
}

/// Приветствие внизу (D-02): первый запуск или обновление (локальное сравнение версий).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Welcome {
    First,
    Updated { from: String, to: String },
}

/// Ссылка в панели (D-03, `screen-linkview.png`): junction или symlink, цель, цели нет.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LinkInfo {
    pub junction: bool,
    pub target: Option<String>,
    pub broken: bool,
}

/// Ссылки каталога панели: для какого листинга (папка, поколение) и по именам.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Links {
    pub loc: Option<Location>,
    pub generation: u64,
    pub items: std::collections::HashMap<CompactString, LinkInfo>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct UiState {
    /// Зеркало `Focus::Overlay` (S0 v3 №1): открытый оверлей.
    pub overlay: Option<OverlayKind>,
    /// Меню F9 и подменю (`ui.menu menu=`); `subdirs` и `crumbs` — меню breadcrumb.
    pub menu: Option<CompactString>,
    /// Панель меню breadcrumb (`ui.menu side=`).
    pub menu_side: Option<Side>,
    pub subdirs: Option<Subdirs>,
    pub draft: Option<Draft>,
    /// Rename по месту: таб и исходное имя.
    pub rename: Option<(TabId, CompactString)>,
    pub path_edit: Option<Side>,
    /// Поколение проверки введённого пути (`ui.path_edit path=`), пока она идёт.
    pub path_check: Option<u64>,
    /// Счётчик поколений фоновых запросов UI.
    pub next_gen: u64,
    /// Свёрнутые вопросы (`ui.prompt.hide`).
    pub prompt_hidden: SmallVec<[PromptId; 4]>,
    pub jobs_expanded: bool,
    /// Выбор темы (№59): `auto` — как в ОС, или id темы; `[ui] theme` при загрузке
    /// конфига, затем `ui.theme.*`. Политика `ThemeLocked` его перекрывает.
    pub theme: CompactString,
    /// Высота строки списка, px (`ui.density.set`): 20 / 24 / 28.
    pub density: u8,
    /// Иконки файлов (`ui.icons.set`).
    pub icons: crate::config::IconMode,
    pub user_themes: UserThemes,
    pub policy: ThemePolicy,
    /// Каталог своих тем (`<config_dir>/themes`); `None` — без диска (тесты).
    pub themes_dir: Option<std::path::PathBuf>,
    /// Свободное место тома активной панели — статус-строка (D-02).
    pub free: Option<crate::status::FreeSpace>,
    /// Открытый список дисков (`ui.drives`, D-03).
    pub drives: Option<crate::drives::Drives>,
    /// Масштаб в процентах (`ui.zoom`), 100 — без масштаба.
    pub zoom: u16,
    /// Доля левой панели (`ui.split`), 0,5 — поровну.
    pub split: f32,
    /// Последний выбранный набор колонок (`ui.columns`, S2-19).
    pub columns: Option<CompactString>,
    /// Наборы колонок `[ui.view_modes]` по порядку имён: `ViewMode::Custom(n)` — `n`-й,
    /// с ширинами колонок.
    pub view_modes: Vec<crate::columns::ColumnSet>,
    /// Ctrl+O: панели скрыты, виден вывод терминала (S0-decisions №27).
    pub panels_hidden: bool,
    /// Режимы панелей (S2-18) по сторонам: «Дерево», «Инфо», быстрый просмотр.
    pub tree: [Option<crate::views::TreeState>; 2],
    pub info: [Option<crate::views::InfoState>; 2],
    /// История полей ввода (№70) и её файл (`None` — без диска: тесты, `--screens`).
    pub fields: crate::field_history::FieldHistory,
    pub fields_file: Option<std::path::PathBuf>,
    /// Список дополнений открытого поля.
    pub complete: Option<Complete>,
    /// Приветствие внизу (первый запуск, обновление).
    pub welcome: Option<Welcome>,
    /// Ссылки в панелях (по сторонам).
    pub links: [Links; 2],
    /// Открытая вкладка настроек и её раздел (D-08).
    pub settings: Option<CompactString>,
    /// Каталоги конфига и журнала — для «О программе».
    pub config_dir: Option<std::path::PathBuf>,
    pub log_dir: Option<std::path::PathBuf>,
}

impl Default for UiState {
    fn default() -> Self {
        Self::new()
    }
}

impl UiState {
    pub fn new() -> Self {
        Self {
            zoom: 100,
            split: 0.5,
            overlay: None,
            menu: None,
            menu_side: None,
            subdirs: None,
            draft: None,
            rename: None,
            path_edit: None,
            path_check: None,
            next_gen: 0,
            prompt_hidden: SmallVec::new(),
            jobs_expanded: false,
            theme: crate::theme::AUTO.into(),
            density: 24,
            icons: crate::config::IconMode::Mono,
            user_themes: UserThemes::default(),
            policy: ThemePolicy::default(),
            themes_dir: None,
            free: None,
            drives: None,
            columns: None,
            view_modes: Vec::new(),
            panels_hidden: false,
            tree: [None, None],
            info: [None, None],
            fields: crate::field_history::FieldHistory::default(),
            fields_file: None,
            complete: None,
            welcome: None,
            links: Default::default(),
            settings: None,
            config_dir: None,
            log_dir: None,
        }
    }

    /// Закрыть оверлей и всё, что к нему относится.
    pub(crate) fn close_overlay(&mut self) {
        self.overlay = None;
        self.menu = None;
        self.menu_side = None;
        self.subdirs = None;
        self.drives = None;
        self.draft = None;
        self.rename = None;
        self.path_edit = None;
        self.path_check = None;
        self.complete = None;
    }

    /// Новое поколение фонового запроса.
    pub(crate) fn bump(&mut self) -> u64 {
        self.next_gen += 1;
        self.next_gen
    }
}
