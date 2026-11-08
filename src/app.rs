//! The application: tabs of documents, files in and out, find and replace, and the commands that
//! menus and shortcuts share. Drawing of the chrome lives in `chrome.rs`.

use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant, SystemTime};

use crossbeam_channel::{Receiver, Sender, unbounded};
use egui::{Id, Key, KeyboardShortcut, Modifiers};
use margin_core::doc::{Document, OP_MAX, Sel};
use margin_core::file::{self, Encoding, Eol};
use margin_core::search::{self, Query};
use margin_core::text::{Source, Text};
use regex::bytes::Regex;

use crate::editor::{self, Reveal, View};
use crate::session::{self, BACKUP_MAX, Session, SessionTab, Settings};
use crate::syntax::{self, Lang};
use crate::theme::{self, Palette};

/// Documents up to this size are searched and saved right away; bigger ones on a thread.
pub const SYNC_MAX: u64 = 48 << 20;

pub struct Tab {
    pub id: u64,
    pub doc: Document,
    pub view: View,
    pub lang: Lang,
    /// language picked by hand (kept across saves and renames)
    pub lang_fixed: bool,
    pub disk: Option<(u64, Option<SystemTime>)>,
    pub disk_note: Option<DiskNote>,
    pub follow: bool,
    pub saving: Option<Saving>,
    pub backup: Option<String>,
    pub backed_up: u64,
    pub untitled: usize,
    pub binary: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiskNote {
    Changed,
    Deleted,
}

pub struct Saving {
    pub progress: Arc<file::Progress>,
    rx: Receiver<std::io::Result<()>>,
    version: u64,
    state: u64,
    path: PathBuf,
    encoding: Encoding,
}

impl Tab {
    pub fn title(&self) -> String {
        match &self.doc.path {
            Some(p) => p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| p.display().to_string()),
            None => format!("new {}", self.untitled),
        }
    }
    pub fn editor_id(&self) -> Id {
        Id::unique(("margin-editor", self.id))
    }
    /// An untitled tab nobody typed in: replaced by the next file opened.
    fn pristine(&self) -> bool {
        self.doc.path.is_none() && self.doc.is_empty() && !self.doc.can_undo()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cmd {
    New,
    Open,
    Save,
    SaveAs,
    SaveAll,
    Close,
    CloseAll,
    CloseOthers,
    Reopen,
    Reload,
    Undo,
    Redo,
    Cut,
    Copy,
    Paste,
    SelectAll,
    Duplicate,
    DeleteLine,
    MoveUp,
    MoveDown,
    Join,
    Upper,
    Lower,
    Comment,
    TrimTrailing,
    SortAsc,
    SortDesc,
    Dedupe,
    TabsToSpaces,
    SpacesToTabs,
    Find,
    Replace,
    FindNext,
    FindPrev,
    FindAll,
    FindInFiles,
    SelectionToFind,
    GotoLine,
    Bookmark,
    NextBookmark,
    PrevBookmark,
    ClearBookmarks,
    Wrap,
    Whitespace,
    LineNumbers,
    ZoomIn,
    ZoomOut,
    ZoomReset,
    Follow,
    NextTab,
    PrevTab,
    Results,
    Settings,
    Escape,
}

impl Cmd {
    /// Commands that act on the text, so they leave the keyboard to a focused input field.
    fn text_only(&self) -> bool {
        use Cmd::*;
        matches!(self, Undo | Redo | SelectAll | Duplicate | DeleteLine | MoveUp | MoveDown | Join | Upper | Lower | Comment | Bookmark)
    }
}

pub fn shortcuts() -> Vec<(Cmd, KeyboardShortcut)> {
    use Cmd::*;
    let c = Modifiers::COMMAND;
    let cs = Modifiers::COMMAND | Modifiers::SHIFT;
    let s = Modifiers::SHIFT;
    let n = Modifiers::NONE;
    let a = Modifiers::ALT;
    let k = KeyboardShortcut::new;
    let mut v = vec![
        (New, k(c, Key::N)),
        (Open, k(c, Key::O)),
        (Save, k(c, Key::S)),
        (SaveAs, k(Modifiers::COMMAND | Modifiers::ALT, Key::S)),
        (SaveAll, k(cs, Key::S)),
        (Close, k(c, Key::W)),
        (CloseAll, k(cs, Key::W)),
        (Reopen, k(cs, Key::T)),
        (Undo, k(c, Key::Z)),
        (Redo, k(c, Key::Y)),
        (Redo, k(cs, Key::Z)),
        (SelectAll, k(c, Key::A)),
        (Duplicate, k(c, Key::D)),
        (DeleteLine, k(c, Key::L)),
        (DeleteLine, k(cs, Key::K)),
        (MoveUp, k(cs, Key::ArrowUp)),
        (MoveDown, k(cs, Key::ArrowDown)),
        (MoveUp, k(a, Key::ArrowUp)),
        (MoveDown, k(a, Key::ArrowDown)),
        (Join, k(c, Key::J)),
        (Upper, k(cs, Key::U)),
        (Lower, k(c, Key::U)),
        (Comment, k(c, Key::Q)),
        (Comment, k(c, Key::Slash)),
        (Find, k(c, Key::F)),
        (Replace, k(c, Key::H)),
        (FindNext, k(n, Key::F3)),
        (FindPrev, k(s, Key::F3)),
        (FindInFiles, k(cs, Key::F)),
        (FindAll, k(a, Key::Enter)),
        (SelectionToFind, k(c, Key::F3)),
        (GotoLine, k(c, Key::G)),
        (Bookmark, k(c, Key::F2)),
        (NextBookmark, k(n, Key::F2)),
        (PrevBookmark, k(s, Key::F2)),
        (ZoomIn, k(c, Key::Equals)),
        (ZoomIn, k(c, Key::Plus)),
        (ZoomOut, k(c, Key::Minus)),
        (ZoomReset, k(c, Key::Num0)),
        (Wrap, k(a, Key::Z)),
        (NextTab, k(Modifiers::CTRL, Key::Tab)),
        (PrevTab, k(Modifiers::CTRL | Modifiers::SHIFT, Key::Tab)),
        (NextTab, k(c, Key::PageDown)),
        (PrevTab, k(c, Key::PageUp)),
        (Results, k(cs, Key::R)),
        (Settings, k(c, Key::Comma)),
        (Escape, k(n, Key::Escape)),
    ];
    // more modifiers first: Ctrl+S would otherwise also catch Ctrl+Shift+S
    v.sort_by_key(|(_, k)| std::cmp::Reverse(k.modifiers.ctrl as u8 + k.modifiers.command as u8 + k.modifiers.shift as u8 + k.modifiers.alt as u8));
    v
}

pub fn shortcut_label(cmd: Cmd) -> String {
    shortcuts()
        .into_iter()
        .filter(|(c, _)| *c == cmd)
        .min_by_key(|(_, k)| k.modifiers.shift as u8 + k.modifiers.alt as u8)
        .map(|(_, k)| {
            let mut s = String::new();
            if k.modifiers.command || k.modifiers.ctrl {
                s.push_str(if cfg!(target_os = "macos") { "⌘" } else { "Ctrl+" });
            }
            if k.modifiers.alt {
                s.push_str("Alt+");
            }
            if k.modifiers.shift {
                s.push_str("Shift+");
            }
            s.push_str(match k.logical_key {
                Key::Equals | Key::Plus => "+",
                Key::Minus => "-",
                Key::Slash => "/",
                Key::Comma => ",",
                Key::ArrowUp => "↑",
                Key::ArrowDown => "↓",
                Key::PageUp => "PgUp",
                Key::PageDown => "PgDn",
                other => other.name(),
            });
            s
        })
        .unwrap_or_default()
}

// ---------------------------------------------------------------- find

pub struct Find {
    pub open: bool,
    pub replace: bool,
    pub query: Query,
    pub replacement: String,
    pub in_selection: bool,
    pub focus: bool,
    pub compiled: Option<Result<Regex, String>>,
    compiled_for: Query,
    /// matches in the document, when counted: (count, capped)
    pub count: Option<(usize, bool)>,
    count_key: Option<(Query, u64, u64)>,
    /// where the caret was when typing started, for incremental search
    origin: Option<u64>,
    pub job: Option<Job>,
}

impl Default for Find {
    fn default() -> Self {
        Find {
            open: false,
            replace: false,
            query: Query::default(),
            replacement: String::new(),
            in_selection: false,
            focus: false,
            compiled: None,
            compiled_for: Query { pattern: "\u{0}".into(), ..Default::default() },
            count: None,
            count_key: None,
            origin: None,
            job: None,
        }
    }
}

pub struct Job {
    pub progress: Arc<search::Progress>,
    pub label: &'static str,
    tab: u64,
    rx: Receiver<JobDone>,
    started: Instant,
}

enum JobDone {
    Found(Option<Range<u64>>),
    Count(usize, bool),
    Replaced(Option<search::Replaced>, u32),
}

pub struct Hit {
    pub path: Option<PathBuf>,
    pub tab: Option<u64>,
    pub at: u64,
    pub len: u64,
    pub line: Option<u64>,
    pub preview: String,
    /// the match within the preview (byte range)
    pub mark: Range<usize>,
}

pub struct Results {
    pub title: String,
    pub hits: Vec<Hit>,
    pub files: usize,
    pub progress: Option<Arc<search::Progress>>,
    rx: Option<Receiver<Vec<Hit>>>,
    pub selected: Option<usize>,
    pub done: bool,
    pub capped: bool,
}

pub struct FifDialog {
    pub dir: String,
    pub filter: String,
    pub hidden: bool,
    pub focus: bool,
}

pub struct Toast {
    pub text: String,
    pub tone: Tone,
    pub at: Instant,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Info,
    Ok,
    Bad,
}

pub enum Ask {
    /// unsaved changes in these tabs; then close them (or quit)
    Unsaved { tabs: Vec<u64>, quit: bool },
    /// writing failed for lack of permission: offer to save as administrator
    Elevate { tab: u64, path: PathBuf },
}

pub struct App {
    pub tabs: Vec<Tab>,
    pub active: usize,
    next_id: u64,
    pub settings: Settings,
    saved_settings: Settings,
    pub find: Find,
    pub results: Option<Results>,
    pub results_open: bool,
    pub fif: Option<FifDialog>,
    pub goto: Option<String>,
    pub settings_open: bool,
    pub ask: Option<Ask>,
    pub toasts: Vec<Toast>,
    closed: Vec<PathBuf>,
    pub palette: Palette,
    pub syn_theme: syntect::highlighting::Theme,
    syn_dark: Option<bool>,
    incoming: Receiver<Vec<PathBuf>>,
    last_disk_check: Instant,
    last_backup: Instant,
    last_session: Instant,
    pub quitting: bool,
    title: String,
    pub focus_editor: bool,
    pub languages: Vec<String>,
}

fn file_meta(p: &Path) -> Option<(u64, Option<SystemTime>)> {
    std::fs::metadata(p).ok().map(|m| (m.len(), m.modified().ok()))
}

fn encoding_from_name(n: &str) -> Option<Encoding> {
    Some(match n {
        "UTF-8" => Encoding::Utf8,
        "UTF-8 with BOM" => Encoding::Utf8Bom,
        "UTF-16 LE" => Encoding::Utf16Le,
        "UTF-16 BE" => Encoding::Utf16Be,
        other => Encoding::Legacy(encoding_rs::Encoding::for_label(other.as_bytes())?),
    })
}

/// "path:line" or "path:line:col" for a path that doesn't exist as written.
fn split_position(p: &Path) -> (PathBuf, Option<u64>) {
    if p.exists() {
        return (p.to_path_buf(), None);
    }
    let s = p.to_string_lossy();
    let mut parts = s.rsplitn(3, ':').collect::<Vec<_>>();
    parts.reverse();
    match parts.as_slice() {
        [path, line, col] if line.parse::<u64>().is_ok() && col.parse::<u64>().is_ok() => (PathBuf::from(path), line.parse().ok()),
        [a, b, line] if line.parse::<u64>().is_ok() => (PathBuf::from(format!("{a}:{b}")), line.parse().ok()),
        [path, line] if line.parse::<u64>().is_ok() => (PathBuf::from(path), line.parse().ok()),
        _ => (p.to_path_buf(), None),
    }
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, files: Vec<PathBuf>, incoming: Receiver<Vec<PathBuf>>) -> App {
        let ctx = &cc.egui_ctx;
        ctx.set_fonts(theme::fonts());
        theme::apply(ctx, &theme::DARK, egui::Theme::Dark);
        theme::apply(ctx, &theme::LIGHT, egui::Theme::Light);
        ctx.options_mut(|o| o.zoom_with_keyboard = false);
        let settings = session::load_settings();
        let mut app = App {
            tabs: Vec::new(),
            active: 0,
            next_id: 1,
            saved_settings: settings.clone(),
            settings,
            find: Find::default(),
            results: None,
            results_open: false,
            fif: None,
            goto: None,
            settings_open: false,
            ask: None,
            toasts: Vec::new(),
            closed: Vec::new(),
            palette: theme::DARK,
            syn_theme: syntax::theme_for(&theme::DARK),
            syn_dark: None,
            incoming,
            last_disk_check: Instant::now(),
            last_backup: Instant::now(),
            last_session: Instant::now(),
            quitting: false,
            title: String::new(),
            focus_editor: true,
            languages: syntax::all_languages(),
        };
        app.apply_theme_pref(ctx);
        if app.settings.restore_session {
            app.restore();
        }
        for f in files {
            let (path, line) = split_position(&f);
            app.open_path(&path, line);
        }
        if app.tabs.is_empty() {
            app.new_tab();
        }
        app
    }

    pub fn apply_theme_pref(&self, ctx: &egui::Context) {
        ctx.set_theme(match self.settings.theme.as_str() {
            "dark" => egui::ThemePreference::Dark,
            "light" => egui::ThemePreference::Light,
            _ => egui::ThemePreference::System,
        });
    }

    pub fn tab(&self) -> &Tab {
        &self.tabs[self.active]
    }

    pub fn tab_mut(&mut self) -> &mut Tab {
        &mut self.tabs[self.active]
    }

    fn tab_index(&self, id: u64) -> Option<usize> {
        self.tabs.iter().position(|t| t.id == id)
    }

    pub fn toast(&mut self, text: impl Into<String>, tone: Tone) {
        self.toasts.push(Toast { text: text.into(), tone, at: Instant::now() });
        if self.toasts.len() > 4 {
            self.toasts.remove(0);
        }
    }

    fn make_tab(&mut self, doc: Document, lang: Lang) -> Tab {
        let id = self.next_id;
        self.next_id += 1;
        Tab {
            id,
            doc,
            view: View::default(),
            lang,
            lang_fixed: false,
            disk: None,
            disk_note: None,
            follow: false,
            saving: None,
            backup: None,
            backed_up: 0,
            untitled: 0,
            binary: false,
        }
    }

    pub fn new_tab(&mut self) {
        let n = (1..).find(|n| !self.tabs.iter().any(|t| t.doc.path.is_none() && t.untitled == *n)).unwrap_or(1);
        let mut t = self.make_tab(Document::empty(), Lang::Plain);
        t.untitled = n;
        self.tabs.insert((self.active + 1).min(self.tabs.len()), t);
        self.active = (self.active + 1).min(self.tabs.len() - 1);
        if self.tabs.len() == 1 {
            self.active = 0;
        }
        self.focus_editor = true;
    }

    /// Opens a file (or shows it if it's open already), optionally at a 1-based line.
    pub fn open_path(&mut self, path: &Path, line: Option<u64>) {
        let path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        if let Some(i) = self.tabs.iter().position(|t| t.doc.path.as_deref() == Some(&path)) {
            self.active = i;
        } else {
            match file::open(&path, None) {
                Ok(o) => {
                    let first = String::from_utf8_lossy(&o.text.read(0..o.text.len().min(256))).lines().next().unwrap_or("").to_string();
                    let lang = if o.binary { Lang::Plain } else { syntax::detect(Some(&path), &first, o.size) };
                    let mut doc = Document::new(o.text, Some(path.clone()), o.encoding, o.eol);
                    doc.binary = o.binary;
                    let mut t = self.make_tab(doc, lang);
                    t.disk = Some((o.size, o.modified));
                    t.binary = o.binary;
                    if o.binary {
                        self.toast("This looks like a binary file; showing it as text", Tone::Info);
                    }
                    // a fresh untitled tab gives way to the file
                    if self.tabs.get(self.active).is_some_and(|t| t.pristine()) {
                        self.tabs[self.active] = t;
                    } else {
                        let at = (self.active + 1).min(self.tabs.len());
                        self.tabs.insert(at, t);
                        self.active = at;
                    }
                    self.remember_recent(&path);
                }
                Err(e) => {
                    self.toast(format!("Couldn't open {}: {}", path.display(), e), Tone::Bad);
                    return;
                }
            }
        }
        if let Some(n) = line {
            self.goto_line(n.saturating_sub(1));
        }
        self.focus_editor = true;
    }

    fn remember_recent(&mut self, p: &Path) {
        self.settings.recent.retain(|r| r != p);
        self.settings.recent.insert(0, p.to_path_buf());
        self.settings.recent.truncate(15);
    }

    pub fn open_dialog(&mut self) {
        let mut d = rfd::FileDialog::new().set_title("Open");
        if let Some(dir) = self.tab().doc.path.as_ref().and_then(|p| p.parent()) {
            d = d.set_directory(dir);
        }
        if let Some(files) = d.pick_files() {
            for f in files {
                self.open_path(&f, None);
            }
        }
    }

    /// Jumps to a 0-based line, if the lines that far are counted.
    pub fn goto_line(&mut self, line: u64) -> bool {
        let t = self.tab_mut();
        match t.doc.text.line_start(line) {
            Some(at) => {
                t.doc.sel = Sel::caret(at);
                t.view.reveal = Reveal::Center;
                true
            }
            None => false,
        }
    }

    pub fn goto_offset(&mut self, at: u64, len: u64) {
        let t = self.tab_mut();
        let at = at.min(t.doc.len());
        t.doc.sel = Sel { anchor: at, head: (at + len).min(t.doc.len()) };
        t.view.reveal = Reveal::Center;
        self.focus_editor = true;
    }

    // ------------------------------------------------------------ saving

    /// Saves a tab (asking where for a new file). Returns true when it's saved or saving.
    pub fn save(&mut self, i: usize, choose: bool) -> bool {
        let t = &self.tabs[i];
        if t.saving.is_some() {
            return true;
        }
        let path = match (&t.doc.path, choose) {
            (Some(p), false) => p.clone(),
            _ => {
                let mut d = rfd::FileDialog::new().set_title("Save as").set_file_name(t.title());
                if let Some(dir) = t.doc.path.as_ref().and_then(|p| p.parent()) {
                    d = d.set_directory(dir);
                }
                match d.save_file() {
                    Some(p) => p,
                    None => return false,
                }
            }
        };
        let t = &mut self.tabs[i];
        if t.doc.encoding.converts() && t.doc.len() > file::CONVERT_MAX {
            self.toast("Too big to save in this encoding; switch it to UTF-8 first", Tone::Bad);
            return false;
        }
        let text = t.doc.text.clone();
        let enc = t.doc.encoding;
        let version = t.doc.version;
        let state = t.doc.state();
        if text.len() <= SYNC_MAX {
            match file::save(&text, &path, enc, &file::Progress::default()) {
                Ok(()) => {
                    self.saved(i, path, state);
                    true
                }
                Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied && cfg!(unix) => {
                    let id = self.tabs[i].id;
                    self.ask = Some(Ask::Elevate { tab: id, path });
                    false
                }
                Err(e) => {
                    self.toast(format!("Couldn't save: {e}"), Tone::Bad);
                    false
                }
            }
        } else {
            let progress = Arc::new(file::Progress::default());
            let (tx, rx) = unbounded();
            let (p2, path2) = (progress.clone(), path.clone());
            std::thread::spawn(move || {
                let _ = tx.send(file::save(&text, &path2, enc, &p2));
            });
            t.saving = Some(Saving { progress, rx, version, state, path, encoding: enc });
            true
        }
    }

    fn saved(&mut self, i: usize, path: PathBuf, state: u64) {
        let t = &mut self.tabs[i];
        let renamed = t.doc.path.as_deref() != Some(&path);
        t.doc.path = Some(path.clone());
        t.doc.mark_saved_at(state);
        t.disk = file_meta(&path);
        t.disk_note = None;
        if renamed && !t.lang_fixed {
            let first = String::from_utf8_lossy(&t.doc.text.read(0..t.doc.len().min(256))).lines().next().unwrap_or("").to_string();
            t.lang = syntax::detect(Some(&path), &first, t.doc.len());
        }
        if let Some(b) = t.backup.take() {
            let _ = std::fs::remove_file(session::backup_path(&b));
        }
        self.remember_recent(&path);
    }

    /// Saves through pkexec when the file belongs to someone else.
    pub fn save_elevated(&mut self, id: u64, path: PathBuf) {
        let Some(i) = self.tab_index(id) else { return };
        let t = &self.tabs[i];
        let tmp = std::env::temp_dir().join(format!("margin-{}-{}", std::process::id(), t.id));
        let state = t.doc.state();
        let res = file::save(&t.doc.text, &tmp, t.doc.encoding, &file::Progress::default()).and_then(|_| {
            let st = std::process::Command::new("pkexec").arg("cp").arg("--").arg(&tmp).arg(&path).status()?;
            if st.success() { Ok(()) } else { Err(std::io::Error::other("not authorised")) }
        });
        let _ = std::fs::remove_file(&tmp);
        match res {
            Ok(()) => {
                self.saved(i, path, state);
                self.toast("Saved as administrator", Tone::Ok);
            }
            Err(e) => self.toast(format!("Couldn't save: {e}"), Tone::Bad),
        }
    }

    fn poll_saves(&mut self) {
        for i in 0..self.tabs.len() {
            let done = match &self.tabs[i].saving {
                Some(s) => s.rx.try_recv().ok().map(|r| (r, s.path.clone(), s.version, s.state, s.encoding)),
                None => None,
            };
            if let Some((res, path, version, state, enc)) = done {
                self.tabs[i].saving = None;
                match res {
                    Ok(()) => {
                        let untouched = self.tabs[i].doc.version == version;
                        self.saved(i, path.clone(), state);
                        // drop the old file's mapping so its disk space comes back
                        if untouched && let Ok(o) = file::open(&path, Some(enc)) {
                            let t = &mut self.tabs[i];
                            let (sel, bookmarks, eol) = (t.doc.sel, t.doc.bookmarks.clone(), t.doc.eol);
                            let mut doc = Document::new(o.text, Some(path.clone()), o.encoding, eol);
                            doc.sel = sel;
                            doc.bookmarks = bookmarks;
                            t.doc = doc;
                        }
                        self.toast(format!("Saved {}", path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()), Tone::Ok);
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => self.toast("Save cancelled", Tone::Info),
                    Err(e) => self.toast(format!("Couldn't save: {e}"), Tone::Bad),
                }
            }
        }
    }

    // ------------------------------------------------------------ closing

    /// Closes tabs, asking about unsaved ones first.
    pub fn close_tabs(&mut self, ids: Vec<u64>) {
        let dirty: Vec<u64> = ids.iter().copied().filter(|id| self.tab_index(*id).is_some_and(|i| self.tabs[i].doc.dirty())).collect();
        if !dirty.is_empty() {
            self.ask = Some(Ask::Unsaved { tabs: ids, quit: false });
            return;
        }
        self.drop_tabs(&ids);
    }

    pub fn drop_tabs(&mut self, ids: &[u64]) {
        for id in ids {
            if let Some(i) = self.tab_index(*id) {
                let t = self.tabs.remove(i);
                if let Some(p) = t.doc.path {
                    self.closed.push(p);
                }
                if let Some(b) = t.backup {
                    let _ = std::fs::remove_file(session::backup_path(&b));
                }
                if self.active > i || self.active >= self.tabs.len() {
                    self.active = self.active.saturating_sub(1);
                }
            }
        }
        if self.tabs.is_empty() {
            self.new_tab();
        }
        self.focus_editor = true;
    }

    // ------------------------------------------------------------ session & backups

    fn restore(&mut self) {
        let s = session::load_session();
        for st in s.tabs {
            let backup = st.backup.as_ref().map(|b| session::backup_path(b)).filter(|p| p.exists());
            let tab = if let Some(bp) = backup {
                // unsaved work: the backup's text, still belonging to its file
                let Ok(bytes) = std::fs::read(&bp) else { continue };
                let enc = st.encoding.as_deref().and_then(encoding_from_name).unwrap_or(Encoding::Utf8);
                let eol = match st.eol.as_deref() {
                    Some("CRLF") => Eol::CrLf,
                    Some("CR") => Eol::Cr,
                    Some("LF") => Eol::Lf,
                    _ => file::detect_eol(&bytes[..bytes.len().min(65536)]),
                };
                // a backup that matches the file isn't unsaved work
                let same = st.path.as_ref().is_some_and(|p| std::fs::metadata(p).is_ok_and(|m| m.len() == bytes.len() as u64) && std::fs::read(p).is_ok_and(|b| b == bytes));
                let mut doc = Document::new(Text::from_bytes(bytes), st.path.clone(), enc, eol);
                if !same {
                    doc.mark_unsaved();
                }
                let mut t = self.make_tab(doc, Lang::Plain);
                t.backup = st.backup.clone();
                t.disk = st.path.as_deref().and_then(file_meta);
                t
            } else if let Some(p) = &st.path {
                let Ok(o) = file::open(p, st.encoding.as_deref().and_then(encoding_from_name)) else { continue };
                let mut doc = Document::new(o.text, Some(p.clone()), o.encoding, o.eol);
                doc.binary = o.binary;
                let mut t = self.make_tab(doc, Lang::Plain);
                t.disk = Some((o.size, o.modified));
                t.binary = o.binary;
                t
            } else {
                continue;
            };
            let mut t = tab;
            t.untitled = st.untitled;
            t.follow = st.follow;
            let len = t.doc.len();
            t.doc.sel = Sel { anchor: st.anchor.min(len), head: st.head.min(len) };
            t.doc.bookmarks = st.bookmarks.into_iter().filter(|b| *b <= len).collect();
            t.view.top = margin_core::rows::row_start(&t.doc.text, st.top.min(len));
            let first = String::from_utf8_lossy(&t.doc.text.read(0..len.min(256))).lines().next().unwrap_or("").to_string();
            t.lang = match st.lang {
                Some(l) if l == "Plain text" => Lang::Plain,
                Some(l) if l == "Log" => Lang::Log,
                Some(l) if syntax::syntaxes().find_syntax_by_name(&l).is_some() => Lang::Syntax(l),
                _ => syntax::detect(t.doc.path.as_deref(), &first, len),
            };
            if t.follow {
                t.view.reveal = Reveal::End;
            }
            self.tabs.push(t);
        }
        self.active = s.active.min(self.tabs.len().saturating_sub(1));
    }

    /// Writes backups of unsaved tabs. With `all`, every one that fits, right now.
    fn backup(&mut self, all: bool) {
        for t in &mut self.tabs {
            if !t.doc.dirty() || t.doc.len() > BACKUP_MAX || (t.backed_up == t.doc.version && !all) {
                continue;
            }
            let id = t.backup.get_or_insert_with(session::new_backup_id).clone();
            let p = session::backup_path(&id);
            let _ = std::fs::create_dir_all(session::backup_dir());
            let tmp = p.with_extension("tmp");
            let mut f = match std::fs::File::create(&tmp) {
                Ok(f) => std::io::BufWriter::new(f),
                Err(_) => continue,
            };
            if file::write(&t.doc.text, &mut f, Encoding::Utf8, &file::Progress::default()).is_ok() {
                drop(f);
                let _ = std::fs::rename(&tmp, &p);
                t.backed_up = t.doc.version;
            }
        }
    }

    pub fn save_session(&mut self) {
        let tabs = self
            .tabs
            .iter()
            .filter(|t| t.doc.path.is_some() || t.doc.dirty())
            .map(|t| SessionTab {
                path: t.doc.path.clone(),
                backup: if t.doc.dirty() { t.backup.clone() } else { None },
                anchor: t.doc.sel.anchor,
                head: t.doc.sel.head,
                top: t.view.top,
                encoding: Some(t.doc.encoding.name()),
                eol: Some(t.doc.eol.name().into()),
                lang: t.lang_fixed.then(|| t.lang.name().to_string()),
                follow: t.follow,
                untitled: t.untitled,
                bookmarks: t.doc.bookmarks.clone(),
            })
            .collect::<Vec<_>>();
        let active = self.tabs.iter().filter(|t| t.doc.path.is_some() || t.doc.dirty()).position(|t| t.id == self.tab().id).unwrap_or(0);
        session::save_session(&Session { tabs, active });
        if self.settings != self.saved_settings {
            session::save_settings(&self.settings);
            self.saved_settings = self.settings.clone();
        }
    }

    /// The window wants to close: back up and go, or ask about what can't be backed up.
    pub fn on_close_request(&mut self, ctx: &egui::Context) {
        if self.quitting {
            return;
        }
        let keep = self.settings.restore_session;
        if keep {
            self.backup(true);
        }
        let unsafe_tabs: Vec<u64> = self
            .tabs
            .iter()
            .filter(|t| t.doc.dirty() && (!keep || t.doc.len() > BACKUP_MAX || t.backup.is_none()))
            .map(|t| t.id)
            .collect();
        if unsafe_tabs.is_empty() && self.tabs.iter().all(|t| t.saving.is_none()) {
            self.save_session();
            self.quitting = true;
            return;
        }
        ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
        if self.tabs.iter().any(|t| t.saving.is_some()) {
            self.toast("Still saving: Margin closes when that's done", Tone::Info);
            return;
        }
        self.ask = Some(Ask::Unsaved { tabs: unsafe_tabs, quit: true });
    }

    // ------------------------------------------------------------ files changing on disk

    fn check_disk(&mut self) {
        for i in 0..self.tabs.len() {
            let t = &self.tabs[i];
            let (Some(path), Some(known)) = (t.doc.path.clone(), t.disk) else { continue };
            if t.saving.is_some() {
                continue;
            }
            let now = file_meta(&path);
            match now {
                None => {
                    self.tabs[i].disk_note = Some(DiskNote::Deleted);
                }
                Some(m) if m != known => {
                    let t = &mut self.tabs[i];
                    t.disk = Some(m);
                    if !t.doc.dirty() {
                        self.reload(i, true);
                    } else {
                        t.disk_note = Some(DiskNote::Changed);
                    }
                }
                Some(_) => {
                    if self.tabs[i].disk_note == Some(DiskNote::Deleted) {
                        self.tabs[i].disk_note = None;
                    }
                }
            }
        }
    }

    /// Reads the file again, keeping the caret and view where they were.
    pub fn reload(&mut self, i: usize, quiet: bool) {
        let t = &mut self.tabs[i];
        let Some(path) = t.doc.path.clone() else { return };
        match file::open(&path, Some(t.doc.encoding)) {
            Ok(o) => {
                let (sel, top, bookmarks) = (t.doc.sel, t.view.top, t.doc.bookmarks.clone());
                let mut doc = Document::new(o.text, Some(path), o.encoding, o.eol);
                let len = doc.len();
                doc.sel = Sel { anchor: sel.anchor.min(len), head: sel.head.min(len) };
                doc.bookmarks = bookmarks.into_iter().filter(|b| *b <= len).collect();
                t.doc = doc;
                t.view.top = margin_core::rows::row_start(&t.doc.text, top.min(len));
                t.view.hl = Default::default();
                t.disk = Some((o.size, o.modified));
                t.disk_note = None;
                if t.follow {
                    t.view.reveal = Reveal::End;
                    t.doc.sel = Sel::caret(len);
                }
                if !quiet {
                    self.toast("Reloaded from disk", Tone::Info);
                }
            }
            Err(e) => self.toast(format!("Couldn't reload: {e}"), Tone::Bad),
        }
    }

    // ------------------------------------------------------------ find & replace

    pub fn compiled(&mut self) -> Option<Regex> {
        if self.find.compiled_for != self.find.query {
            self.find.compiled_for = self.find.query.clone();
            self.find.compiled = if self.find.query.pattern.is_empty() { None } else { Some(self.find.query.compile()) };
        }
        self.find.compiled.as_ref().and_then(|r| r.as_ref().ok()).cloned()
    }

    pub fn open_find(&mut self, replace: bool) {
        self.find.open = true;
        self.find.replace = replace;
        self.find.focus = true;
        self.focus_editor = false;
        let t = self.tab();
        if !t.doc.sel.is_empty() {
            let r = t.doc.sel.range();
            if r.end - r.start < 500 {
                let b = t.doc.text.read(r);
                if !b.contains(&b'\n') {
                    let s = String::from_utf8_lossy(&b).into_owned();
                    self.find.query.pattern = if self.find.query.regex { regex::escape(&s) } else { s };
                }
            }
        }
        self.find.origin = Some(self.tab().doc.sel.range().start);
    }

    fn remember_query(&mut self) {
        let p = self.find.query.pattern.clone();
        if p.is_empty() {
            return;
        }
        let h = &mut self.settings.find_history;
        h.retain(|x| *x != p);
        h.insert(0, p);
        h.truncate(20);
    }

    /// Incremental search as the pattern changes: from where the search began.
    pub fn find_as_you_type(&mut self) {
        let Some(re) = self.compiled() else { return };
        let origin = *self.find.origin.get_or_insert(self.tabs[self.active].doc.sel.range().start);
        let t = &self.tabs[self.active];
        if t.doc.len() <= SYNC_MAX {
            let found = search::find_next(&t.doc.text, &re, origin, self.settings.wrap_search, &search::Progress::default());
            self.show_found(found, false);
        } else {
            self.start_find(re, origin, true);
        }
    }

    pub fn find_next(&mut self, forward: bool) {
        let Some(re) = self.compiled() else {
            self.open_find(false);
            return;
        };
        self.remember_query();
        let t = &self.tabs[self.active];
        let r = t.doc.sel.range();
        let from = if forward { if r.is_empty() { r.start } else { r.end } } else { r.start };
        self.find.origin = Some(from);
        if t.doc.len() <= SYNC_MAX {
            let p = search::Progress::default();
            let found = if forward { search::find_next(&t.doc.text, &re, from, self.settings.wrap_search, &p) } else { search::find_prev(&t.doc.text, &re, from, self.settings.wrap_search, &p) };
            self.show_found(found, true);
        } else {
            self.start_find(re, from, forward);
        }
    }

    fn start_find(&mut self, re: Regex, from: u64, forward: bool) {
        if let Some(j) = &self.find.job {
            j.progress.cancel.store(true, Ordering::Relaxed);
        }
        let t = &self.tabs[self.active];
        let text = t.doc.text.clone();
        let progress = Arc::new(search::Progress::default());
        let (tx, rx) = unbounded();
        let p = progress.clone();
        let wrap = self.settings.wrap_search;
        std::thread::spawn(move || {
            let found = if forward { search::find_next(&text, &re, from, wrap, &p) } else { search::find_prev(&text, &re, from, wrap, &p) };
            if !p.cancel.load(Ordering::Relaxed) {
                let _ = tx.send(JobDone::Found(found));
            }
        });
        self.find.job = Some(Job { progress, label: "Searching", tab: t.id, rx, started: Instant::now() });
    }

    fn show_found(&mut self, found: Option<Range<u64>>, say_none: bool) {
        match found {
            Some(r) => {
                let t = self.tab_mut();
                t.doc.sel = Sel { anchor: r.start, head: r.end };
                t.view.reveal = Reveal::Center;
            }
            None if say_none => self.toast(format!("“{}” not found", self.find.query.pattern), Tone::Info),
            None => {}
        }
    }

    /// Keeps the match count fresh for documents small enough to count on the spot.
    fn update_count(&mut self) {
        if !self.find.open {
            return;
        }
        let t = &self.tabs[self.active];
        let key = (self.find.query.clone(), t.doc.version, t.id);
        if self.find.count_key.as_ref() == Some(&key) {
            return;
        }
        self.find.count_key = Some(key);
        self.find.count = None;
        let Some(re) = self.compiled() else { return };
        let t = &self.tabs[self.active];
        if t.doc.len() <= SYNC_MAX {
            let n = search::find_all(&t.doc.text, &re, 100_000, &search::Progress::default(), |_| {});
            self.find.count = Some((n, n >= 100_000));
        }
    }

    /// Counts matches on a thread (for big documents).
    pub fn count_now(&mut self) {
        let Some(re) = self.compiled() else { return };
        let t = &self.tabs[self.active];
        let text = t.doc.text.clone();
        let progress = Arc::new(search::Progress::default());
        let (tx, rx) = unbounded();
        let p = progress.clone();
        std::thread::spawn(move || {
            let n = search::find_all(&text, &re, 10_000_000, &p, |_| {});
            let _ = tx.send(JobDone::Count(n, n >= 10_000_000));
        });
        self.find.job = Some(Job { progress, label: "Counting", tab: t.id, rx, started: Instant::now() });
    }

    /// Replaces the selected match, then finds the next one.
    pub fn replace_one(&mut self) {
        let Some(re) = self.compiled() else { return };
        let regex = self.find.query.regex;
        let rep = self.find.replacement.clone();
        let t = &mut self.tabs[self.active];
        let r = t.doc.sel.range();
        if !r.is_empty() && r.end - r.start < 1 << 20 {
            let hay = t.doc.text.read(r.clone());
            if re.find(&hay).is_some_and(|m| m.start() == 0 && m.end() == hay.len()) {
                let out = search::expand(&re, regex, &hay, &rep);
                let n = out.len() as u64;
                t.doc.edit(vec![(r.clone(), out)], margin_core::doc::Kind::Other, |_| Sel::caret(r.start + n));
            }
        }
        self.find_next(true);
    }

    pub fn replace_all(&mut self) {
        let Some(re) = self.compiled() else { return };
        self.remember_query();
        let regex = self.find.query.regex;
        let rep = self.find.replacement.clone();
        let in_sel = self.find.in_selection;
        let t = &self.tabs[self.active];
        let range = if in_sel && !t.doc.sel.is_empty() { t.doc.sel.range() } else { 0..t.doc.len() };
        let text = t.doc.text.clone();
        let src = text.sources().len() as u32;
        if t.doc.len() <= SYNC_MAX {
            let r = search::replace_all(&text, &re, regex, &rep, range, src, &search::Progress::default());
            self.apply_replaced(r, src);
        } else {
            let progress = Arc::new(search::Progress::default());
            let (tx, rx) = unbounded();
            let p = progress.clone();
            std::thread::spawn(move || {
                let r = search::replace_all(&text, &re, regex, &rep, range, src, &p);
                let _ = tx.send(JobDone::Replaced(r, src));
            });
            self.find.job = Some(Job { progress, label: "Replacing", tab: t.id, rx, started: Instant::now() });
        }
    }

    fn apply_replaced(&mut self, r: Option<search::Replaced>, src: u32) {
        let Some(r) = r else { return };
        if r.count == 0 {
            self.toast(format!("“{}” not found", self.find.query.pattern), Tone::Info);
            return;
        }
        let t = self.tab_mut();
        if t.doc.text.sources().len() as u32 != src {
            // the text changed while replacing: the result no longer fits
            self.toast("The text changed while replacing; try again", Tone::Bad);
            return;
        }
        let caret = t.doc.sel.head.min(r.pieces.iter().map(|p| p.len).sum());
        t.doc.swap_pieces(r.pieces, Some(Source::owned(r.added)), Sel::caret(caret));
        let n = r.count;
        self.toast(format!("Replaced {n} match{}", if n == 1 { "" } else { "es" }), Tone::Ok);
    }

    fn poll_jobs(&mut self) {
        let Some(job) = &self.find.job else { return };
        let Ok(done) = job.rx.try_recv() else { return };
        let tab = job.tab;
        self.find.job = None;
        if self.tab().id != tab {
            return;
        }
        match done {
            JobDone::Found(r) => self.show_found(r, true),
            JobDone::Count(n, capped) => self.find.count = Some((n, capped)),
            JobDone::Replaced(r, src) => self.apply_replaced(r, src),
        }
    }

    pub fn job_age(&self) -> Option<Duration> {
        self.find.job.as_ref().map(|j| j.started.elapsed())
    }

    pub fn cancel_job(&mut self) {
        if let Some(j) = self.find.job.take() {
            j.progress.cancel.store(true, Ordering::Relaxed);
        }
    }

    /// All matches in the current document, into the results panel.
    pub fn find_all(&mut self) {
        let Some(re) = self.compiled() else { return };
        self.remember_query();
        let t = &self.tabs[self.active];
        let text = t.doc.text.clone();
        let tab = t.id;
        let title = format!("“{}” in {}", self.find.query.pattern, t.title());
        let progress = Arc::new(search::Progress::default());
        let (tx, rx) = unbounded::<Vec<Hit>>();
        let p = progress.clone();
        std::thread::spawn(move || {
            search::find_all(&text, &re, 200_000, &p, |batch| {
                let hits = batch.iter().map(|m| hit_in(&text, m.clone(), Some(tab), None)).collect();
                let _ = tx.send(hits);
            });
        });
        self.results = Some(Results { title, hits: Vec::new(), files: 1, progress: Some(progress), rx: Some(rx), selected: None, done: false, capped: false });
        self.results_open = true;
    }

    /// Searches a folder.
    pub fn find_in_files(&mut self, dir: PathBuf, filter: String, hidden: bool) {
        let Some(re) = self.compiled() else {
            self.toast("Type something to find first", Tone::Info);
            return;
        };
        self.remember_query();
        self.settings.fif_dir = dir.display().to_string();
        self.settings.fif_filter = filter.clone();
        self.settings.fif_hidden = hidden;
        let title = format!("“{}” in {}", self.find.query.pattern, dir.display());
        let progress = Arc::new(search::Progress::default());
        let (tx, rx) = unbounded::<Vec<Hit>>();
        let p = progress.clone();
        std::thread::spawn(move || search_folder(dir, filter, hidden, re, tx, p));
        self.results = Some(Results { title, hits: Vec::new(), files: 0, progress: Some(progress), rx: Some(rx), selected: None, done: false, capped: false });
        self.results_open = true;
    }

    fn poll_results(&mut self) {
        let Some(r) = &mut self.results else { return };
        let Some(rx) = &r.rx else { return };
        let mut got = false;
        loop {
            match rx.try_recv() {
                Ok(batch) => {
                    got = true;
                    if r.hits.len() < 200_000 {
                        let mut files: Vec<&Option<PathBuf>> = Vec::new();
                        for h in &batch {
                            if h.path.is_some() && (files.last() != Some(&&h.path)) && r.hits.last().is_none_or(|l| l.path != h.path) {
                                files.push(&h.path);
                            }
                        }
                        r.files += files.len();
                        r.hits.extend(batch);
                    } else {
                        r.capped = true;
                    }
                }
                Err(crossbeam_channel::TryRecvError::Empty) => break,
                Err(crossbeam_channel::TryRecvError::Disconnected) => {
                    r.rx = None;
                    r.done = true;
                    break;
                }
            }
        }
        let _ = got;
    }

    pub fn open_hit(&mut self, i: usize) {
        let Some(r) = &mut self.results else { return };
        let Some(h) = r.hits.get(i) else { return };
        r.selected = Some(i);
        let (tab, path, at, len) = (h.tab, h.path.clone(), h.at, h.len);
        if let Some(id) = tab {
            if let Some(ti) = self.tab_index(id) {
                self.active = ti;
                self.goto_offset(at, len);
            }
        } else if let Some(p) = path {
            self.open_path(&p, None);
            if self.tab().doc.path.as_deref().map(|x| std::fs::canonicalize(x).unwrap_or(x.to_path_buf())) == std::fs::canonicalize(&p).ok() {
                self.goto_offset(at, len);
            }
        }
    }

    // ------------------------------------------------------------ commands

    pub fn run(&mut self, cmd: Cmd, ctx: &egui::Context) {
        use Cmd::*;
        let tab_size = self.settings.editor.tab_width;
        match cmd {
            New => self.new_tab(),
            Open => self.open_dialog(),
            Save => {
                let i = self.active;
                self.save(i, false);
            }
            SaveAs => {
                let i = self.active;
                self.save(i, true);
            }
            SaveAll => {
                for i in 0..self.tabs.len() {
                    if self.tabs[i].doc.dirty() {
                        self.save(i, false);
                    }
                }
            }
            Close => {
                let id = self.tab().id;
                self.close_tabs(vec![id]);
            }
            CloseAll => {
                let ids = self.tabs.iter().map(|t| t.id).collect();
                self.close_tabs(ids);
            }
            CloseOthers => {
                let keep = self.tab().id;
                let ids = self.tabs.iter().map(|t| t.id).filter(|i| *i != keep).collect();
                self.close_tabs(ids);
            }
            Reopen => {
                if let Some(p) = self.closed.pop() {
                    self.open_path(&p, None);
                }
            }
            Reload => {
                let i = self.active;
                self.reload(i, false);
            }
            Undo => {
                let t = self.tab_mut();
                if t.doc.undo() {
                    t.view.reveal = Reveal::Caret;
                }
            }
            Redo => {
                let t = self.tab_mut();
                if t.doc.redo() {
                    t.view.reveal = Reveal::Caret;
                }
            }
            Cut => {
                self.focus_editor = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::RequestCut);
            }
            Copy => {
                self.focus_editor = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::RequestCopy);
            }
            Paste => {
                self.focus_editor = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::RequestPaste);
            }
            SelectAll => {
                let t = self.tab_mut();
                t.doc.sel = Sel { anchor: 0, head: t.doc.len() };
            }
            Duplicate => self.edit_cmd(|d| d.duplicate()),
            DeleteLine => self.edit_cmd(|d| d.delete_lines()),
            MoveUp => self.edit_cmd(|d| d.move_lines(false)),
            MoveDown => self.edit_cmd(|d| d.move_lines(true)),
            Join => self.lines_cmd(|lines| {
                let joined = lines.iter().enumerate().map(|(i, l)| if i == 0 { l.trim_end().to_string() } else { l.trim().to_string() }).filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" ");
                vec![joined]
            }),
            Upper => self.case_cmd(true),
            Lower => self.case_cmd(false),
            Comment => {
                let c = self.tab().lang.comment();
                if c.is_empty() {
                    self.toast("This language has no line comments", Tone::Info);
                } else {
                    self.edit_cmd(|d| d.toggle_comment(c));
                }
            }
            TrimTrailing => self.whole_or_lines(|lines| lines.iter().map(|l| l.trim_end().to_string()).collect()),
            SortAsc => self.lines_cmd(|lines| {
                let mut v: Vec<String> = lines.iter().map(|s| s.to_string()).collect();
                v.sort_by(|a, b| natural(a, b));
                v
            }),
            SortDesc => self.lines_cmd(|lines| {
                let mut v: Vec<String> = lines.iter().map(|s| s.to_string()).collect();
                v.sort_by(|a, b| natural(b, a));
                v
            }),
            Dedupe => self.lines_cmd(|lines| {
                let mut seen = std::collections::HashSet::new();
                lines.iter().filter(|l| seen.insert(l.to_string())).map(|s| s.to_string()).collect()
            }),
            TabsToSpaces => self.whole_or_lines(move |lines| lines.iter().map(|l| expand_tabs(l, tab_size)).collect()),
            SpacesToTabs => self.whole_or_lines(move |lines| lines.iter().map(|l| leading_to_tabs(l, tab_size)).collect()),
            Find => self.open_find(false),
            Replace => self.open_find(true),
            FindNext => self.find_next(true),
            FindPrev => self.find_next(false),
            FindAll => self.find_all(),
            FindInFiles => {
                if !self.find.open {
                    self.open_find(false);
                    self.find.focus = false;
                }
                let dir = if self.settings.fif_dir.is_empty() {
                    self.tab().doc.path.as_ref().and_then(|p| p.parent()).map(|p| p.display().to_string()).unwrap_or_else(|| dirs::home_dir().map(|h| h.display().to_string()).unwrap_or_default())
                } else {
                    self.settings.fif_dir.clone()
                };
                self.fif = Some(FifDialog { dir, filter: self.settings.fif_filter.clone(), hidden: self.settings.fif_hidden, focus: true });
                self.focus_editor = false;
            }
            SelectionToFind => {
                let t = self.tab();
                let r = if t.doc.sel.is_empty() { t.doc.word_at(t.doc.sel.head) } else { t.doc.sel.range() };
                if !r.is_empty() && r.end - r.start < 500 {
                    let s = String::from_utf8_lossy(&t.doc.text.read(r.clone())).into_owned();
                    self.find.query.pattern = s;
                    self.find.query.regex = false;
                    let t = self.tab_mut();
                    t.doc.sel = Sel { anchor: r.start, head: r.end };
                    self.find_next(true);
                }
            }
            GotoLine => {
                self.goto = Some(String::new());
                self.focus_editor = false;
            }
            Bookmark => self.tab_mut().doc.toggle_bookmark(),
            NextBookmark | PrevBookmark => {
                let t = self.tab_mut();
                if let Some(b) = t.doc.next_bookmark(cmd == NextBookmark) {
                    t.doc.sel = Sel::caret(b);
                    t.view.reveal = Reveal::Center;
                }
            }
            ClearBookmarks => self.tab_mut().doc.bookmarks.clear(),
            Wrap => self.settings.editor.wrap = !self.settings.editor.wrap,
            Whitespace => self.settings.editor.show_whitespace = !self.settings.editor.show_whitespace,
            LineNumbers => self.settings.editor.line_numbers = !self.settings.editor.line_numbers,
            ZoomIn => self.settings.editor.font_size = (self.settings.editor.font_size + 1.0).min(40.0),
            ZoomOut => self.settings.editor.font_size = (self.settings.editor.font_size - 1.0).max(7.0),
            ZoomReset => self.settings.editor.font_size = editor::EditorSettings::default().font_size,
            Follow => {
                let t = self.tab_mut();
                t.follow = !t.follow;
                if t.follow {
                    t.doc.sel = Sel::caret(t.doc.len());
                    t.view.reveal = Reveal::End;
                }
            }
            NextTab => {
                self.active = (self.active + 1) % self.tabs.len();
                self.focus_editor = true;
            }
            PrevTab => {
                self.active = (self.active + self.tabs.len() - 1) % self.tabs.len();
                self.focus_editor = true;
            }
            Results => self.results_open = !self.results_open && self.results.is_some(),
            Settings => {
                self.settings_open = true;
                self.focus_editor = false;
            }
            Escape => {
                if self.goto.is_some() {
                    self.goto = None;
                } else if self.fif.is_some() {
                    self.fif = None;
                } else if self.find.open {
                    self.find.open = false;
                    self.cancel_job();
                } else if self.results_open {
                    self.results_open = false;
                } else {
                    let t = self.tab_mut();
                    if !t.doc.sel.is_empty() {
                        t.doc.sel = Sel::caret(t.doc.sel.head);
                    }
                }
                self.focus_editor = true;
            }
        }
    }

    fn edit_cmd(&mut self, f: impl FnOnce(&mut Document)) {
        let t = self.tab_mut();
        f(&mut t.doc);
        t.view.reveal = Reveal::Caret;
    }

    fn lines_cmd(&mut self, f: impl FnOnce(Vec<&str>) -> Vec<String>) {
        let t = self.tab_mut();
        if !t.doc.map_lines(f) {
            self.toast("That's more text than line operations handle (256 MB)", Tone::Bad);
        }
        self.tab_mut().view.reveal = Reveal::Caret;
    }

    /// A line operation on the selection, or on the whole document when nothing is selected.
    fn whole_or_lines(&mut self, f: impl FnOnce(Vec<&str>) -> Vec<String>) {
        let t = self.tab_mut();
        if t.doc.sel.is_empty() {
            if t.doc.len() > OP_MAX {
                self.toast("That's more text than line operations handle (256 MB)", Tone::Bad);
                return;
            }
            let keep = t.doc.sel.head;
            t.doc.sel = Sel { anchor: 0, head: t.doc.len() };
            t.doc.map_lines(f);
            let len = t.doc.len();
            t.doc.sel = Sel::caret(keep.min(len));
        } else {
            self.lines_cmd(f);
        }
    }

    fn case_cmd(&mut self, upper: bool) {
        let t = self.tab_mut();
        let r = if t.doc.sel.is_empty() { t.doc.word_at(t.doc.sel.head) } else { t.doc.sel.range() };
        if r.is_empty() || r.end - r.start > OP_MAX {
            return;
        }
        let s = String::from_utf8_lossy(&t.doc.text.read(r.clone())).into_owned();
        let out = if upper { s.to_uppercase() } else { s.to_lowercase() };
        let n = out.len() as u64;
        let had = !t.doc.sel.is_empty();
        let caret = t.doc.sel.head;
        t.doc.edit(vec![(r.clone(), out.into_bytes())], margin_core::doc::Kind::Other, |_| if had { Sel { anchor: r.start, head: r.start + n } } else { Sel::caret(caret) });
    }

    // ------------------------------------------------------------ per frame

    /// Everything that isn't drawing: other launches, finished jobs, disk checks, backups.
    pub fn tick(&mut self, ctx: &egui::Context) {
        while let Ok(paths) = self.incoming.try_recv() {
            for p in paths {
                let (path, line) = split_position(&p);
                self.open_path(&path, line);
            }
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        }
        self.poll_saves();
        self.poll_jobs();
        self.poll_results();
        self.update_count();
        if self.last_disk_check.elapsed() > Duration::from_millis(1000) {
            self.last_disk_check = Instant::now();
            self.check_disk();
        }
        if self.last_backup.elapsed() > Duration::from_secs(3) && self.settings.restore_session {
            self.last_backup = Instant::now();
            self.backup(false);
        }
        if self.last_session.elapsed() > Duration::from_secs(15) {
            self.last_session = Instant::now();
            self.save_session();
        }
        self.toasts.retain(|t| t.at.elapsed() < Duration::from_millis(4200));
        let busy = self.find.job.is_some() || self.tabs.iter().any(|t| t.saving.is_some() || !t.doc.text.indexed()) || self.results.as_ref().is_some_and(|r| !r.done) || !self.toasts.is_empty();
        ctx.request_repaint_after(if busy { Duration::from_millis(100) } else { Duration::from_millis(1000) });

        // theme
        let dark = ctx.theme() == egui::Theme::Dark;
        if self.syn_dark != Some(dark) {
            self.syn_dark = Some(dark);
            self.palette = if dark { theme::DARK } else { theme::LIGHT };
            self.syn_theme = syntax::theme_for(&self.palette);
        }

        // title
        let t = self.tab();
        let title = format!("{}{} — Margin", if t.doc.dirty() { "• " } else { "" }, t.title());
        if title != self.title {
            self.title = title.clone();
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title));
        }
    }

    /// Runs shortcuts, leaving text commands to a focused input field.
    pub fn shortcuts(&mut self, ctx: &egui::Context) {
        let editor_focused = ctx.memory(|m| m.focused() == Some(self.tab().editor_id()));
        let field_focused = ctx.memory(|m| m.focused().is_some()) && !editor_focused;
        let mut todo = Vec::new();
        ctx.input_mut(|i| {
            for (cmd, k) in shortcuts() {
                if field_focused && cmd.text_only() {
                    continue;
                }
                if cmd == Cmd::Escape && (self.ask.is_some() || self.settings_open) {
                    continue;
                }
                if i.consume_shortcut(&k) {
                    todo.push(cmd);
                }
            }
        });
        for c in todo {
            self.run(c, ctx);
        }
        // Ctrl + wheel: text size
        let zoom = ctx.input(|i| i.zoom_delta());
        if zoom != 1.0 {
            let f = &mut self.settings.editor.font_size;
            *f = (*f * zoom).clamp(7.0, 40.0);
        }
        // files dropped on the window
        let dropped: Vec<PathBuf> = ctx.input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).collect());
        for p in dropped {
            self.open_path(&p, None);
        }
    }
}

