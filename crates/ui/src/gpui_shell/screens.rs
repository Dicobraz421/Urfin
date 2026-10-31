//! `--screens <каталог>`: снимки экранов без человека — дизайн проверяется по PNG,
//! а не нажатиями в окне (как `--screens` Postman).
//!
//! Сцены идут по очереди, как шаги замера: команды Core → ждём, пока листинги
//! панелей загружены, и ещё несколько кадров → снимок `PrintWindow` в фоновом
//! потоке → следующая сцена. На снимок — строка `screen <имя> <путь> <ш>x<в>` в
//! stdout, в конце — выход; ошибка (снимок не удался, одного цвета, сцена не
//! готова) — stderr и код 1. Сообщения — по-английски: это диагностика.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use urfin_core::{Command, ListingStatus, Side};

/// Кадров с готовой сценой до снимка: первые кадры после смены состояния ещё прежние.
const SETTLE_FRAMES: u32 = 3;
const SCENE_TIMEOUT: Duration = Duration::from_secs(20);

/// Сцена: имя файла и команды. `resize WxH` — размер окна, `wait prompt` —
/// снимок, когда открыт вопрос (job спрашивает асинхронно); это не команды Core.
pub(super) struct Scene {
    pub name: &'static str,
    pub steps: &'static [&'static str],
}

/// Сцены по порядку: общие отрезки здесь, сцены фич — в их модулях (ADR-0022 §3).
/// Сцены идут подряд и опираются на состояние после предыдущих.
const ORDER: &[&[Scene]] = &[
    PANELS,
    super::jobs::SCENES,
    OPS,
    super::compare::SCENES,
    super::settings::SCENES,
    super::welcome::SCENES,
    FRAME,
    super::view::SCENES,
    super::term_view::SCENES,
    TAIL,
];

/// Все сцены подряд.
fn scenes() -> impl Iterator<Item = &'static Scene> {
    ORDER.iter().flat_map(|part| part.iter())
}

