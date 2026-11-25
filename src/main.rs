//! archstore — an Arch Linux package store in your terminal.
//!
//! `archstore`                 open the store
//! `archstore <words>`         open it, already searched for <words>
//! `archstore --json <words>`  print results as JSON (for scripts)
//! `archstore --plain <words>` print results as a plain list

mod data;
mod ui;

use std::collections::HashSet;
use std::io::{self, IsTerminal, Write};
use std::process::Command;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::layout::{Position, Rect};
use ratatui::DefaultTerminal;

use data::{Detail, Job, Out, Pkg, Src};

pub const TABS: [&str; 4] = ["Search", "Top", "Updated", "Installed"];
pub const ITEM_H: usize = 2; // list rows per package card

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum LoadState {
    Idle,
    Loading,
    Ready,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ConfirmOp {
    Install,
    Remove,
}

#[derive(Default)]
pub struct Rects {
    pub header: Rect,
    pub tabs: Vec<Rect>,
    pub search: Rect,
    pub list: Rect,
    pub side: Rect,
    pub footer: Rect,
}

pub struct App {
    pub tab: usize,
    pub query: String,
    pub cursor: usize, // char index into query
    pub focus_search: bool,

    pub search_results: Vec<Pkg>,
    pub top_results: Vec<Pkg>,
    pub updated_results: Vec<Pkg>,
    pub installed_all: Vec<Pkg>,
    pub installed_shown: Vec<Pkg>,

    pub top_state: LoadState,
    pub updated_state: LoadState,
    pub installed_state: LoadState,

    pub selected: usize,
    pub offset: usize,
    pub search_loading: bool,
    pub net_down: bool,
    pub status: String,
    pub toast: Option<(String, bool, Instant)>,

    pub detail: Option<Detail>,
    pub detail_loading: bool,
    pub detail_open: bool,
    pub detail_scroll: u16,
    pub detail_target: String,

    pub confirm: Option<(Pkg, ConfirmOp)>,
    pub help: bool,
    pub installed: HashSet<String>,

    pub gen: u64,
    pub debounce: Option<Instant>,
    pub offline: bool,
    pub quit: bool,
    pub rects: Rects,
    pub spinner: usize,
}

impl App {
    fn new(offline: bool) -> Self {
        Self {
            tab: 0,
            query: String::new(),
            cursor: 0,
            focus_search: false,
            search_results: Vec::new(),
            top_results: Vec::new(),
            updated_results: Vec::new(),
            installed_all: Vec::new(),
            installed_shown: Vec::new(),
            top_state: LoadState::Idle,
            updated_state: LoadState::Idle,
            installed_state: LoadState::Idle,
            selected: 0,
            offset: 0,
            search_loading: false,
            net_down: false,
            status: "ready".into(),
            toast: None,
            detail: None,
            detail_loading: false,
            detail_open: false,
            detail_scroll: 0,
            detail_target: String::new(),
            confirm: None,
            help: false,
            installed: data::installed_names(),
            gen: 0,
            debounce: None,
            offline,
            quit: false,
            rects: Rects::default(),
            spinner: 0,
        }
    }

    pub fn active_list(&self) -> &[Pkg] {
        match self.tab {
            0 if self.query.trim().is_empty() => &self.top_results,
            0 => &self.search_results,
            1 => &self.top_results,
            2 => &self.updated_results,
            _ => &self.installed_shown,
        }
    }

    fn toast_msg(&mut self, msg: &str, ok: bool) {
        self.toast = Some((msg.to_string(), ok, Instant::now()));
    }

    fn move_sel(&mut self, delta: i32) {
        let len = self.active_list().len();
        if len == 0 {
            self.selected = 0;
            return;
        }
        let cur = self.selected.min(len - 1) as i32;
        self.selected = (cur + delta).clamp(0, len as i32 - 1) as usize;
    }

    fn selected_pkg(&self) -> Option<Pkg> {
        self.active_list().get(self.selected).cloned()
    }

    fn clamp_sel(&mut self) {
        let len = self.active_list().len();
        if self.selected >= len {
            self.selected = len.saturating_sub(1);
        }
    }

    /// Is this package present in the local database?
    pub fn is_installed(&self, p: &Pkg) -> bool {
        p.installed || self.installed.contains(&p.name)
    }

    /// May `d` remove the package currently under the cursor?
    pub fn can_remove(&self) -> bool {
        self.selected_pkg().is_some_and(|p| self.is_installed(&p))
    }

    /// Same, for the package open in the package-page overlay.
    pub fn detail_can_remove(&self) -> bool {
        self.detail_pkg().is_some_and(|p| self.is_installed(&p))
    }

    fn set_tab(&mut self, t: usize, tx: &Sender<Job>) {
        if t == self.tab {
            return;
        }
        self.tab = t;
        self.focus_search = false;
        self.selected = 0;
        self.offset = 0;
        match t {
            1 => {
                if self.top_state == LoadState::Idle {
                    self.top_state = LoadState::Loading;
                    let _ = tx.send(Job::Top);
                }
            }
            2 => {
                if self.updated_state == LoadState::Idle {
                    self.updated_state = LoadState::Loading;
                    let _ = tx.send(Job::Updated);
                }
            }
            3 => {
                if self.installed_state == LoadState::Idle {
                    self.installed_state = LoadState::Loading;
                    let _ = tx.send(Job::Installed);
                }
                self.refilter_installed();
            }
            _ => {}
        }
    }

    fn cycle_tab(&mut self, dir: i32, tx: &Sender<Job>) {
        let n = TABS.len() as i32;
        let t = ((self.tab as i32 + dir) % n + n) % n;
        self.set_tab(t as usize, tx);
    }

    fn send_search(&mut self, tx: &Sender<Job>) {
        if self.tab == 3 {
            self.refilter_installed();
            return;
        }
        let q = self.query.trim().to_string();
        if q.is_empty() {
            self.search_results.clear();
            self.search_loading = false;
            self.selected = 0;
            self.offset = 0;
            return;
        }
        self.gen += 1;
        self.search_loading = true;
        let _ = tx.send(Job::Search { gen: self.gen, q, offline: self.offline });
    }

    fn flush_search(&mut self, tx: &Sender<Job>) {
        if self.debounce.take().is_some() {
            self.send_search(tx);
        }
    }

    fn on_query_change(&mut self) {
        if self.tab == 3 {
            self.refilter_installed();
            self.selected = 0;
            self.offset = 0;
        } else {
            self.debounce = Some(Instant::now() + Duration::from_millis(350));
        }
    }

    fn refilter_installed(&mut self) {
        let q = self.query.trim().to_lowercase();
        self.installed_shown = if q.is_empty() {
            self.installed_all.clone()
        } else {
            self.installed_all
                .iter()
                .filter(|p| {
                    p.name.to_lowercase().contains(&q) || p.desc.to_lowercase().contains(&q)
                })
                .cloned()
                .collect()
        };
        if self.selected >= self.installed_shown.len() {
            self.selected = self.installed_shown.len().saturating_sub(1);
        }
    }

    fn insert_char(&mut self, c: char) {
        if self.tab == 1 || self.tab == 2 {
            self.tab = 0; // start typing anywhere → jumps into search
            self.clamp_sel();
        }
        let byte = self
            .query
            .char_indices()
            .nth(self.cursor)
            .map(|(i, _)| i)
            .unwrap_or(self.query.len());
        self.query.insert(byte, c);
        self.cursor += 1;
    }

    fn delete_before(&mut self) {
        if self.cursor == 0 {
            return;
        }
        if let Some((b, _)) = self.query.char_indices().nth(self.cursor - 1) {
            self.query.remove(b);
            self.cursor -= 1;
        }
    }

    fn delete_at(&mut self) {
        if let Some((b, _)) = self.query.char_indices().nth(self.cursor) {
            self.query.remove(b);
        }
    }

    fn open_detail(&mut self, tx: &Sender<Job>) {
        let Some(p) = self.selected_pkg() else { return };
        self.detail_target = p.name.clone();
        self.detail = None;
        self.detail_loading = true;
        self.detail_open = true;
        self.detail_scroll = 0;
        let _ = tx.send(Job::Detail { name: p.name, src: p.src });
    }

    fn detail_pkg(&self) -> Option<Pkg> {
        let d = self.detail.as_ref()?;
        if let Some(p) = self.active_list().iter().find(|p| p.name == d.name) {
            return Some(p.clone());
        }
        let get = |k: &str| -> String {
            d.rows
                .iter()
                .find(|(kk, _)| kk == k)
                .map(|(_, v)| v.clone())
                .unwrap_or_default()
        };
        Some(Pkg {
            src: d.src,
            repo: if d.src == Src::Aur { "aur".into() } else { "repo".into() },
            name: d.name.clone(),
            version: get("Version"),
            desc: get("Description"),
            maintainer: get("Maintainer"),
            votes: 0,
            popularity: 0.0,
            outdated: false,
            installed: self.installed.contains(&d.name),
        })
    }

    fn reload(&mut self, tx: &Sender<Job>) {
        match self.tab {
            0 => {
                if self.query.trim().is_empty() {
                    self.top_state = LoadState::Loading;
                    let _ = tx.send(Job::Top);
                } else {
                    self.debounce = None;
                    self.send_search(tx);
                }
            }
            1 => {
                self.top_state = LoadState::Loading;
                let _ = tx.send(Job::Top);
            }
            2 => {
                self.updated_state = LoadState::Loading;
                let _ = tx.send(Job::Updated);
            }
            _ => {
                self.installed_state = LoadState::Loading;
                let _ = tx.send(Job::Installed);
            }
        }
    }

    fn apply(&mut self, msg: Out) {
        match msg {
            Out::Search { gen, items, net_down } => {
                if gen == self.gen {
                    self.search_results = items;
                    self.search_loading = false;
                    self.net_down = net_down;
                    self.selected = 0;
                    self.offset = 0;
                    self.status = format!("{} results", self.search_results.len());
                }
            }
            Out::Top { items } => {
                self.top_results = items;
                self.top_state = LoadState::Ready;
            }
            Out::Updated { items } => {
                self.updated_results = items;
                self.updated_state = LoadState::Ready;
            }
            Out::Installed { items } => {
                self.installed = items.iter().map(|p| p.name.clone()).collect();
                self.installed_all = items;
                self.installed_state = LoadState::Ready;
                self.refilter_installed();
            }
            Out::Detail { name, detail } => {
                if name == self.detail_target {
                    self.detail = detail;
                    self.detail_loading = false;
                }
            }
        }
    }

    fn on_key(&mut self, k: KeyEvent, tx: &Sender<Job>) -> Option<(Pkg, ConfirmOp)> {
        if k.kind == KeyEventKind::Release {
            return None;
        }
        if k.modifiers.contains(KeyModifiers::CONTROL) && k.code == KeyCode::Char('c') {
            self.quit = true;
            return None;
        }
        if k.code == KeyCode::F(1) {
            self.help = !self.help;
            return None;
        }

        // confirm dialog wins over everything
        if self.confirm.is_some() {
            match k.code {
                KeyCode::Char('y' | 'Y') | KeyCode::Enter => return self.confirm.take(),
                KeyCode::Char('n' | 'N') | KeyCode::Esc | KeyCode::Char('q') => {
                    self.confirm = None;
                }
                _ => {}
            }
            return None;
        }

        if self.help {
            if matches!(k.code, KeyCode::Esc | KeyCode::Char('q')) {
                self.help = false;
            }
            return None;
        }

        // package page overlay
        if self.detail_open {
            match k.code {
                KeyCode::Esc | KeyCode::Char('q') => self.detail_open = false,
                KeyCode::Up => self.detail_scroll = self.detail_scroll.saturating_sub(1),
                KeyCode::Down => self.detail_scroll += 1,
                KeyCode::PageUp => self.detail_scroll = self.detail_scroll.saturating_sub(10),
                KeyCode::PageDown => self.detail_scroll += 10,
                KeyCode::Home => self.detail_scroll = 0,
                KeyCode::Char('i' | 'I') => {
                    if let Some(p) = self.detail_pkg() {
                        self.confirm = Some((p, ConfirmOp::Install));
                    }
                }
                KeyCode::Char('d' | 'D') => {
                    if let Some(p) = self.detail_pkg() {
                        if self.is_installed(&p) {
                            self.confirm = Some((p, ConfirmOp::Remove));
                        } else {
                            self.toast_msg("not installed — press i to install", false);
                        }
                    }
                }
                _ => {}
            }
            return None;
        }

        // search bar focused: everything types, arrows still browse
        if self.focus_search {
            match k.code {
                KeyCode::Esc => self.focus_search = false,
                KeyCode::Enter => {
                    self.flush_search(tx);
                    self.focus_search = false;
                }
                KeyCode::Up => self.move_sel(-1),
                KeyCode::Down => self.move_sel(1),
                KeyCode::Left => self.cursor = self.cursor.saturating_sub(1),
                KeyCode::Right => {
                    self.cursor = (self.cursor + 1).min(self.query.chars().count())
                }
                KeyCode::Home => self.cursor = 0,
                KeyCode::End => self.cursor = self.query.chars().count(),
                KeyCode::Backspace => {
                    self.delete_before();
                    self.on_query_change();
                }
                KeyCode::Delete => {
                    self.delete_at();
                    self.on_query_change();
                }
                KeyCode::Tab => {
                    self.focus_search = false;
                    self.cycle_tab(1, tx);
                }
                KeyCode::BackTab => {
                    self.focus_search = false;
                    self.cycle_tab(-1, tx);
                }
                KeyCode::Char(c) => {
                    self.insert_char(c);
                    self.on_query_change();
                }
                _ => {}
            }
            return None;
        }

        // normal browsing keys — deliberately boring and easy
        match k.code {
            KeyCode::Char('q') => self.quit = true,
            KeyCode::Tab => self.cycle_tab(1, tx),
            KeyCode::BackTab => self.cycle_tab(-1, tx),
            KeyCode::Char(c @ '1'..='4') => {
                let t = c as usize - '1' as usize;
                self.set_tab(t, tx);
            }
            KeyCode::Char('/') => {
                if self.tab == 1 || self.tab == 2 {
                    self.tab = 0;
                    self.clamp_sel();
                }
                self.focus_search = true;
            }
            KeyCode::Char('?') => self.help = true,
            KeyCode::Char('r') => self.reload(tx),
            KeyCode::Char('i' | 'I') => {
                if let Some(p) = self.selected_pkg() {
                    self.confirm = Some((p, ConfirmOp::Install));
                }
            }
            KeyCode::Char('d' | 'D') | KeyCode::Delete => {
                if let Some(p) = self.selected_pkg() {
                    if self.is_installed(&p) {
                        self.confirm = Some((p, ConfirmOp::Remove));
                    } else {
                        self.toast_msg("not installed — press i to install", false);
                    }
                }
            }
            KeyCode::Up => self.move_sel(-1),
            KeyCode::Down => self.move_sel(1),
            KeyCode::PageUp => self.move_sel(-10),
            KeyCode::PageDown => self.move_sel(10),
            KeyCode::Home => {
                self.selected = 0;
                self.offset = 0;
            }
            KeyCode::End => {
                let len = self.active_list().len();
                self.selected = len.saturating_sub(1);
            }
            KeyCode::Enter => self.open_detail(tx),
            _ => {}
        }
        None
    }

    fn on_mouse(&mut self, m: MouseEvent, tx: &Sender<Job>) {
        if self.help || self.confirm.is_some() {
            return;
        }
        let pos = Position { x: m.column, y: m.row };
        match m.kind {
            MouseEventKind::ScrollUp => {
                if self.detail_open {
                    self.detail_scroll = self.detail_scroll.saturating_sub(3);
                } else if self.rects.list.contains(pos) {
                    self.move_sel(-3);
                }
            }
            MouseEventKind::ScrollDown => {
                if self.detail_open {
                    self.detail_scroll += 3;
                } else if self.rects.list.contains(pos) {
                    self.move_sel(3);
                }
            }
            MouseEventKind::Down(MouseButton::Left) => {
                if self.detail_open {
                    return;
                }
                if let Some(i) = self.rects.tabs.iter().position(|t| t.contains(pos)) {
                    self.set_tab(i, tx);
                    return;
                }
                if self.rects.search.contains(pos) {
                    self.focus_search = true;
                    return;
                }
                if self.rects.list.contains(pos) {
                    let inner = self.rects.list;
                    let row =
                        (m.row.saturating_sub(inner.y) as usize) / ITEM_H + self.offset;
                    let len = self.active_list().len();
                    if row < len {
                        self.selected = row;
                        self.focus_search = false;
                    }
                    return;
                }
                self.focus_search = false;
            }
            _ => {}
        }
    }
}

// ------------------------------------------------------------------ terminal

fn enter() -> io::Result<DefaultTerminal> {
    enable_raw_mode()?;
    let mut out = io::stdout();
    execute!(out, EnterAlternateScreen, EnableMouseCapture)?;
    Terminal::new(CrosstermBackend::new(out))
}

use ratatui::Terminal;

fn leave(t: &mut DefaultTerminal) {
    let _ = disable_raw_mode();
    let _ = execute!(io::stdout(), LeaveAlternateScreen, DisableMouseCapture);
    let _ = t.show_cursor();
}

fn suspend(t: &mut DefaultTerminal) -> io::Result<()> {
    t.backend_mut().flush()?;
    disable_raw_mode()?;
    execute!(io::stdout(), LeaveAlternateScreen, DisableMouseCapture)?;
    t.show_cursor()?;
    Ok(())
}

fn resume(t: &mut DefaultTerminal) -> io::Result<()> {
    enable_raw_mode()?;
    execute!(io::stdout(), EnterAlternateScreen, EnableMouseCapture)?;
    t.hide_cursor()?;
    t.clear()?;
    Ok(())
}

fn install_hook() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen, DisableMouseCapture);
        default(info);
    }));
}

