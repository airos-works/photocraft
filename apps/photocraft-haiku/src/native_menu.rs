//! The native menu bar (`Services::native_menu`): a `BMenuBar` at the top of the window instead of
//! the in-window menus. What the menus hold, and when they change, is upstream's
//! (`photocraft_ui_egui::native_menu`); this file lays them out the Haiku way and keeps the
//! `BMenuBar` in step (`pc_menu_build`, `pc_menu_update` in host.cpp).
//!
//! - **Layout.** Upstream hands over a Mac menu bar: an app menu named PhotoCraft holding About,
//!   Settings, Language, Appearance, Hide and Quit. A Haiku app has no app menu, so they go back
//!   where Haiku apps keep them: About at the end of Help, Settings (Edit › Preferences' pages) at
//!   the end of Edit, Language and Appearance at the end of Window, Quit at the end of File.
//!   AppKit's own items (Services, Hide Others, Show All, Bring All to Front) are dropped;
//!   Minimize, Zoom and Close Window are the window's own.
//! - **Keys stay with the interface**, as upstream's contract asks: the window hands every
//!   Command key to the canvas view before `BWindow` looks for a menu shortcut (`DispatchMessage`
//!   in host.cpp), so the menus only show the shortcuts. Those with Command (`Cmd+…`) and one
//!   character are drawn by `BMenuItem` itself; the others (a function key, Page Up, a tool's bare
//!   letter) as text, without becoming window shortcuts. Command+Q still reaches `BWindow`,
//!   which runs the item showing it (File › Quit).
//! - A structure change rebuilds the whole bar (fast enough with a few hundred items); labels,
//!   enabled and checked are updated in place, only for the items that changed.
//! - `PHOTOCRAFT_NATIVE_MENU=0` keeps the in-window menus.

use std::cell::RefCell;
use std::fmt::Write as _;
use std::rc::Rc;

use photocraft_ui_egui::native_menu::{self, Backend, Chord, Event, ItemRole, Menu, MenuBar, MenuRole, Node, Standard};

/// `B_SHIFT_KEY`, `B_COMMAND_KEY`, `B_CONTROL_KEY`, `B_OPTION_KEY` (`<InterfaceDefs.h>`).
const SHIFT: u32 = 0x1;
const COMMAND: u32 = 0x2;
const CONTROL: u32 = 0x4;
const OPTION: u32 = 0x40;

/// Where the bar goes: the host in the app, a recorder in tests.
pub trait Sink {
    fn build(&mut self, spec: &str);
    fn update(&mut self, spec: &str);
}

/// The window's menu bar.
#[cfg(target_os = "haiku")]
pub struct HostSink;

#[cfg(target_os = "haiku")]
impl Sink for HostSink {
    fn build(&mut self, spec: &str) {
        crate::host::menu_build(spec);
    }

    fn update(&mut self, spec: &str) {
        crate::host::menu_update(spec);
    }
}

/// One item of the bar, by its index in the spec.
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    command: String,
    label: String,
    enabled: bool,
    checked: Option<bool>,
}

/// Clicks reported by the window ([`crate::host::kind::MENU`]), by item index.
pub type Clicks = Rc<RefCell<Vec<i32>>>;

pub struct HaikuMenu<S: Sink> {
    sink: S,
    clicks: Clicks,
    /// Upstream's structure and Window-menu keys of the bar last built.
    built: Option<(u64, u64)>,
    entries: Vec<Entry>,
}

impl<S: Sink> HaikuMenu<S> {
    pub fn new(sink: S, clicks: Clicks) -> Self {
        Self { sink, clicks, built: None, entries: Vec::new() }
    }
}

impl<S: Sink> Backend for HaikuMenu<S> {
    fn sync(&mut self, bar: &MenuBar) {
        let keys = (native_menu::structure_key(bar), native_menu::window_key(bar));
        let bar = haiku_layout(bar);
        if self.built != Some(keys) {
            let (spec, entries) = spec(&bar);
            self.sink.build(&spec);
            self.entries = entries;
            self.built = Some(keys);
            // Clicks on the old bar's items no longer mean anything.
            self.clicks.borrow_mut().clear();
            return;
        }
        let mut changes = String::new();
        let items = bar.items();
        for (i, (e, it)) in self.entries.iter_mut().zip(items).enumerate() {
            if e.command != it.id {
                continue;
            }
            if e.label != it.label || e.enabled != it.enabled || e.checked != it.checked {
                let _ = writeln!(changes, "{i}\t{}\t{}\t{}", field(&it.label), i32::from(it.enabled), checked(it.checked));
                e.label.clone_from(&it.label);
                e.enabled = it.enabled;
                e.checked = it.checked;
            }
        }
        if !changes.is_empty() {
            self.sink.update(&changes);
        }
    }