/// Панели, темы, строки операций, состояния папок.
const PANELS: &[Scene] = &[
    Scene {
        name: "files",
        steps: &[],
    },
    // Темы (D-01, №59): имена — как у снимков docs/design/screens/theme-*.png.
    Scene {
        name: "theme-system-light",
        steps: &["ui.theme.set id=system-light"],
    },
    Scene {
        name: "theme-system-dark",
        steps: &["ui.theme.set id=system-dark"],
    },
    Scene {
        name: "theme-blue",
        steps: &["ui.theme.set id=blue"],
    },
    Scene {
        name: "theme-turquoise",
        steps: &["ui.theme.set id=turquoise"],
    },
    Scene {
        name: "theme-grass",
        steps: &["ui.theme.set id=grass"],
    },
    Scene {
        name: "theme-sepia",
        steps: &["ui.theme.set id=sepia"],
    },
    // Полоса контекста (D-02, T-01-5): как при запуске от администратора.
    Scene {
        name: "context-admin",
        steps: &["ui.theme.set id=system-light", "context admin"],
    },
    // Плотность 20 и цветные иконки (D-03): имя — как у снимка дизайна.
    Scene {
        name: "density-20-color-icons",
        steps: &[
            "context none",
            "ui.density.set px=20",
            "ui.icons.set mode=color",
        ],
    },
    Scene {
        name: "marked",
        steps: &[
            "ui.density.set px=24",
            "ui.icons.set mode=mono",
            "ui.theme.follow_os",
            "sel.add mask=\"*.md;*.pdf;*.jpg\"",
            "panel.cursor.down n=3",
        ],
    },
    Scene {
        name: "screen-brief",
        steps: &["panel.view_mode mode=brief"],
    },
    Scene {
        name: "medium",
        steps: &["panel.view_mode mode=medium"],
    },
    Scene {
        name: "wide",
        steps: &["panel.view_mode mode=wide"],
    },
    // Меню и «мешочки» (D-05): имена — как у снимков дизайна.
    Scene {
        name: "screen-menu",
        steps: &["panel.view_mode mode=full", "ui.menu menu=files"],
    },
    Scene {
        name: "screen-sort",
        steps: &["ui.cancel", "ui.menu.sort"],
    },
    Scene {
        name: "screen-bookmarks",
        steps: &[
            "ui.cancel",
            "panel.bookmark.set n=1",
            "panel.cursor.to name=Projects",
            "panel.enter",
            "panel.bookmark.set n=2",
            "panel.back",
            "ui.menu menu=bookmarks",
        ],
    },
    Scene {
        name: "screen-history",
        steps: &["ui.cancel", "ui.history.dirs"],
    },
    Scene {
        name: "screen-context",
        steps: &[
            "ui.cancel",
            "panel.cursor.to name=Cargo.toml",
            "ui.menu menu=context",
        ],
    },
    Scene {
        name: "screen-copy",
        steps: &["ui.cancel", "ops.copy"],
    },
    // Дополнение пути (№70): Tab в поле «куда» — подкаталоги и история.
    Scene {
        name: "complete-path",
        steps: &[
            "ui.cancel",
            "ui.draft command=ops.copy arg=dest init=P",
            "ui.complete kind=path text=P",
        ],
    },
    Scene {
        name: "screen-move",
        steps: &["ui.cancel", "ops.move"],
    },
    Scene {
        name: "screen-delete",
        steps: &["ui.cancel", "ops.delete"],
    },
    Scene {
        name: "screen-mkdir",
        steps: &[
            "ui.cancel",
            "ui.draft command=ops.mkdir arg=name init=release-notes",
        ],
    },
    // Отметка группы по маске (D-04): совпавшие имена подсвечены.
    Scene {
        name: "screen-select",
        steps: &[
            "ui.cancel",
            "ui.draft command=sel.add arg=mask init=\"*.toml;*.json\"",
        ],
    },
    // Атрибуты (Ctrl+A) двух файлов.
    Scene {
        name: "screen-attrs",
        steps: &[
            "ui.cancel",
            "sel.none",
            "sel.add mask=\"Cargo.toml;data.json\"",
            "ops.attrs",
        ],
    },
    // Фильтр (Ctrl+I): строка, затем панель с фильтром и чипом у пути.
    Scene {
        name: "screen-filter",
        steps: &[
            "ui.cancel",
            "sel.none",
            "ui.draft command=panel.filter arg=mask init=\"*.rs;*.toml\"",
        ],
    },
    Scene {
        name: "screen-filtered",
        steps: &["ui.cancel", "panel.filter mask=\"*.rs;*.toml\""],
    },
    Scene {
        name: "screen-rename",
        steps: &[
            "ui.cancel",
            // Снять фильтр прошлой сцены (Cargo.toml виден и с ним, и без).
            "panel.filter mask=\"\"",
            "sel.none",
            "panel.cursor.to name=Cargo.toml",
            "ui.rename",
        ],
    },
    Scene {
        name: "screen-quick",
        steps: &["ui.cancel", "keymap.qs.start text=re"],
    },
    Scene {
        name: "toast",
        steps: &["keymap.qs.cancel", "panel.cursor.home", "ops.copy"],
    },
    Scene {
        name: "screen-info",
        steps: &["panel.mode mode=info"],
    },
    Scene {
        name: "screen-tree",
        steps: &["panel.mode mode=tree"],
    },
    // Быстрый просмотр (S7-13): курсор на Cargo.toml, справа — вьювер в области панели.
    Scene {
        name: "screen-quickview",
        steps: &["panel.cursor.to name=Cargo.toml", "panel.mode mode=qview"],
    },
    // Markdown в быстром просмотре — документом (≤ 4 МиБ).
    Scene {
        name: "quickview-md",
        steps: &["panel.cursor.to name=README.md"],
    },
    Scene {
        name: "screen-drives",
        steps: &["panel.mode mode=files", "ui.drives side=left"],
    },
    // Состояния папки (D-03): нет доступа (папка ОС только для администраторов) и нет
    // связи (несуществующий ресурс на своём SMB — ответ сразу); затем назад.
    Scene {
        name: "screen-errors",
        steps: &[
            "ui.cancel",
            "panel.go side=left path=\"C:\\System Volume Information\"",
            "panel.go side=right path=\"\\\\127.0.0.1\\urfin-missing\"",
        ],
    },
];

/// Ссылки, даты, дерево папок, групповое переименование.
const OPS: &[Scene] = &[
    Scene {
        name: "screen-link",
        steps: &[
            "screens.banner stop=true",
            "ui.cancel",
            "sel.none",
            "panel.cursor.to name=Projects",
            "ops.link",
        ],
    },
    Scene {
        name: "screen-touch",
        steps: &["ui.cancel", "sel.add mask=\"*.toml;*.json\"", "ops.touch"],
    },
    Scene {
        name: "screen-mkdirtree",
        steps: &[
            "ui.cancel",
            "sel.none",
            "ui.draft command=ops.mkdir arg=name init=\"Фото\\2026\\10\"",
        ],
    },
    Scene {
        name: "screen-multirename",
        steps: &[
            "panel.focus side=left",
            "sel.add mask=\"*.md;*.pdf;*.jpg;*.txt\"",
            "ui.draft command=ops.multi_rename arg=pattern init=\"[N]_[C:1:1:2].[E]\"",
        ],
    },
];