// ------------------------------------------------------------------ actions

fn run_action(
    app: &mut App,
    terminal: &mut DefaultTerminal,
    tx: &Sender<Job>,
    pkg: &Pkg,
    op: ConfirmOp,
) {
    let (cmd, kind, verb) = match op {
        ConfirmOp::Install => {
            let Some((c, k)) = data::install_cmd(&pkg.name) else {
                app.toast_msg("not in any repo, and no AUR helper (yay/paru) found", false);
                return;
            };
            (c, k, "installed")
        }
        ConfirmOp::Remove => {
            let (c, k) = data::remove_cmd(&pkg.name);
            (c, k, "removed")
        }
    };

    // Hand the terminal over to pacman/yay: they need line-buffered stdin for
    // passwords and their own confirmation prompt. Ignore SIGINT while the
    // child runs so Ctrl+C cancels the operation, not archstore itself.
    if let Err(e) = suspend(terminal) {
        app.toast_msg(&format!("could not hand over the terminal: {e}"), false);
        return;
    }
    unsafe {
        let old = libc::signal(libc::SIGINT, libc::SIG_IGN);
        println!("\n  archstore → {}  ({kind})\n", cmd.join(" "));
        let res = Command::new(&cmd[0]).args(&cmd[1..]).status();
        libc::signal(libc::SIGINT, old);
        println!("\n  press Enter to return to the store…");
        let mut buf = String::new();
        let _ = io::stdin().read_line(&mut buf);
        if let Err(e) = resume(terminal) {
            eprintln!("archstore: failed to resume the interface: {e}");
        }
        match res {
            Ok(st) if st.success() => app.toast_msg(&format!("✓ {verb} {}", pkg.name), true),
            Ok(st) => app.toast_msg(&format!("✗ {verb} exited with {st}"), false),
            Err(e) => app.toast_msg(&format!("✗ {e}"), false),
        }
    }
    let _ = tx.send(Job::Installed);
}