    fn drain(&mut self) -> Vec<Event> {
        let clicks = std::mem::take(&mut *self.clicks.borrow_mut());
        clicks.into_iter().filter_map(|i| usize::try_from(i).ok().and_then(|i| self.entries.get(i))).map(|e| Event::Click(e.command.clone())).collect()
    }
}

/// Upstream's Mac menu bar laid out the Haiku way (see the module docs).
pub fn haiku_layout(bar: &MenuBar) -> MenuBar {
    let mut menus = bar.menus.clone();
    if let Some(app) = menus.iter().position(|m| m.role == MenuRole::App).map(|i| menus.remove(i)) {
        let (mut about, mut settings, mut window, mut quit) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        for n in app.children {
            let role = match &n {
                Node::Item(it) => it.role,
                Node::Submenu { role, .. } => *role,
                _ => None,
            };
            match role {
                Some(ItemRole::About) => about.push(n),
                Some(ItemRole::Settings) => settings.push(n),
                Some(ItemRole::Language | ItemRole::Appearance) => window.push(n),
                Some(ItemRole::Quit) => quit.push(n),
                _ => {}
            }
        }
        // The titles are translated: File and Edit are the first two plain menus.
        let normal: Vec<usize> = menus.iter().enumerate().filter(|(_, m)| m.role == MenuRole::Normal).map(|(i, _)| i).collect();
        let role = |r: MenuRole| menus.iter().position(|m| m.role == r);
        let places = [(normal.get(1).copied(), settings), (role(MenuRole::Window), window), (role(MenuRole::Help), about), (normal.first().copied(), quit)];
        for (at, nodes) in places {
            if let Some(m) = at.and_then(|i| menus.get_mut(i))
                && !nodes.is_empty()
            {
                m.children.push(Node::Separator);
                m.children.extend(nodes);
            }
        }
    }
    for m in &mut menus {
        m.children = tidy(haiku_nodes(std::mem::take(&mut m.children)));
    }
    menus.retain(|m| !m.children.is_empty());
    MenuBar { menus }
}

/// AppKit's own items go; the window's (Minimize, Zoom, Close Window) stay.
fn haiku_nodes(nodes: Vec<Node>) -> Vec<Node> {
    nodes
        .into_iter()
        .filter_map(|n| match n {
            Node::Standard(Standard::Services | Standard::HideOthers | Standard::ShowAll | Standard::BringAllToFront) => None,
            Node::Item(it) if it.id == native_menu::HIDE => None,
            Node::Submenu { label, children, role } => Some(Node::Submenu { label, children: tidy(haiku_nodes(children)), role }),
            other => Some(other),
        })
        .collect()
}

/// No separators at either end of a menu, or two in a row.
fn tidy(nodes: Vec<Node>) -> Vec<Node> {
    let mut out: Vec<Node> = Vec::with_capacity(nodes.len());
    for n in nodes {
        if matches!(n, Node::Separator) && matches!(out.last(), None | Some(Node::Separator)) {
            continue;
        }
        out.push(n);
    }
    while matches!(out.last(), Some(Node::Separator)) {
        out.pop();
    }
    out
}

/// Text for one field of a spec line.
fn field(s: &str) -> String {
    s.replace(['\t', '\n', '\r'], " ")
}

fn checked(c: Option<bool>) -> i32 {
    c.map_or(-1, i32::from)
}

/// A shortcut as the menu shows it: a character and modifiers `BMenuItem` draws (Command
/// shortcuts with one character), or text.
pub fn shortcut(s: &str) -> (u8, u32, String) {
    let Some(c) = Chord::parse(s) else { return (0, 0, String::new()) };
    if c.cmd {
        let ch = match c.key.as_str() {
            k if k.len() == 1 && k.is_ascii() && !k.chars().all(char::is_whitespace) => k.as_bytes()[0].to_ascii_uppercase(),
            "Delete" => 0x7f,
            "Backspace" => 0x08,
            "Enter" => 0x0a,
            "Tab" => 0x09,
            "Space" => 0x20,
            _ => 0,
        };
        if ch != 0 {
            let mut mods = COMMAND;
            for (on, bit) in [(c.ctrl, CONTROL), (c.alt, OPTION), (c.shift, SHIFT)] {
                if on {
                    mods |= bit;
                }
            }
            return (ch, mods, String::new());
        }
    }
    (0, 0, photocraft_ui_egui::shortcuts::pretty(&c.portable()))
}

