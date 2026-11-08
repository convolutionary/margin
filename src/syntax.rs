//! Syntax highlighting for what's on screen.
//!
//! Only visible rows are highlighted. The parser's state after each row is kept, so scrolling down
//! continues where it left off; jumping somewhere new starts a little above the screen, which is
//! right for everything except comments or strings that began hundreds of lines earlier.

use std::collections::HashMap;
use std::path::Path;
use std::str::FromStr;
use std::sync::OnceLock;

use egui::Color32;
use regex::Regex;
use syntect::highlighting::{Color, FontStyle, HighlightIterator, HighlightState, Highlighter, ScopeSelectors, StyleModifier, Theme, ThemeItem, ThemeSettings};
use syntect::parsing::{ParseState, ScopeStack, SyntaxReference, SyntaxSet};

use crate::theme::Palette;

/// Rows longer than this aren't highlighted (minified files, huge log lines): it keeps them fast.
pub const MAX_ROW: usize = 3000;
/// Rows parsed above the screen when starting fresh.
const WARMUP: usize = 120;

pub fn syntaxes() -> &'static SyntaxSet {
    static SET: OnceLock<SyntaxSet> = OnceLock::new();
    SET.get_or_init(two_face::syntax::extra_newlines)
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Lang {
    Plain,
    Log,
    /// a syntax by name
    Syntax(String),
}

impl Lang {
    pub fn name(&self) -> &str {
        match self {
            Lang::Plain => "Plain text",
            Lang::Log => "Log",
            Lang::Syntax(n) => n,
        }
    }

    pub fn syntax(&self) -> Option<&'static SyntaxReference> {
        match self {
            Lang::Syntax(n) => syntaxes().find_syntax_by_name(n),
            _ => None,
        }
    }

    /// The line comment for this language, for "toggle comment".
    pub fn comment(&self) -> &'static str {
        let n = self.name().to_ascii_lowercase();
        let hash = ["python", "shell", "bash", "ruby", "perl", "yaml", "toml", "makefile", "dockerfile", "r", "cmake", "nix", "powershell", "elixir", "julia", "nim", "fish", "conf", "ini", "gitignore", "git", "requirements", "crystal", "tcl"];
        let dash = ["sql", "lua", "haskell", "elm", "ada", "vhdl"];
        let semi = ["lisp", "clojure", "scheme", "assembly", "asm", "racket", "nasm"];
        let pct = ["latex", "tex", "erlang", "matlab", "prolog"];
        if hash.iter().any(|h| n.contains(h)) {
            "# "
        } else if dash.iter().any(|h| n.contains(h)) {
            "-- "
        } else if semi.iter().any(|h| n.contains(h)) {
            "; "
        } else if pct.iter().any(|h| n.contains(h)) {
            "% "
        } else if n.contains("vim") {
            "\" "
        } else if n.contains("batch") {
            "REM "
        } else if matches!(self, Lang::Plain | Lang::Log) || n.contains("markdown") || n.contains("json") && !n.contains("jsonc") {
            ""
        } else {
            "// "
        }
    }
}