/// One search hit with a preview of its line.
fn hit_in(text: &Text, m: Range<u64>, tab: Option<u64>, path: Option<PathBuf>) -> Hit {
    let ls = match text.rfind_byte(b'\n', m.start.saturating_sub(400)..m.start) {
        Some(n) => n + 1,
        None => m.start.saturating_sub(if m.start > 400 { 120 } else { m.start }),
    };
    let le = text.find_byte(b'\n', m.end..(m.end + 400).min(text.len())).unwrap_or((m.end + 200).min(text.len()));
    let (pre_start, cut_front) = if m.start - ls > 120 { (m.start - 60, true) } else { (ls, false) };
    let bytes = text.read(pre_start..le.min(pre_start + 600));
    let raw = String::from_utf8_lossy(&bytes).replace('\t', "  ").replace('\r', "");
    let lead = if cut_front { "…" } else { "" };
    let ms = String::from_utf8_lossy(&text.read(pre_start..m.start)).replace('\t', "  ").replace('\r', "").len();
    let me = ms + String::from_utf8_lossy(&text.read(m.start..m.end.min(m.start + 300))).replace('\t', "  ").len();
    let preview = format!("{lead}{}", raw.trim_end());
    let off = lead.len();
    let mark = (ms + off).min(preview.len())..(me + off).min(preview.len());
    Hit { path, tab, at: m.start, len: m.end - m.start, line: text.line_of(m.start), preview, mark }
}