/// Каркас (D-02, D-03): ссылки, много табов.
const FRAME: &[Scene] = &[
    Scene {
        name: "link-make",
        steps: &[
            "ui.welcome.done",
            "panel.focus side=left",
            "panel.cursor.to name=Projects",
            "ops.link kind=junction dest=old-builds",
        ],
    },
    Scene {
        name: "screen-linkview",
        steps: &["panel.refresh", "panel.cursor.to name=old-builds"],
    },
    Scene {
        name: "screen-tabsmany",
        steps: &[
            "panel.focus side=left",
            "tab.dup",
            "panel.cursor.to name=Projects",
            "panel.enter",
            "tab.dup",
            "panel.back",
            "panel.cursor.to name=Документы",
            "panel.enter",
            "tab.dup",
            "panel.back",
            "panel.cursor.to name=Фото",
            "panel.enter",
            "tab.dup",
            "panel.back",
            "panel.cursor.to name=.git",
            "panel.enter",
            "tab.dup",
            "panel.back",
            "panel.cursor.to name=Projects",
            "panel.enter",
            "tab.dup",
            "panel.back",
        ],
    },
    Scene {
        name: "screen-tabsmore",
        steps: &["ui.menu menu=tabs-left"],
    },
];

/// Командная строка, ширины окна, конфликт.
const TAIL: &[Scene] = &[
    // Набранная строка (S6-15): курсор-блок посреди строки, на слове.
    Scene {
        name: "cmdline-typed",
        steps: &[
            "screens.term stop=true",
            "cmdline.insert text=\"dir /b *.toml\"",
            "cmdline.word.left",
        ],
    },
    Scene {
        name: "width-800-panels",
        steps: &[
            "cmdline.clear",
            "ui.cancel",
            "tab.close",
            "tab.close",
            "tab.close",
            "tab.close",
            "tab.close",
            "tab.close",
            "resize 800x760",
        ],
    },
    Scene {
        name: "narrow",
        steps: &[
            "panel.mode mode=files",
            "ui.theme.set id=system-light",
            "resize 720x600",
        ],
    },
    // Последней: вопрос о конфликте остаётся открытым (S5-07).
    Scene {
        name: "screen-conflict",
        steps: &[
            "ui.cancel",
            "sel.none",
            "panel.focus side=left",
            "panel.cursor.to name=Cargo.toml",
            // Размер как у прочих сцен (до `narrow`), чтобы сверять с макетом.
            "resize 1200x760",
            // Назначение относительно левой панели (корень демо-дерева).
            "ops.copy current=true dest=Projects",
            WAIT_PROMPT,
        ],
    },
];

const WAIT_PROMPT: &str = "wait prompt";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Enter,
    Wait,
    Shooting,
}

/// Ход снимков.
#[derive(Debug)]
pub struct Screens {
    dir: PathBuf,
    scene: usize,
    phase: Phase,
    settle: u32,
    since: Instant,
}

/// Что окну сделать в этом кадре.
/// Шаг `context …`: какой контекст окна показать (`None` — обычный).
#[derive(Clone, Copy, Debug)]
pub(super) struct SetContext(pub Option<crate::sys::Context>);

pub(super) enum Step {
    /// Команды Core (уже разобранные), может быть, размер окна и контекст окна
    /// (`context admin|remote|none` — сцена полосы контекста без запуска от админа).
    Enter(Vec<Command>, Option<(f32, f32)>, Option<SetContext>),
    /// Ждать следующего кадра.
    Next,
    /// Снять сцену в этот файл.
    Shoot(PathBuf),
    /// Ничего: снимок в работе.
    Idle,
}

impl Screens {
    pub fn new(dir: PathBuf) -> Screens {
        Screens {
            dir,
            scene: 0,
            phase: Phase::Enter,
            settle: 0,
            since: Instant::now(),
        }
    }