// ---------------------------------------------------------------- event loop

fn event_loop(
    app: &mut App,
    terminal: &mut DefaultTerminal,
    tx: &Sender<Job>,
    rx: &Receiver<Out>,
) -> i32 {
    let mut tick = Instant::now();
    loop {
        while let Ok(msg) = rx.try_recv() {
            app.apply(msg);
        }
        if let Some(dl) = app.debounce {
            if Instant::now() >= dl {
                app.debounce = None;
                app.send_search(tx);
            }
        }
        if let Some((_, _, t)) = app.toast {
            if t.elapsed() > Duration::from_secs(4) {
                app.toast = None;
            }
        }
        if tick.elapsed() >= Duration::from_millis(120) {
            app.spinner = app.spinner.wrapping_add(1);
            tick = Instant::now();
        }

        if let Err(e) = terminal.draw(|f| ui::draw(app, f)) {
            eprintln!("archstore: {e}");
            return 1;
        }
        if app.quit {
            return 0;
        }

        match event::poll(Duration::from_millis(90)) {
            Ok(true) => match event::read() {
                Ok(Event::Key(k)) => {
                    if let Some((pkg, op)) = app.on_key(k, tx) {
                        run_action(app, terminal, tx, &pkg, op);
                    }
                }
                Ok(Event::Mouse(m)) => app.on_mouse(m, tx),
                Ok(_) => {}
                Err(e) => {
                    eprintln!("archstore: {e}");
                    return 1;
                }
            },
            Ok(false) => {}
            Err(e) => {
                eprintln!("archstore: {e}");
                return 1;
            }
        }
    }
}