/// The host's spec of `bar` (`BuildMenuBar` in host.cpp), and its items by index.
pub fn spec(bar: &MenuBar) -> (String, Vec<Entry>) {
    fn walk(out: &mut String, entries: &mut Vec<Entry>, nodes: &[Node]) {
        for n in nodes {
            match n {
                Node::Separator => out.push_str("-\n"),
                Node::Submenu { label, children, .. } => {
                    let _ = writeln!(out, "S\t{}", field(label));
                    walk(out, entries, children);
                    out.push_str("E\n");
                }
                Node::Standard(s) => {
                    let (kind, label) = match s {
                        Standard::Minimize => ("min", "Minimize"),
                        Standard::Zoom => ("zoom", "Zoom"),
                        Standard::CloseWindow => ("close", "Close Window"),
                        _ => continue,
                    };
                    let _ = writeln!(out, "W\t{kind}\t{}", field(photocraft_ui_egui::i18n::t(label)));
                }
                Node::Item(it) => {
                    let (ch, mods, text) = it.shortcut.as_deref().map(shortcut).unwrap_or_default();
                    let _ = writeln!(
                        out,
                        "I\t{}\t{}\t{ch}\t{mods}\t{}\t{}\t{}",
                        entries.len(),
                        field(&it.label),
                        field(&text),
                        i32::from(it.enabled),
                        checked(it.checked)
                    );
                    entries.push(Entry { command: it.id.clone(), label: it.label.clone(), enabled: it.enabled, checked: it.checked });
                }
            }
        }
    }
    let mut out = String::new();
    let mut entries = Vec::new();
    for Menu { title, children, .. } in &bar.menus {
        let _ = writeln!(out, "M\t{}", field(title));
        walk(&mut out, &mut entries, children);
        out.push_str("E\n");
    }
    (out, entries)
}

#[cfg(test)]
mod tests {
    use super::*;
    use photocraft_ui_egui::native_menu::Item;

    fn item(id: &str, label: &str, shortcut: Option<&str>, role: Option<ItemRole>) -> Node {
        Node::Item(Item { id: id.into(), label: label.into(), shortcut: shortcut.map(Into::into), enabled: true, checked: None, role })
    }

    fn mac_bar() -> MenuBar {
        let menu = |title: &str, role, children| Menu { title: title.into(), role, children };
        MenuBar {
            menus: vec![
                menu(
                    "PhotoCraft",
                    MenuRole::App,
                    vec![
                        item("help.about", "About PhotoCraft", None, Some(ItemRole::About)),
                        Node::Separator,
                        Node::Submenu { label: "Settings".into(), children: vec![item("prefs.general", "General…", Some("Cmd+K"), None)], role: Some(ItemRole::Settings) },
                        Node::Submenu { label: "Language".into(), children: vec![], role: Some(ItemRole::Language) },
                        Node::Separator,
                        Node::Standard(Standard::Services),
                        Node::Separator,
                        item(native_menu::HIDE, "Hide PhotoCraft", Some("Cmd+H"), Some(ItemRole::Hide)),
                        Node::Standard(Standard::HideOthers),
                        Node::Standard(Standard::ShowAll),
                        Node::Separator,
                        item("file.exit", "Quit PhotoCraft", Some("Cmd+Q"), Some(ItemRole::Quit)),
                    ],
                ),
                menu("File", MenuRole::Normal, vec![item("file.new", "New…", Some("Cmd+N"), None), Node::Separator, Node::Standard(Standard::CloseWindow)]),
                menu("Edit", MenuRole::Normal, vec![item("edit.undo", "Undo", Some("Cmd+Z"), None)]),
                menu(
                    "Window",
                    MenuRole::Window,
                    vec![Node::Standard(Standard::Minimize), Node::Standard(Standard::Zoom), Node::Separator, Node::Standard(Standard::BringAllToFront)],
                ),
                menu("Help", MenuRole::Help, vec![item("help.docs", "Help", Some("F1"), None)]),
            ],
        }
    }