fn search_folder(dir: PathBuf, filter: String, hidden: bool, re: Regex, tx: Sender<Vec<Hit>>, p: Arc<search::Progress>) {
    let mut ob = ignore::overrides::OverrideBuilder::new(&dir);
    for pat in filter.split([';', ',', ' ']).map(str::trim).filter(|s| !s.is_empty()) {
        let _ = ob.add(pat);
    }
    let overrides = ob.build().ok();
    let mut wb = ignore::WalkBuilder::new(&dir);
    wb.hidden(!hidden).git_ignore(true).parents(true);
    if let Some(o) = overrides {
        wb.overrides(o);
    }
    let total = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    wb.threads(std::thread::available_parallelism().map(|n| n.get().min(12)).unwrap_or(4)).build_parallel().run(|| {
        let tx = tx.clone();
        let re = re.clone();
        let p = p.clone();
        let total = total.clone();
        Box::new(move |entry| {
            if p.cancel.load(Ordering::Relaxed) || total.load(Ordering::Relaxed) >= 200_000 {
                return ignore::WalkState::Quit;
            }
            let Ok(e) = entry else { return ignore::WalkState::Continue };
            if !e.file_type().is_some_and(|t| t.is_file()) {
                return ignore::WalkState::Continue;
            }
            p.done.fetch_add(1, Ordering::Relaxed);
            let Ok(o) = file::open(e.path(), None) else { return ignore::WalkState::Continue };
            if o.binary {
                return ignore::WalkState::Continue;
            }
            let path = e.path().to_path_buf();
            let mut hits = Vec::new();
            search::find_all(&o.text, &re, 5000, &search::Progress::default(), |batch| {
                for m in batch {
                    hits.push(hit_in(&o.text, m.clone(), None, Some(path.clone())));
                }
            });
            if !hits.is_empty() {
                total.fetch_add(hits.len(), Ordering::Relaxed);
                let _ = tx.send(hits);
            }
            ignore::WalkState::Continue
        })
    });
}