/// Picks a language from the file name and first line. Big unknown files are treated as logs.
pub fn detect(path: Option<&Path>, first_line: &str, size: u64) -> Lang {
    let ss = syntaxes();
    if let Some(p) = path {
        let name = p.file_name().map(|n| n.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
        let ext = p.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
        if ext == "log" || name.ends_with(".log.1") || name == "syslog" || name == "messages" || ext == "out" && size > 1 << 20 {
            return Lang::Log;
        }
        if ext == "txt" || ext == "text" {
            return Lang::Plain;
        }
        if let Some(s) = ss.find_syntax_by_extension(&name).or_else(|| ss.find_syntax_by_extension(&ext)) {
            if s.name != "Plain Text" {
                return Lang::Syntax(s.name.clone());
            }
        }
    }
    if let Some(s) = ss.find_syntax_by_first_line(first_line)
        && s.name != "Plain Text"
    {
        return Lang::Syntax(s.name.clone());
    }
    if size > 16 << 20 || log_like(first_line) {
        return Lang::Log;
    }
    Lang::Plain
}

fn log_like(line: &str) -> bool {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^(\[?\d{4}-\d{2}-\d{2}[T ]\d{2}:\d{2}|\w{3} [ \d]\d \d{2}:\d{2}:\d{2}|\[\s*\d+\.\d+\])").unwrap()).is_match(line)
}

/// One coloured stretch of a row: byte range in the row, colour, italic.
#[derive(Clone, Copy, Debug)]
pub struct Span {
    pub start: u32,
    pub end: u32,
    pub color: Color32,
    pub italic: bool,
}

fn to_color(c: Color) -> Color32 {
    Color32::from_rgba_unmultiplied(c.r, c.g, c.b, c.a)
}

fn from32(c: Color32) -> Color {
    Color { r: c.r(), g: c.g(), b: c.b(), a: 255 }
}

/// A syntect theme made from our palette, so code looks like the rest of the app.
pub fn theme_for(p: &Palette) -> Theme {
    let item = |scope: &str, c: Color32, style: FontStyle| ThemeItem {
        scope: ScopeSelectors::from_str(scope).unwrap(),
        style: StyleModifier { foreground: Some(from32(c)), background: None, font_style: Some(style) },
    };
    let n = FontStyle::empty();
    Theme {
        name: Some("Margin".into()),
        author: None,
        settings: ThemeSettings { foreground: Some(from32(p.text)), background: Some(from32(p.editor)), ..Default::default() },
        scopes: vec![
            item("comment, punctuation.definition.comment", p.comment, FontStyle::ITALIC),
            item("string, punctuation.definition.string, string.quoted", p.string, n),
            item("constant.character.escape, string.regexp", p.constant, n),
            item("constant.numeric", p.number, n),
            item("constant.language, constant.other, support.constant, variable.language", p.constant, n),
            item("keyword, storage, keyword.control, keyword.other", p.keyword, n),
            item("keyword.operator, punctuation.separator.key-value", p.operator, n),
            item("storage.type, support.type, entity.name.type, entity.name.class, entity.name.struct, entity.name.enum, entity.other.inherited-class", p.r#type, n),
            item("entity.name.function, support.function, meta.function-call variable.function, variable.function", p.function, n),
            item("entity.name.tag, meta.tag.sgml", p.tag, n),
            item("entity.other.attribute-name", p.attribute, n),
            item("meta.mapping.key string, support.type.property-name, meta.object-literal.key, entity.name.label, variable.other.member", p.function, n),
            item("markup.heading, markup.heading punctuation", p.keyword, FontStyle::BOLD),
            item("markup.bold", p.text, FontStyle::BOLD),
            item("markup.italic", p.text, FontStyle::ITALIC),
            item("markup.underline.link, markup.link", p.function, n),
            item("markup.raw, markup.inline.raw", p.string, n),
            item("markup.inserted", p.ok, n),
            item("markup.deleted", p.bad, n),
            item("markup.changed", p.warn, n),
            item("meta.diff.range, meta.diff.header", p.r#type, n),
            item("punctuation, meta.brace", p.punctuation, n),
            item("invalid", p.invalid, n),
            item("entity.name.section", p.keyword, n),
        ],
    }
}

type State = (ParseState, HighlightState);

/// Per-document cache of highlighted rows, thrown away when the text changes.
pub struct Highlights {
    version: u64,
    lang: Lang,
    dark: bool,
    rows: HashMap<u64, (Vec<Span>, Option<State>)>,
}

impl Default for Highlights {
    fn default() -> Self {
        Highlights { version: 0, lang: Lang::Plain, dark: true, rows: HashMap::new() }
    }
}

impl Highlights {
    /// Highlights `rows` (start offset and text of each visible row, in order). `before` gives the
    /// rows above the first one, nearest first, for warming up the parser.
    pub fn get(
        &mut self,
        version: u64,
        lang: &Lang,
        palette: &Palette,
        theme: &Theme,
        rows: &[(u64, &[u8])],
        before: impl FnOnce(usize) -> Vec<(u64, Vec<u8>)>,
    ) -> Vec<Vec<Span>> {
        if self.version != version || &self.lang != lang || self.dark != palette.dark {
            self.rows.clear();
            self.version = version;
            self.lang = lang.clone();
            self.dark = palette.dark;
        }
        if self.rows.len() > 20_000 {
            self.rows.clear();
        }
        match lang {
            Lang::Plain => rows.iter().map(|_| Vec::new()).collect(),
            Lang::Log => rows
                .iter()
                .map(|(s, t)| self.rows.entry(*s).or_insert_with(|| (log_spans(t, palette), None)).0.clone())
                .collect(),
            Lang::Syntax(_) => {
                let Some(syntax) = lang.syntax() else { return rows.iter().map(|_| Vec::new()).collect() };
                let hl = Highlighter::new(theme);
                let fresh = || (ParseState::new(syntax), HighlightState::new(&hl, ScopeStack::new()));
                let mut out = Vec::with_capacity(rows.len());
                // state entering the first row: cached from the row above, or warm up
                let mut state: Option<State> = None;
                if let Some((first, _)) = rows.first()
                    && !self.rows.contains_key(first)
                {
                    let above = before(WARMUP);
                    if let Some((p, _)) = above.first() {
                        state = self.rows.get(p).and_then(|r| r.1.clone());
                    }
                    if state.is_none() {
                        let mut st = fresh();
                        for (_, t) in above.iter().rev() {
                            highlight_row(t, &mut st, &hl);
                        }
                        state = Some(st);
                    }
                }
                for (s, t) in rows {
                    if let Some(hit) = self.rows.get(s)
                        && hit.1.is_some()
                    {
                        out.push(hit.0.clone());
                        state = hit.1.clone();
                        continue;
                    }
                    let mut st = state.take().unwrap_or_else(fresh);
                    let spans = highlight_row(t, &mut st, &hl);
                    self.rows.insert(*s, (spans.clone(), Some(st.clone())));
                    out.push(spans);
                    state = Some(st);
                }
                out
            }
        }
    }
}

fn highlight_row(bytes: &[u8], st: &mut State, hl: &Highlighter) -> Vec<Span> {
    if bytes.len() > MAX_ROW {
        return Vec::new();
    }
    let Ok(text) = std::str::from_utf8(bytes) else { return Vec::new() };
    // the newline-aware syntaxes want the line break
    let line = format!("{text}\n");
    let Ok(ops) = st.0.parse_line(&line, syntaxes()) else { return Vec::new() };
    let mut spans = Vec::new();
    let mut at = 0u32;
    for (style, piece) in HighlightIterator::new(&mut st.1, &ops, &line, hl) {
        let end = (at as usize + piece.len()).min(text.len()) as u32;
        if end > at {
            spans.push(Span { start: at, end, color: to_color(style.foreground), italic: style.font_style.contains(FontStyle::ITALIC) });
        }
        at += piece.len() as u32;
    }
    spans
}

/// Colours for log lines: timestamps, levels, quoted strings, key=value keys, numbers.
fn log_spans(bytes: &[u8], p: &Palette) -> Vec<Span> {
    struct Rules {
        all: Regex,
    }
    static RULES: OnceLock<Rules> = OnceLock::new();
    let r = RULES.get_or_init(|| Rules {
        all: Regex::new(concat!(
            r"(?P<ts>\b\d{4}-\d{2}-\d{2}[T ]\d{2}:\d{2}:\d{2}(?:[.,]\d+)?(?:Z|[+-]\d{2}:?\d{2})?|\b\w{3} [ \d]\d \d{2}:\d{2}:\d{2}\b|^\[\s*\d+\.\d+\])",
            r"|(?P<bad>\b(?:ERROR|ERR|FATAL|CRITICAL|CRIT|PANIC|EMERG|ALERT|SEVERE|FAIL(?:ED|URE)?|Exception|Traceback)\b)",
            r"|(?P<warn>\b(?:WARN(?:ING)?|DEPRECATED)\b)",
            r"|(?P<info>\b(?:INFO|NOTICE)\b)",
            r"|(?P<dim>\b(?:DEBUG|TRACE|VERBOSE)\b)",
            r#"|(?P<str>"[^"\n]{0,400}"|'[^'\n]{0,200}')"#,
            r"|(?P<key>\b[A-Za-z_][\w.-]{0,40}(?:=|: ))",
            r"|(?P<num>\b(?:\d{1,3}\.){3}\d{1,3}(?::\d+)?\b|\b0x[0-9a-fA-F]+\b|\b[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\b|\b\d+(?:\.\d+)?(?:ms|s|us|µs|ns|KB|MB|GB|%)?\b)",
        ))
        .unwrap(),
    });
    if bytes.len() > MAX_ROW {
        return Vec::new();
    }
    let text = String::from_utf8_lossy(bytes);
    if text.len() != bytes.len() {
        return Vec::new(); // offsets would be off; leave odd bytes plain
    }
    let mut out = Vec::new();
    for c in r.all.captures_iter(&text) {
        let (name, m) = ["ts", "bad", "warn", "info", "dim", "str", "key", "num"]
            .iter()
            .find_map(|n| c.name(n).map(|m| (*n, m)))
            .unwrap();
        let (color, italic) = match name {
            "ts" => (p.text3, false),
            "bad" => (p.bad, false),
            "warn" => (p.warn, false),
            "info" => (p.ok, false),
            "dim" => (p.text3, false),
            "str" => (p.string, false),
            "key" => (p.function, false),
            _ => (p.number, false),
        };
        // a key's colour stops before its "=" or ": "
        let end = if name == "key" { m.start() + m.as_str().trim_end_matches([':', ' ', '=']).len() } else { m.end() };
        out.push(Span { start: m.start() as u32, end: end.max(m.start()) as u32, color, italic });
    }
    out
}

/// Every language the menu offers, by name, sorted.
pub fn all_languages() -> Vec<String> {
    let mut v: Vec<String> = syntaxes().syntaxes().iter().filter(|s| !s.hidden && s.name != "Plain Text").map(|s| s.name.clone()).collect();
    v.sort_by_key(|a| a.to_lowercase());
    v.dedup();
    v
}