    #[test]
    fn the_app_menu_goes_back_where_haiku_apps_keep_its_items() {
        let bar = haiku_layout(&mac_bar());
        let titles: Vec<&str> = bar.menus.iter().map(|m| m.title.as_str()).collect();
        assert_eq!(titles, ["File", "Edit", "Window", "Help"]);
        let ids = |title: &str| -> Vec<String> {
            let m = bar.menus.iter().find(|m| m.title == title).unwrap();
            m.children
                .iter()
                .map(|n| match n {
                    Node::Item(it) => it.id.clone(),
                    Node::Submenu { label, .. } => format!("[{label}]"),
                    Node::Separator => "-".into(),
                    Node::Standard(s) => format!("{s:?}"),
                })
                .collect()
        };
        assert_eq!(ids("File"), ["file.new", "-", "CloseWindow", "-", "file.exit"]);
        assert_eq!(ids("Edit"), ["edit.undo", "-", "[Settings]"]);
        // Bring All to Front is AppKit's; the empty Language menu is still a submenu.
        assert_eq!(ids("Window"), ["Minimize", "Zoom", "-", "[Language]"]);
        assert_eq!(ids("Help"), ["help.docs", "-", "help.about"]);
        assert!(bar.find(native_menu::HIDE).is_none());
    }

    #[test]
    fn command_shortcuts_are_native_and_the_others_text() {
        assert_eq!(shortcut("Cmd+Shift+Z"), (b'Z', COMMAND | SHIFT, String::new()));
        assert_eq!(shortcut("Cmd+Alt+Ctrl+["), (b'[', COMMAND | CONTROL | OPTION, String::new()));
        assert_eq!(shortcut("Cmd+Delete"), (0x7f, COMMAND, String::new()));
        assert_eq!(shortcut("Cmd+F5").2, "Ctrl+F5");
        assert_eq!(shortcut("Shift+F5"), (0, 0, "Shift+F5".into()));
        // A tool's bare key must not become a Command shortcut (B would be Command+B).
        assert_eq!(shortcut("B"), (0, 0, "B".into()));
        assert_eq!(shortcut("Ctrl+Q"), (0, 0, "Ctrl+Q".into()));
    }

    #[derive(Default)]
    struct Recorder(Rc<RefCell<Vec<(bool, String)>>>);
    impl Sink for Recorder {
        fn build(&mut self, spec: &str) {
            self.0.borrow_mut().push((true, spec.into()));
        }
        fn update(&mut self, spec: &str) {
            self.0.borrow_mut().push((false, spec.into()));
        }
    }

    #[test]
    fn builds_once_then_updates_only_what_changed_and_reports_clicks() {
        let log = Rc::new(RefCell::new(Vec::new()));
        let clicks: Clicks = Rc::default();
        let mut menu = HaikuMenu::new(Recorder(log.clone()), clicks.clone());
        let mut bar = mac_bar();
        menu.sync(&bar);
        menu.sync(&bar);
        {
            let log = log.borrow();
            assert_eq!(log.len(), 1, "an unchanged bar isn't sent again");
            let spec = &log[0].1;
            assert!(spec.starts_with("M\tFile\nI\t0\tNew…\t78\t2\t\t1\t-1\n-\nW\tclose\t"), "{spec}");
            assert!(spec.contains("I\t1\tQuit PhotoCraft\t81\t2\t\t1\t-1\n"), "{spec}");
            assert!(spec.contains("S\tSettings\nI\t3\tGeneral…\t75\t2\t\t1\t-1\nE\n"), "{spec}");
            assert!(spec.contains("I\t4\tHelp\t0\t0\tF1\t1\t-1\n"), "{spec}");
        }
        // Edit › Undo becomes disabled and relabelled: one in-place update.
        if let Node::Item(it) = &mut bar.menus[2].children[0] {
            it.enabled = false;
            it.label = "Undo Brush".into();
        }
        menu.sync(&bar);
        assert_eq!(log.borrow().last().cloned(), Some((false, "2\tUndo Brush\t0\t-1\n".into())));
        clicks.borrow_mut().extend([1, 99, 2]);
        assert_eq!(menu.drain(), [Event::Click("file.exit".into()), Event::Click("edit.undo".into())]);
        // A new item rebuilds the bar.
        bar.menus[1].children.push(item("file.open", "Open…", Some("Cmd+O"), None));
        menu.sync(&bar);
        assert!(log.borrow().last().unwrap().0);
    }
}
