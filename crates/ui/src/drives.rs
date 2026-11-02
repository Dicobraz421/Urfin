//! Диски, сеть и места (`ui.drives side=`, Alt+F1/F2; D-03, `screen-drives.png`):
//! всплывающий список под путём панели. Корни читает задача Interactive
//! (`LocalFs::roots`, метки и место — по потоку на том, не дольше `ROOTS_TIMEOUT`);
//! места — известные папки пользователя. В кадре I/O нет.

use urfin_core::{Command, Cx, Focus, Lane, Location, Msg, OverlayKind, Side};
use urfin_vfs::{LocalFs, RootKind};

use crate::handler::ui_state;

/// Раздел списка.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Section {
    Drives,
    Network,
    /// Профили SSH (F03 §2.8, `screen-sshdrives`) — между «Сетью» и «Местами».
    Ssh,
    Places,
}

/// Строка списка: диск, сеть или место; профиль SSH; «Новое подключение…».
#[derive(Clone, Debug, PartialEq)]
pub enum Item<'a> {
    Drive(&'a Drive),
    Ssh(crate::ssh::SshRow),
    NewSsh,
}

impl Item<'_> {
    pub fn section(&self) -> Section {
        match self {
            Item::Drive(d) => d.section,
            Item::Ssh(_) | Item::NewSsh => Section::Ssh,
        }
    }
}

/// Пункт списка дисков.
#[derive(Clone, Debug, PartialEq)]
pub struct Drive {
    pub section: Section,
    /// `C:`, `\\fs01\share`, «Документы».
    pub name: String,
    /// Метка тома или путь места.
    pub sub: String,
    pub loc: Location,
    /// (свободно, всего); `None` — не ответил (сеть) или не том.
    pub space: Option<(u64, u64)>,
    /// Сетевой ресурс без ответа.
    pub offline: bool,
}

/// Открытый список дисков.
#[derive(Clone, Debug, PartialEq)]
pub struct Drives {
    pub side: Side,
    pub items: Vec<Drive>,
    pub sel: usize,
    pub loading: bool,
}

impl Drives {
    /// Сдвиг выбора по кругу.
    pub fn step(&mut self, dir: isize) {
        let n = self.items.len();
        if n == 0 {
            return;
        }
        let at = isize::try_from(self.sel).unwrap_or(0) + dir;
        self.sel = usize::try_from(at.rem_euclid(isize::try_from(n).unwrap_or(1))).unwrap_or(0);
    }

    /// Строки по порядку: диски, сеть, профили SSH и «Новое подключение…», места.
    /// Выбор `sel` при открытии указывает на диск — индексы дисков те же.
    pub fn list(&self, ssh: Vec<crate::ssh::SshRow>) -> Vec<Item<'_>> {
        let mut out: Vec<Item<'_>> = self
            .items
            .iter()
            .filter(|d| d.section != Section::Places)
            .map(Item::Drive)
            .collect();
        out.extend(ssh.into_iter().map(Item::Ssh));
        out.push(Item::NewSsh);
        out.extend(
            self.items
                .iter()
                .filter(|d| d.section == Section::Places)
                .map(Item::Drive),
        );
        out
    }

    /// Команды выбора пункта: закрыть список, перейти панелью; профиль — подключить.
    pub fn open_cmds(&self, it: Option<&Item<'_>>) -> Vec<Command> {
        let side = crate::side_arg(self.side);
        match it {
            Some(Item::Drive(d)) => vec![
                Command::new("ui.cancel"),
                Command::new("panel.focus").arg("side", side),
                Command::new("panel.go")
                    .arg("side", side)
                    .arg("path", d.loc.clone()),
            ],
            Some(Item::Ssh(r)) => crate::ssh::open_cmds(&r.profile, self.side),
            Some(Item::NewSsh) => crate::ssh::new_cmds(self.side),
            None => vec![Command::new("ui.cancel")],
        }
    }
}

/// `ui.drives side=`: открыть список и запустить чтение корней.
pub fn open(cx: &mut Cx<'_>, side: Side) {
    let st = ui_state(cx);
    st.close_overlay();
    st.overlay = Some(OverlayKind::Menu);
    st.menu = Some("drives".into());
    st.drives = Some(Drives {
        side,
        items: places(),
        sel: 0,
        loading: true,
    });
    cx.state.focus = Focus::Overlay(OverlayKind::Menu);
    // Профили SSH перечитываются при открытии меню (F03 §2.2).
    cx.dispatch(Command::new("ssh.reload"));
    cx.rt
        .spawn(Lane::Interactive, "ui.drives", cx.rt.token(), move |t| {
            let mut roots = Vec::new();
            LocalFs::default().roots(&t.cancel, |r| roots.push(r));
            let items: Vec<Drive> = roots
                .into_iter()
                .map(|r| {
                    let net = r.kind == RootKind::Network;
                    let name = r.loc.path.display().to_string();
                    let name = name.trim_end_matches(['\\', '/']).to_owned();
                    Drive {
                        section: if net {
                            Section::Network
                        } else {
                            Section::Drives
                        },
                        name,
                        sub: r.label.to_string(),
                        space: r.space.map(|s| (s.avail, s.total)),
                        offline: net && r.space.is_none(),
                        loc: r.loc,
                    }
                })
                .collect();
            let _ = t.sink.send(Box::new(RootsRead { side, items }), &t.cancel);
        });
}

/// Известные папки пользователя: «Места».
fn places() -> Vec<Drive> {
    let i18n = urfin_i18n::current();
    [
        (dirs::home_dir(), "ui-place-home"),
        (dirs::desktop_dir(), "ui-place-desktop"),
        (dirs::document_dir(), "ui-place-documents"),
        (dirs::download_dir(), "ui-place-downloads"),
    ]
    .into_iter()
    .filter_map(|(p, key)| {
        let p = p?;
        Some(Drive {
            section: Section::Places,
            name: i18n.get(key),
            sub: p.display().to_string(),
            loc: Location::local(p),
            space: None,
            offline: false,
        })
    })
    .collect()
}

struct RootsRead {
    side: Side,
    items: Vec<Drive>,
}

impl Msg for RootsRead {
    fn name(&self) -> &'static str {
        "ui.drives"
    }

    fn apply(self: Box<Self>, cx: &mut Cx<'_>) {
        let active_root = cx
            .state
            .panel(self.side)
            .tab()
            .loc
            .path
            .ancestors()
            .last()
            .map(std::path::Path::to_path_buf);
        let Some(d) = ui_state(cx).drives.as_mut().filter(|d| d.side == self.side) else {
            return;
        };
        let mut items = self.items;
        items.sort_by_key(|i| (i.section as u8, i.name.to_lowercase()));
        let places = std::mem::take(&mut d.items);
        // Выбран том текущего каталога панели.
        d.sel = items
            .iter()
            .position(|i| Some(&i.loc.path) == active_root.as_ref())
            .unwrap_or(0);
        items.extend(places);
        d.items = items;
        d.loading = false;
    }
}