/// "file2" before "file10".
fn natural(a: &str, b: &str) -> std::cmp::Ordering {
    let (mut x, mut y) = (a.chars().peekable(), b.chars().peekable());
    loop {
        match (x.peek().copied(), y.peek().copied()) {
            (None, None) => return std::cmp::Ordering::Equal,
            (None, _) => return std::cmp::Ordering::Less,
            (_, None) => return std::cmp::Ordering::Greater,
            (Some(c), Some(d)) if c.is_ascii_digit() && d.is_ascii_digit() => {
                let mut n1 = String::new();
                while let Some(c) = x.peek().copied().filter(|c| c.is_ascii_digit()) {
                    n1.push(c);
                    x.next();
                }
                let mut n2 = String::new();
                while let Some(d) = y.peek().copied().filter(|c| c.is_ascii_digit()) {
                    n2.push(d);
                    y.next();
                }
                let o = n1.trim_start_matches('0').len().cmp(&n2.trim_start_matches('0').len()).then(n1.trim_start_matches('0').cmp(n2.trim_start_matches('0')));
                if o != std::cmp::Ordering::Equal {
                    return o;
                }
            }
            (Some(c), Some(d)) => {
                let o = c.to_lowercase().cmp(d.to_lowercase());
                if o != std::cmp::Ordering::Equal {
                    return o;
                }
                x.next();
                y.next();
            }
        }
    }
}

fn expand_tabs(l: &str, w: usize) -> String {
    let mut out = String::with_capacity(l.len());
    let mut col = 0;
    for c in l.chars() {
        if c == '\t' {
            let n = w - col % w.max(1);
            out.extend(std::iter::repeat_n(' ', n));
            col += n;
        } else {
            out.push(c);
            col += 1;
        }
    }
    out
}

fn leading_to_tabs(l: &str, w: usize) -> String {
    let lead = l.len() - l.trim_start_matches(' ').len();
    let tabs = lead / w.max(1);
    format!("{}{}{}", "\t".repeat(tabs), " ".repeat(lead % w.max(1)), &l[lead..])
}