fn run_tui(initial: String, offline: bool) -> i32 {
    let (jtx, jrx) = mpsc::channel::<Job>();
    let (otx, orx) = mpsc::channel::<Out>();
    std::thread::spawn(move || data::worker(jrx, otx));

    let mut app = App::new(offline);
    let initial = initial.trim().to_string();
    if !initial.is_empty() {
        app.query = initial;
        app.cursor = app.query.chars().count();
    }
    app.focus_search = true; // type a word the moment it opens

    // home screen content + installed badges arrive in the background
    app.top_state = LoadState::Loading;
    let _ = jtx.send(Job::Top);
    app.installed_state = LoadState::Loading;
    let _ = jtx.send(Job::Installed);
    if !app.query.is_empty() {
        app.send_search(&jtx);
    }

    let mut terminal = match enter() {
        Ok(t) => t,
        Err(e) => {
            eprintln!("archstore: {e}");
            return 1;
        }
    };
    install_hook();
    let code = event_loop(&mut app, &mut terminal, &jtx, &orx);
    leave(&mut terminal);
    code
}

// ---------------------------------------------------------------- plain mode

fn print_plain(items: &[Pkg], q: &str) {
    if items.is_empty() {
        println!("  no packages matched “{q}”");
        return;
    }
    let color = io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none();
    let c = |name: &str, t: &str| -> String {
        if !color {
            return t.to_string();
        }
        let code = match name {
            "bold" => "1",
            "dim" => "2",
            "green" => "32",
            "yellow" => "33",
            "cyan" => "36",
            "magenta" => "35",
            _ => "0",
        };
        format!("\x1b[{code}m{t}\x1b[0m")
    };
    let tw = ratatui::crossterm::terminal::size().map(|(w, _)| w as usize).unwrap_or(100);
    let local = data::installed_names();
    let repos: Vec<&Pkg> = items.iter().filter(|p| p.src == Src::Repo).collect();
    let aurs: Vec<&Pkg> = items.iter().filter(|p| p.src == Src::Aur).collect();

    println!(
        "{}   {} result(s)\n",
        c("bold", &format!("  “{q}”")),
        items.len()
    );
    for (group, label, hdr) in [
        (repos, "OFFICIAL REPOS", "cyan"),
        (aurs, "AUR", "yellow"),
    ] {
        if group.is_empty() {
            continue;
        }
        println!("{}", c(hdr, &format!("▶ {label} ({})", group.len())));
        let bw = group
            .iter()
            .map(|p| data::dwidth(&p.repo))
            .max()
            .unwrap_or(4)
            .clamp(4, 10);
        let vw = group
            .iter()
            .map(|p| data::dwidth(&p.version))
            .max()
            .unwrap_or(6)
            .clamp(6, 22);
        for p in group {
            let mut line = format!(
                "  {} {} {}",
                c(hdr, &data::pad(&p.repo, bw)),
                c("bold", &p.name),
                c("dim", &data::pad(&data::truncate(&p.version, vw), vw)),
            );
            if p.src == Src::Aur {
                line += &format!(
                    " {}{}",
                    c("magenta", &format!("▲{}", p.votes)),
                    c("dim", &format!(" ({:.2})", p.popularity))
                );
            } else if p.installed || local.contains(&p.name) {
                line += &format!(" {}", c("green", "[installed]"));
            }
            let avail = tw.saturating_sub(data::dwidth(&line) + 3);
            line += &format!(
                "  {}",
                c("dim", &data::truncate(&p.desc, avail))
            );
            println!("{line}");
        }
        println!();
    }
}