    fn name(&self) -> &'static str {
        self.current().name
    }

    fn current(&self) -> &'static Scene {
        let n = scenes().count();
        scenes().nth(self.scene.min(n - 1)).unwrap_or(&PANELS[0])
    }

    /// Кадр нарисован; `ready` — листинги обеих панелей загружены.
    pub(super) fn frame(&mut self, ready: bool) -> Step {
        match self.phase {
            Phase::Enter => {
                self.phase = Phase::Wait;
                self.settle = 0;
                self.since = Instant::now();
                let mut cmds = Vec::new();
                let mut size = None;
                let mut context = None;
                for s in self.current().steps {
                    if let Some(c) = s.strip_prefix("context ") {
                        context = Some(SetContext(match c {
                            "admin" => Some(crate::sys::Context::Admin),
                            "remote" => Some(crate::sys::Context::Remote),
                            _ => None,
                        }));
                        continue;
                    }
                    if *s == WAIT_PROMPT {
                        continue;
                    }
                    if let Some(wh) = s.strip_prefix("resize ") {
                        size = parse_size(wh);
                        continue;
                    }
                    match Command::parse(s) {
                        Ok(c) => cmds.push(c),
                        Err(e) => fail(self.name(), &format!("bad step {s}: {e}")),
                    }
                }
                Step::Enter(cmds, size, context)
            }
            Phase::Wait => {
                if self.since.elapsed() > SCENE_TIMEOUT {
                    fail(self.name(), "scene not ready");
                }
                self.settle = if ready { self.settle + 1 } else { 0 };
                if self.settle < SETTLE_FRAMES {
                    return Step::Next;
                }
                self.phase = Phase::Shooting;
                Step::Shoot(self.dir.join(format!("{}.png", self.name())))
            }
            Phase::Shooting => Step::Idle,
        }
    }

    /// Снимок готов: строка в stdout, следующая сцена. `true` — сцены кончились.
    pub(super) fn shot(&mut self, result: Result<(PathBuf, u32, u32), String>) -> bool {
        match result {
            Ok((path, w, h)) => println!("screen {} {} {w}x{h}", self.name(), path.display()),
            Err(e) => fail(self.name(), &e),
        }
        self.scene += 1;
        self.phase = Phase::Enter;
        self.scene >= scenes().count()
    }
}

impl Screens {
    /// Сцена готова к снимку: обе панели показывают загруженный листинг (и, если
    /// сцена ждёт, открыт вопрос).
    pub(super) fn ready(&self, s: &urfin_core::AppState) -> bool {
        let prompt = !self.current().steps.contains(&WAIT_PROMPT) || s.prompts.active().is_some();
        prompt
            && super::view::settled(s)
            && [Side::Left, Side::Right].into_iter().all(|side| {
                matches!(
                    s.panel(side).tab().listing.status,
                    ListingStatus::Complete | ListingStatus::Failed(_)
                )
            })
    }
}

fn parse_size(wh: &str) -> Option<(f32, f32)> {
    let (w, h) = wh.split_once('x')?;
    Some((w.trim().parse().ok()?, h.trim().parse().ok()?))
}

/// Снять окно и записать PNG (фоновый поток).
pub(super) fn shoot(hwnd: isize, path: &Path) -> Result<(PathBuf, u32, u32), String> {
    #[cfg(windows)]
    {
        let img = crate::sys::capture::capture(hwnd)?;
        if img
            .rgba
            .as_chunks::<4>()
            .0
            .iter()
            .all(|p| p[..] == img.rgba[..4])
        {
            return Err(format!(
                "capture {}x{} is a single color: the window is not drawn",
                img.width, img.height
            ));
        }
        write_png(path, img.width, img.height, &img.rgba)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        Ok((path.to_path_buf(), img.width, img.height))
    }
    #[cfg(not(windows))]
    {
        let _ = (hwnd, path);
        Err("screens: Windows only".into())
    }
}

fn write_png(path: &Path, w: u32, h: u32, rgba: &[u8]) -> Result<(), png::EncodingError> {
    let file = std::io::BufWriter::new(std::fs::File::create(path)?);
    let mut enc = png::Encoder::new(file, w, h);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    let mut wr = enc.write_header()?;
    wr.write_image_data(rgba)?;
    wr.finish()
}

fn fail(scene: &str, e: &str) -> ! {
    eprintln!("screens: {scene}: {e}");
    std::process::exit(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Шаги всех сцен — разбираемые команды (опечатка ловится тестом, а не на стенде).
    #[test]
    fn scene_steps_parse() {
        for sc in scenes() {
            for s in sc.steps {
                if *s == WAIT_PROMPT {
                    continue;
                }
                if let Some(wh) = s.strip_prefix("resize ") {
                    assert!(parse_size(wh).is_some(), "{}: {s}", sc.name);
                } else if let Some(c) = s.strip_prefix("context ") {
                    assert!(["admin", "remote", "none"].contains(&c), "{}: {s}", sc.name);
                } else {
                    Command::parse(s).unwrap_or_else(|e| panic!("{}: {s}: {e}", sc.name));
                }
            }
        }
    }
}