fn plain_mode(query: &str, json: bool, offline: bool) -> i32 {
    let q = query.trim();
    if q.is_empty() {
        eprintln!("archstore: what should I search for?  e.g.  archstore video editor");
        return 2;
    }
    let (mut items, _) = data::search_official(q, offline);
    if !offline {
        items.extend(data::search_aur(q));
    }
    let items = data::rank(q, items, 40);

    if json {
        let arr: Vec<serde_json::Value> = items
            .iter()
            .map(|p| {
                serde_json::json!({
                    "source": if p.src == Src::Repo { "repo" } else { "aur" },
                    "repo": p.repo.as_str(),
                    "name": p.name.as_str(),
                    "version": p.version.as_str(),
                    "desc": p.desc.as_str(),
                    "maintainer": p.maintainer.as_str(),
                    "votes": p.votes,
                    "popularity": p.popularity,
                    "outdated": p.outdated,
                    "installed": p.installed || data::installed_names().contains(&p.name),
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&arr).unwrap_or_else(|_| "[]".into())
        );
        return 0;
    }
    print_plain(&items, q);
    0
}

// ---------------------------------------------------------------------- cli

fn print_help() {
    println!(
        "archstore — an Arch Linux package store in your terminal

  archstore                 open the store
  archstore <words>         open it, already searched for <words>
  archstore --json <words>   print results as JSON (for scripts)
  archstore --plain <words>  print results as a plain list
  archstore --offline ...    use only the local pacman database

in the store:
  type to search · ↑↓ move · Enter open · i install · d remove
  / focus search · Tab next tab · 1-4 jump · r refresh · q quit"
    );
}

fn main() {
    // die quietly on a closed pipe like every other CLI (no "Broken pipe" panic)
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut json = false;
    let mut plain = false;
    let mut offline = false;
    let mut words: Vec<String> = Vec::new();

    for a in &args {
        match a.as_str() {
            "--help" | "-h" => {
                print_help();
                return;
            }
            "--version" | "-V" => {
                println!("archstore {}", env!("CARGO_PKG_VERSION"));
                return;
            }
            "--json" | "-j" => json = true,
            "--plain" => plain = true,
            "--offline" => offline = true,
            other if other.starts_with('-') && other.len() > 1 => {
                eprintln!("archstore: unknown flag {other} (try --help)");
                std::process::exit(2);
            }
            other => words.push(other.to_string()),
        }
    }

    let query = words.join(" ");
    let code = if json || plain || !io::stdout().is_terminal() {
        plain_mode(&query, json, offline)
    } else {
        run_tui(query, offline)
    };
    std::process::exit(code);
}
