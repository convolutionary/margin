//! The text area: draws the rows on screen and turns keys and the mouse into edits and caret moves.
//!
//! Nothing here ever looks at the whole document. Each frame lays out the rows that fit, starting
//! from `View::top`; moving and scrolling step row by row from there.

use std::ops::Range;
use std::sync::Arc;

use egui::text::{LayoutJob, TextFormat};
use egui::{Color32, CornerRadius, CursorIcon, Event, FontFamily, FontId, Galley, Id, Key, Modifiers, Pos2, Rect, Response, Sense, Stroke, Ui, Vec2, pos2, vec2};
use margin_core::doc::{Document, Kind, OP_MAX, Sel};
use margin_core::rows;
use regex::bytes::Regex;

use crate::syntax::{Highlights, Lang, Span};
use crate::theme::{MONO_ITALIC, Palette};

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct EditorSettings {
    pub font_size: f32,
    pub line_spacing: f32,
    pub tab_width: usize,
    pub insert_spaces: bool,
    pub wrap: bool,
    pub show_whitespace: bool,
    pub line_numbers: bool,
    pub highlight_line: bool,
    pub auto_indent: bool,
    pub smart_highlight: bool,
    pub caret_blink: bool,
}

impl Default for EditorSettings {
    fn default() -> Self {
        EditorSettings {
            font_size: 13.5,
            line_spacing: 1.45,
            tab_width: 4,
            insert_spaces: true,
            wrap: false,
            show_whitespace: false,
            line_numbers: true,
            highlight_line: true,
            auto_indent: true,
            smart_highlight: true,
            caret_blink: true,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Reveal {
    #[default]
    None,
    /// scroll just enough to show the caret
    Caret,
    /// put the caret's line in the middle
    Center,
    /// show the end of the document (following a growing file)
    End,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Gran {
    Char,
    Word,
    Line,
}

#[derive(Clone, Debug)]
enum Drag {
    Select { gran: Gran, origin: Range<u64> },
    Thumb { grab: f32 },
}

/// Per-tab view state.
pub struct View {
    pub top: u64,
    pub top_sub: usize,
    pub scroll_x: f32,
    widest: f32,
    want_x: Option<f32>,
    pub reveal: Reveal,
    drag: Option<Drag>,
    pub overwrite: bool,
    wheel: f32,
    /// visual lines that fit, from the last frame
    pub page: usize,
    pub hl: Highlights,
    blink: f64,
    /// the caret's screen rectangle last frame (for popups)
    pub caret_rect: Option<Rect>,
}

impl Default for View {
    fn default() -> Self {
        View {
            top: 0,
            top_sub: 0,
            scroll_x: 0.0,
            widest: 0.0,
            want_x: None,
            reveal: Reveal::None,
            drag: None,
            overwrite: false,
            wheel: 0.0,
            page: 30,
            hl: Highlights::default(),
            blink: 0.0,
            caret_rect: None,
        }
    }
}

/// What the app needs from a frame of the editor.
pub struct Output {
    pub response: Response,
    pub edited: bool,
    #[allow(dead_code)]
    pub moved: bool,
}

/// A laid-out row.
struct RowLay {
    start: u64,
    text_end: u64,
    next: Option<u64>,
    galley: Arc<Galley>,
    /// row byte -> display char
    b2c: Vec<u32>,
    /// display char -> row byte
    c2b: Vec<u32>,
    bytes: Vec<u8>,
}

impl RowLay {
    fn char_of(&self, at: u64) -> usize {
        let i = (at.saturating_sub(self.start)).min(self.b2c.len() as u64 - 1) as usize;
        self.b2c[i] as usize
    }
    fn byte_of(&self, c: usize) -> u64 {
        self.start + self.c2b[c.min(self.c2b.len() - 1)] as u64
    }
    fn lines(&self) -> usize {
        self.galley.rows.len().max(1)
    }
    /// Visual line and x of display char `c` (a wrap point belongs to the next line).
    fn locate(&self, c: usize) -> (usize, f32) {
        let mut ci = 0;
        let n = self.galley.rows.len();
        for (i, r) in self.galley.rows.iter().enumerate() {
            let cnt = r.row.glyphs.len();
            if c < ci + cnt || i + 1 == n {
                let local = c.saturating_sub(ci).min(cnt);
                return (i, r.pos.x + r.row.x_offset(epaint_index(local)));
            }
            ci += cnt;
        }
        (0, 0.0)
    }
    /// Display char nearest to `x` on visual line `line`.
    fn char_at(&self, line: usize, x: f32) -> usize {
        let mut ci = 0;
        for (i, r) in self.galley.rows.iter().enumerate() {
            let cnt = r.row.glyphs.len();
            if i == line.min(self.galley.rows.len().saturating_sub(1)) {
                let local = r.row.char_at(x - r.pos.x).0;
                // the last char of a wrapped line is its wrap point: stay on this line
                let last = i + 1 < self.galley.rows.len();
                return ci + if last { local.min(cnt.saturating_sub(1)) } else { local };
            }
            ci += cnt;
        }
        0
    }
    fn line_y(&self, line: usize) -> f32 {
        self.galley.rows.get(line).map(|r| r.pos.y).unwrap_or(0.0)
    }
}

fn epaint_index(i: usize) -> egui::epaint::text::CharIndex {
    egui::epaint::text::CharIndex(i)
}

/// Bytes of a row to display text: tabs expanded, control characters and bad bytes made visible.
/// Calls `emit(text, whitespace)` per run so whitespace can be drawn dimmer.
struct Builder<'a> {
    b2c: Vec<u32>,
    c2b: Vec<u32>,
    col: usize,
    tab: usize,
    ws: bool,
    emit: &'a mut dyn FnMut(&str, bool),
}

impl Builder<'_> {
    fn push(&mut self, bytes: &[u8], base: usize) {
        let mut run = String::new();
        let mut run_ws = false;
        let flush = |run: &mut String, ws: bool, emit: &mut dyn FnMut(&str, bool)| {
            if !run.is_empty() {
                emit(run, ws);
                run.clear();
            }
        };
        let mut off = base;
        for chunk in bytes.utf8_chunks() {
            for (i, ch) in chunk.valid().char_indices() {
                let at = off + i;
                let first_char = self.c2b.len() as u32;
                let (text, is_ws): (String, bool) = match ch {
                    '\t' => {
                        let n = self.tab - self.col % self.tab;
                        let s = if self.ws { format!("→{}", " ".repeat(n - 1)) } else { " ".repeat(n) };
                        (s, true)
                    }
                    ' ' if self.ws => ("·".into(), true),
                    c if (c as u32) < 0x20 => (char::from_u32(0x2400 + c as u32).unwrap().to_string(), false),
                    '\u{7f}' => ("␡".into(), false),
                    c => (c.to_string(), false),
                };
                if is_ws != run_ws {
                    flush(&mut run, run_ws, self.emit);
                    run_ws = is_ws;
                }
                let chars = text.chars().count();
                for _ in 0..ch.len_utf8() {
                    self.b2c.push(first_char);
                }
                for _ in 0..chars {
                    self.c2b.push(at as u32);
                }
                self.col += chars;
                run.push_str(&text);
            }
            off += chunk.valid().len();
            for (k, _) in chunk.invalid().iter().enumerate() {
                if run_ws {
                    flush(&mut run, true, self.emit);
                    run_ws = false;
                }
                self.b2c.push(self.c2b.len() as u32);
                self.c2b.push((off + k) as u32);
                self.col += 1;
                run.push('\u{FFFD}');
            }
            off += chunk.invalid().len();
        }
        flush(&mut run, run_ws, self.emit);
    }
}

pub struct Ctx<'a> {
    pub settings: &'a EditorSettings,
    pub palette: &'a Palette,
    pub theme: &'a syntect::highlighting::Theme,
    pub lang: &'a Lang,
    pub find: Option<&'a Regex>,
}

fn mono(size: f32) -> FontId {
    FontId::new(size, FontFamily::Monospace)
}

fn line_height(ui: &Ui, s: &EditorSettings) -> f32 {
    let h = ui.fonts_mut(|f| f.row_height(&mono(s.font_size)));
    (h * s.line_spacing).round()
}

/// Lays out one row. `spans` colours it (empty: plain).
fn layout(ui: &Ui, doc: &Document, start: u64, cx: &Ctx, wrap: f32, spans: &[Span], with_eol: bool) -> RowLay {
    let (text_end, next) = rows::row_end(&doc.text, start);
    let bytes = doc.text.read(start..text_end);
    let s = cx.settings;
    let p = cx.palette;
    let lh = line_height(ui, s);
    let font = mono(s.font_size);
    let italic = FontId::new(s.font_size, FontFamily::Name(MONO_ITALIC.into()));
    let mut job = LayoutJob::default();
    job.wrap.max_width = if s.wrap { wrap.max(40.0) } else { f32::INFINITY };
    job.wrap.break_anywhere = false;
    job.break_on_newline = false;
    let fmt = |color: Color32, it: bool| TextFormat { font_id: if it { italic.clone() } else { font.clone() }, color, line_height: Some(lh), valign: egui::Align::Center, ..Default::default() };
    let ws_color = p.text4;
    let (mut b2c, mut c2b) = {
        let job_ref = &mut job;
        let mut emit = |t: &str, _ws: bool| job_ref.append(t, 0.0, fmt(p.text, false));
        let mut b = Builder { b2c: Vec::with_capacity(bytes.len() + 1), c2b: Vec::with_capacity(bytes.len() + 1), col: 0, tab: s.tab_width.max(1), ws: s.show_whitespace, emit: &mut emit };
        b.push(&bytes, 0);
        (std::mem::take(&mut b.b2c), std::mem::take(&mut b.c2b))
    };
    // colour by syntax spans and dim the whitespace markers
    recolor(&mut job, &bytes, spans, &b2c, p, s.show_whitespace, &font, &italic, lh);
    let chars = c2b.len() as u32;
    b2c.push(chars);
    c2b.push(bytes.len() as u32);
    if with_eol && s.show_whitespace && next.is_some() && rows::starts_line(&doc.text, next.unwrap()) {
        let crlf = doc.text.byte(text_end) == Some(b'\r');
        job.append(if crlf { "↵" } else { "¬" }, 0.0, fmt(ws_color, false));
    }
    if job.text.is_empty() {
        job.append("", 0.0, fmt(p.text, false));
    }
    let galley = ui.fonts_mut(|f| f.layout_job(job));
    RowLay { start, text_end, next, galley, b2c, c2b, bytes }
}

/// Splits the job's sections so each display char gets its span's colour.
#[allow(clippy::too_many_arguments)]
fn recolor(job: &mut LayoutJob, bytes: &[u8], spans: &[Span], b2c: &[u32], p: &Palette, ws: bool, font: &FontId, italic: &FontId, lh: f32) {
    if spans.is_empty() && !ws {
        return;
    }
    // display char -> byte offset in job.text
    let char_bytes: Vec<usize> = job.text.char_indices().map(|(i, _)| i).chain(std::iter::once(job.text.len())).collect();
    let nchars = char_bytes.len() - 1;
    let mut colors: Vec<(Color32, bool)> = vec![(p.text, false); nchars];
    for sp in spans {
        let a = (sp.start as usize).min(bytes.len());
        let e = (sp.end as usize).min(bytes.len());
        if a >= e {
            continue;
        }
        let ca = b2c[a] as usize;
        let ce = if e < b2c.len() { b2c[e] as usize } else { nchars };
        for c in colors.iter_mut().take(ce.min(nchars)).skip(ca) {
            *c = (sp.color, sp.italic);
        }
    }
    if ws {
        for (i, ch) in job.text.chars().enumerate() {
            if (ch == '·' || ch == '→') && i < colors.len() {
                colors[i] = (p.text4, false);
            }
        }
        // spaces after a tab arrow belong to it
    }
    let mut sections = Vec::new();
    let mut i = 0;
    while i < nchars {
        let mut j = i + 1;
        while j < nchars && colors[j] == colors[i] {
            j += 1;
        }
        let (c, it) = colors[i];
        sections.push(egui::text::LayoutSection {
            leading_space: 0.0,
            byte_range: egui::text::ByteIndex(char_bytes[i])..egui::text::ByteIndex(char_bytes[j]),
            format: TextFormat { font_id: if it { italic.clone() } else { font.clone() }, color: c, line_height: Some(lh), valign: egui::Align::Center, ..Default::default() },
        });
        i = j;
    }
    if !sections.is_empty() {
        job.sections = sections;
    }
}

/// The editor for one document. `id` must be stable per tab.
pub fn show(ui: &mut Ui, doc: &mut Document, view: &mut View, cx: &Ctx, id: Id) -> Output {
    let p = *cx.palette;
    let s = cx.settings.clone();
    let rect = ui.available_rect_before_wrap();
    let response = ui.interact(rect, id, Sense::click_and_drag());
    let focused = response.has_focus();
    if response.clicked() || response.drag_started() || response.secondary_clicked() {
        response.request_focus();
    }
    if focused {
        ui.memory_mut(|m| m.set_focus_lock_filter(id, egui::EventFilter { tab: true, horizontal_arrows: true, vertical_arrows: true, escape: false }));
    }
    let lh = line_height(ui, &s);
    let char_w = ui.fonts_mut(|f| f.glyph_width(&mono(s.font_size), '0'));

    // gutter: line numbers and a bookmark lane
    let digits = doc.text.line_count().map(|n| n.to_string().len()).unwrap_or(6).max(3);
    let gutter = if s.line_numbers { 18.0 + digits as f32 * char_w + 14.0 } else { 18.0 };
    let bar_w = 12.0;
    let text_rect = Rect::from_min_max(pos2(rect.left() + gutter, rect.top()), pos2(rect.right() - bar_w, rect.bottom()));
    let pad_x = 6.0;
    let wrap_w = text_rect.width() - pad_x * 2.0;
    view.page = ((rect.height() / lh).floor() as usize).max(1);

    let mut edited = false;
    let mut moved = false;

    // ---- keyboard and clipboard
    if focused {
        let events = ui.input(|i| i.events.clone());
        for ev in events {
            let before = (doc.version, doc.sel);
            handle_event(ui, doc, view, &s, &ev, wrap_w, cx);
            if doc.version != before.0 {
                edited = true;
            }
            if doc.sel != before.1 || doc.version != before.0 {
                moved = true;
                view.blink = ui.input(|i| i.time);
                if view.reveal == Reveal::None {
                    view.reveal = Reveal::Caret;
                }
            }
        }
    }

    // ---- mouse wheel
    if response.hovered() || response.contains_pointer() {
        let (delta, ctrl) = ui.input(|i| (i.smooth_scroll_delta, i.modifiers.command));
        if !ctrl && delta != Vec2::ZERO {
            if delta.y != 0.0 {
                view.wheel -= delta.y / lh;
                let whole = view.wheel.trunc() as i64;
                if whole != 0 {
                    view.wheel -= whole as f32;
                    scroll_lines(ui, doc, view, cx, wrap_w, whole);
                }
            }
            if delta.x != 0.0 && !s.wrap {
                view.scroll_x = (view.scroll_x - delta.x).clamp(0.0, (view.widest - wrap_w * 0.5).max(0.0));
            }
        }
    }

    // ---- reveal requests
    match std::mem::take(&mut view.reveal) {
        Reveal::None => {}
        Reveal::Caret => reveal_caret(ui, doc, view, cx, wrap_w, false),
        Reveal::Center => reveal_caret(ui, doc, view, cx, wrap_w, true),
        Reveal::End => {
            view.top = rows::row_start(&doc.text, doc.len());
            view.top_sub = 0;
            let back = view.page.saturating_sub(2) as i64;
            scroll_lines(ui, doc, view, cx, wrap_w, -back);
        }
    }

    // ---- lay out what fits
    let mut shown: Vec<(RowLay, f32)> = Vec::new();
    {
        // plain layout first to know which rows are visible, then colour them
        let mut at = Some(view.top);
        let mut y = -(view.top_sub as f32) * lh;
        let mut starts = Vec::new();
        while let Some(r) = at {
            if y > rect.height() {
                break;
            }
            let (_, next) = rows::row_end(&doc.text, r);
            starts.push(r);
            // height needs a layout only when wrapping
            y += if s.wrap { layout(ui, doc, r, cx, wrap_w, &[], false).lines() as f32 * lh } else { lh };
            at = next;
            if starts.len() > 400 {
                break;
            }
        }
        let texts: Vec<(u64, Vec<u8>)> = starts.iter().map(|&r| (r, doc.text.read(r..rows::row_end(&doc.text, r).0))).collect();
        let refs: Vec<(u64, &[u8])> = texts.iter().map(|(r, t)| (*r, t.as_slice())).collect();
        let top = view.top;
        let spans = view.hl.get(doc.version, cx.lang, &p, cx.theme, &refs, |n| {
            let mut out = Vec::new();
            let mut r = top;
            for _ in 0..n {
                match rows::prev_row(&doc.text, r) {
                    Some(pr) => {
                        out.push((pr, doc.text.read(pr..rows::row_end(&doc.text, pr).0)));
                        r = pr;
                    }
                    None => break,
                }
            }
            out
        });
        let mut y = -(view.top_sub as f32) * lh;
        for (i, &r) in starts.iter().enumerate() {
            let lay = layout(ui, doc, r, cx, wrap_w, &spans[i], true);
            let h = lay.lines() as f32 * lh;
            shown.push((lay, y));
            y += h;
        }
        // don't leave the screen mostly empty after scrolling past the end
        if shown.last().is_some_and(|(l, _)| l.next.is_none()) && y < rect.height() * 0.4 && view.top > 0 && view.drag.is_none() {
            let gap = ((rect.height() * 0.4 - y) / lh).ceil() as i64;
            scroll_lines(ui, doc, view, cx, wrap_w, -gap);
            ui.ctx().request_repaint();
        }
    }
    for (l, _) in &shown {
        view.widest = view.widest.max(l.galley.size().x);
    }

    // ---- mouse: select, drag, gutter, scrollbar
    let painter = ui.painter_at(rect);
    let pointer = ui.input(|i| i.pointer.interact_pos());
    let scroll_x = view.scroll_x;
    let to_byte = |pos: Pos2, shown: &[(RowLay, f32)]| -> u64 {
        let rel_y = pos.y - rect.top();
        let gx = pos.x - text_rect.left() - pad_x + scroll_x;
        if shown.is_empty() {
            return 0;
        }
        if rel_y < shown[0].1 {
            return shown[0].0.start;
        }
        for (l, y) in shown {
            let h = l.lines() as f32 * lh;
            if rel_y >= *y && rel_y < y + h {
                let line = ((rel_y - y) / lh).floor() as usize;
                return l.byte_of(l.char_at(line, gx));
            }
        }
        let (l, _) = shown.last().unwrap();
        l.text_end
    };
    let bar = Rect::from_min_max(pos2(rect.right() - bar_w, rect.top()), rect.max);
    let in_bar = pointer.is_some_and(|p| bar.contains(p));
    let in_gutter = pointer.is_some_and(|p| p.x < text_rect.left() && rect.contains(p));
    if response.hovered() {
        ui.ctx().set_cursor_icon(if in_bar || in_gutter { CursorIcon::Default } else { CursorIcon::Text });
    }

    // scrollbar geometry
    let (thumb_frac, thumb_len) = scroll_position(doc, view, &shown, rect.height(), lh);
    let thumb_h = (thumb_len * bar.height()).clamp(28.0, bar.height());
    let thumb_y = bar.top() + thumb_frac * (bar.height() - thumb_h);
    let thumb = Rect::from_min_size(pos2(bar.left() + 2.0, thumb_y), vec2(bar_w - 4.0, thumb_h));

    if response.drag_started() || (response.clicked() && view.drag.is_none()) || response.double_clicked() || response.triple_clicked() {
        if let Some(pos) = pointer {
            if in_bar {
                if thumb.contains(pos) {
                    view.drag = Some(Drag::Thumb { grab: pos.y - thumb.top() });
                } else {
                    // jump there
                    let f = ((pos.y - bar.top() - thumb_h / 2.0) / (bar.height() - thumb_h)).clamp(0.0, 1.0);
                    jump_to_fraction(doc, view, f);
                    view.drag = Some(Drag::Thumb { grab: thumb_h / 2.0 });
                }
            } else if in_gutter && pos.x < rect.left() + 16.0 {
                let at = to_byte(pos, &shown);
                let keep = doc.sel;
                doc.sel = Sel::caret(at);
                doc.toggle_bookmark();
                doc.sel = keep;
            } else {
                let at = to_byte(pos, &shown);
                let shift = ui.input(|i| i.modifiers.shift);
                let (gran, origin) = if response.triple_clicked() || (in_gutter && !response.double_clicked()) {
                    let s = doc.line_start_of(at);
                    let e = doc.line_span_of(at).end;
                    (Gran::Line, s..e)
                } else if response.double_clicked() {
                    (Gran::Word, doc.word_at(at))
                } else {
                    (Gran::Char, at..at)
                };
                if shift && gran == Gran::Char {
                    doc.sel.head = at;
                } else {
                    doc.sel = Sel { anchor: origin.start, head: origin.end };
                }
                view.drag = Some(Drag::Select { gran, origin });
                view.want_x = None;
                moved = true;
                view.blink = ui.input(|i| i.time);
            }
        }
    }
    if response.dragged() || ui.input(|i| i.pointer.primary_down()) && view.drag.is_some() {
        if let (Some(pos), Some(drag)) = (pointer, view.drag.clone()) {
            match drag {
                Drag::Thumb { grab } => {
                    let f = ((pos.y - grab - bar.top()) / (bar.height() - thumb_h).max(1.0)).clamp(0.0, 1.0);
                    jump_to_fraction(doc, view, f);
                }
                Drag::Select { gran, origin } => {
                    // past the edges: scroll
                    if pos.y < rect.top() {
                        scroll_lines(ui, doc, view, cx, wrap_w, -(((rect.top() - pos.y) / lh).ceil() as i64).min(8));
                        ui.ctx().request_repaint();
                    } else if pos.y > rect.bottom() {
                        scroll_lines(ui, doc, view, cx, wrap_w, (((pos.y - rect.bottom()) / lh).ceil() as i64).min(8));
                        ui.ctx().request_repaint();
                    }
                    let at = to_byte(pos.clamp(rect.min, rect.max - vec2(0.0, 1.0)), &shown);
                    let at = if pos.y > rect.bottom() { shown.last().map(|(l, _)| l.text_end.max(at)).unwrap_or(at) } else { at };
                    let ext = match gran {
                        Gran::Char => at..at,
                        Gran::Word => doc.word_at(at),
                        Gran::Line => doc.line_start_of(at)..doc.line_span_of(at).end,
                    };
                    let sel = if ext.start < origin.start { Sel { anchor: origin.end, head: ext.start } } else { Sel { anchor: origin.start, head: ext.end.max(origin.end) } };
                    if sel != doc.sel {
                        doc.sel = sel;
                        moved = true;
                    }
                    if pos.x > text_rect.right() - 4.0 && !s.wrap {
                        view.scroll_x += 12.0;
                    } else if pos.x < text_rect.left() + 4.0 && view.scroll_x > 0.0 {
                        view.scroll_x = (view.scroll_x - 12.0).max(0.0);
                    }
                }
            }
        }
    }
    if !ui.input(|i| i.pointer.primary_down()) {
        view.drag = None;
    }

    // ---- paint
    painter.rect_filled(rect, CornerRadius::ZERO, p.editor);
    let gx0 = text_rect.left() + pad_x - view.scroll_x;
    let text_clip = Rect::from_min_max(pos2(text_rect.left(), rect.top()), pos2(text_rect.right(), rect.bottom()));
    let tp = painter.with_clip_rect(text_clip);
    let sel = doc.sel.range();
    let caret = doc.sel.head;

    // which shown row holds the caret
    let caret_row = shown.iter().position(|(l, _)| caret >= l.start && (caret < l.next.unwrap_or(u64::MAX) || l.next.is_none()) && caret <= l.next.unwrap_or(u64::MAX));
    let caret_row = caret_row.filter(|&i| {
        let l = &shown[i].0;
        caret <= l.text_end || l.next.is_none() || caret < l.next.unwrap()
    });

    // current line
    if s.highlight_line && doc.sel.is_empty()
        && let Some(i) = caret_row
    {
        let (l, y) = &shown[i];
        let (line, _) = l.locate(l.char_of(caret));
        let yy = rect.top() + y + l.line_y(line);
        painter.rect_filled(Rect::from_min_size(pos2(rect.left(), yy), vec2(rect.width() - bar_w, lh)), CornerRadius::ZERO, p.current_line);
    }

    let paint_range = |l: &RowLay, y: f32, a: u64, b: u64, color: Color32, past_end: bool, radius: u8| {
        let (ca, cb) = (l.char_of(a), l.char_of(b));
        let (la, xa) = l.locate(ca);
        let (lb, xb) = l.locate(cb);
        for line in la..=lb {
            let r = &l.galley.rows[line.min(l.galley.rows.len() - 1)];
            let x0 = if line == la { xa } else { r.pos.x };
            let mut x1 = if line == lb { xb } else { r.pos.x + r.row.size.x };
            if line == lb && past_end {
                x1 += char_w * 0.6;
            }
            if x1 <= x0 && !(past_end && line == lb) {
                continue;
            }
            let yy = rect.top() + y + r.pos.y;
            tp.rect_filled(Rect::from_min_max(pos2(gx0 + x0, yy), pos2(gx0 + x1.max(x0 + 2.0), yy + lh)), CornerRadius::same(radius), color);
        }
    };

    // search matches and the selected word, in what's visible
    let word: Option<Vec<u8>> = if s.smart_highlight && !doc.sel.is_empty() && sel.end - sel.start <= 200 {
        let w = doc.text.read(sel.clone());
        (w.iter().all(|b| b.is_ascii_alphanumeric() || *b == b'_' || *b >= 0x80) && doc.word_at(sel.start) == sel).then_some(w)
    } else {
        None
    };
    for (l, y) in &shown {
        if let Some(re) = cx.find {
            for m in re.find_iter(&l.bytes) {
                if m.is_empty() {
                    continue;
                }
                let (a, b) = (l.start + m.start() as u64, l.start + m.end() as u64);
                let current = a == sel.start && b == sel.end;
                paint_range(l, *y, a, b, if current { p.match_current } else { p.match_bg }, false, 3);
            }
        }
        if let Some(w) = &word {
            for i in memchr::memmem::find_iter(&l.bytes, w) {
                let a = l.start + i as u64;
                let b = a + w.len() as u64;
                let before_ok = i == 0 || !is_word(l.bytes[i - 1]);
                let after_ok = i + w.len() >= l.bytes.len() || !is_word(l.bytes[i + w.len()]);
                if before_ok && after_ok && a != sel.start {
                    paint_range(l, *y, a, b, p.word_bg, false, 3);
                }
            }
        }
    }

    // selection
    if !sel.is_empty() {
        let color = if focused { p.selection } else { p.selection_dim };
        for (l, y) in &shown {
            let row_end = l.next.unwrap_or(l.text_end + 1);
            if sel.end <= l.start || sel.start >= row_end {
                continue;
            }
            let a = sel.start.max(l.start);
            let b = sel.end.min(l.text_end);
            let past = sel.end > l.text_end && l.next.is_some();
            paint_range(l, *y, a, b.max(a), color, past, 2);
        }
    }

    // matching bracket
    if doc.sel.is_empty()
        && let Some((a, b)) = matching_bracket(doc, caret)
    {
        for (l, y) in &shown {
            for x in [a, b] {
                if x >= l.start && x < l.text_end {
                    let (line, x0) = l.locate(l.char_of(x));
                    let (_, x1) = l.locate(l.char_of(x + 1));
                    let yy = rect.top() + y + l.line_y(line);
                    tp.rect_stroke(Rect::from_min_max(pos2(gx0 + x0, yy + 1.0), pos2(gx0 + x1, yy + lh - 1.0)), CornerRadius::same(2), Stroke::new(1.0, p.text3), egui::StrokeKind::Inside);
                }
            }
        }
    }

    // text
    for (l, y) in &shown {
        tp.galley(pos2(gx0, rect.top() + y), l.galley.clone(), p.text);
    }

    // caret
    view.caret_rect = None;
    if let Some(i) = caret_row {
        let (l, y) = &shown[i];
        let (line, x) = l.locate(l.char_of(caret));
        let yy = rect.top() + y + l.line_y(line);
        let r = Rect::from_min_size(pos2(gx0 + x, yy), vec2(2.0, lh));
        view.caret_rect = Some(r);
        let now = ui.input(|i| i.time);
        let on = !s.caret_blink || !focused || ((now - view.blink) % 1.06) < 0.62;
        if focused && on {
            if view.overwrite {
                let (_, x2) = l.locate(l.char_of(doc.next_char(caret)));
                let w = (x2 - x).max(char_w);
                tp.rect_filled(Rect::from_min_size(pos2(gx0 + x, yy + lh - 3.0), vec2(w, 2.0)), CornerRadius::ZERO, p.caret);
            } else {
                tp.rect_filled(Rect::from_min_size(pos2(gx0 + x - 0.5, yy + 2.0), vec2(2.0, lh - 4.0)), CornerRadius::same(1), p.caret);
            }
        }
        if focused && s.caret_blink {
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(530));
        }
        if focused {
            ui.ctx().output_mut(|o| {
                o.ime = Some(egui::output::IMEOutput { purpose: Default::default(), rect, cursor_rect: r, should_interrupt_composition: false });
            });
        }
    }

    // gutter
    let gutter_rect = Rect::from_min_max(rect.min, pos2(text_rect.left(), rect.bottom()));
    painter.rect_filled(gutter_rect, CornerRadius::ZERO, p.editor);
    if s.line_numbers {
        let mut line_no = doc.text.line_of(view.top);
        let caret_line_start = doc.line_start_of(caret);
        let font = mono((s.font_size - 1.0).max(8.0));
        for (k, (l, y)) in shown.iter().enumerate() {
            let starts = rows::starts_line(&doc.text, l.start);
            if k > 0 && starts {
                line_no = line_no.map(|n| n + 1);
            }
            let yy = rect.top() + y;
            if yy > rect.bottom() {
                break;
            }
            let is_caret_line = l.start <= caret && doc.line_start_of(l.start) == caret_line_start;
            let color = if is_caret_line { p.text2 } else { p.text4 };
            let label = if starts { line_no.map(|n| (n + 1).to_string()).unwrap_or_default() } else { "·".into() };
            painter.text(pos2(text_rect.left() - 12.0, yy + lh / 2.0), egui::Align2::RIGHT_CENTER, label, font.clone(), color);
        }
    }
    for (l, y) in &shown {
        if doc.bookmarks.binary_search(&l.start).is_ok() {
            let yy = rect.top() + y + lh / 2.0;
            painter.rect_filled(Rect::from_center_size(pos2(rect.left() + 8.0, yy), vec2(6.0, lh * 0.62)), CornerRadius::same(2), p.accent);
        }
    }

    // scrollbar: bookmarks as ticks, then the thumb
    painter.rect_filled(bar, CornerRadius::ZERO, p.editor);
    if doc.len() > 0 {
        for &b in doc.bookmarks.iter().take(2000) {
            let f = mark_fraction(doc, b);
            let yy = bar.top() + f * bar.height();
            painter.rect_filled(Rect::from_min_size(pos2(bar.left() + 2.0, yy - 1.0), vec2(bar_w - 4.0, 2.0)), CornerRadius::ZERO, p.accent);
        }
    }
    let content_taller = view.top > 0 || shown.last().is_none_or(|(l, y)| l.next.is_some() || y + l.lines() as f32 * lh > rect.height());
    if content_taller {
        let hot = in_bar || matches!(view.drag, Some(Drag::Thumb { .. }));
        let c = if hot { p.text3 } else { p.line_strong };
        painter.rect_filled(thumb, CornerRadius::same(4), c);
    }
    if !s.wrap && view.widest > wrap_w {
        // a thin horizontal position hint
        let f = view.scroll_x / (view.widest - wrap_w * 0.5).max(1.0);
        let w = (wrap_w / view.widest * text_rect.width()).max(30.0);
        let x = text_rect.left() + f.clamp(0.0, 1.0) * (text_rect.width() - w);
        painter.rect_filled(Rect::from_min_size(pos2(x, rect.bottom() - 3.0), vec2(w, 2.0)), CornerRadius::same(1), p.line_strong);
    }

    Output { response, edited, moved }
}

fn is_word(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b >= 0x80
}

/// Where the view is in the document (0..1) and how much of it shows, for the scrollbar. By lines
/// once they're counted, by bytes before that.
fn scroll_position(doc: &Document, view: &View, shown: &[(RowLay, f32)], height: f32, lh: f32) -> (f32, f32) {
    let len = doc.len().max(1);
    let visible_bytes = shown.last().map(|(l, _)| l.next.unwrap_or(doc.len()) - view.top).unwrap_or(0);
    if let (Some(total), Some(at)) = (doc.text.line_count(), doc.text.line_of(view.top)) {
        let page = (height / lh) as f64;
        let total = total as f64;
        let max_top = (total - page * 0.6).max(1.0);
        return (((at as f64) / max_top).clamp(0.0, 1.0) as f32, (page / total).min(1.0) as f32);
    }
    let max_top = (len.saturating_sub(visible_bytes / 2)).max(1);
    ((view.top as f64 / max_top as f64).clamp(0.0, 1.0) as f32, (visible_bytes as f64 / len as f64).min(1.0) as f32)
}

fn mark_fraction(doc: &Document, at: u64) -> f32 {
    match (doc.text.line_of(at), doc.text.line_count()) {
        (Some(l), Some(n)) => l as f32 / n.max(1) as f32,
        _ => at as f32 / doc.len().max(1) as f32,
    }
}

fn jump_to_fraction(doc: &Document, view: &mut View, f: f32) {
    let target = match doc.text.line_count() {
        Some(n) => {
            let line = ((n.saturating_sub(view.page as u64 * 6 / 10)) as f64 * f as f64) as u64;
            doc.text.line_start(line).unwrap_or(0)
        }
        None => (doc.len() as f64 * f as f64) as u64,
    };
    view.top = rows::row_start(&doc.text, target);
    view.top_sub = 0;
}

/// Moves the view by `n` visual lines.
fn scroll_lines(ui: &Ui, doc: &Document, view: &mut View, cx: &Ctx, wrap_w: f32, n: i64) {
    let wrap = cx.settings.wrap;
    let lines_of = |r: u64| if wrap { layout(ui, doc, r, cx, wrap_w, &[], false).lines() } else { 1 };
    if n > 0 {
        for _ in 0..n {
            if view.top_sub + 1 < lines_of(view.top) {
                view.top_sub += 1;
            } else if let Some(next) = rows::next_row(&doc.text, view.top) {
                // keep at least a line on screen
                view.top = next;
                view.top_sub = 0;
            } else {
                break;
            }
        }
    } else {
        for _ in 0..(-n) {
            if view.top_sub > 0 {
                view.top_sub -= 1;
            } else if let Some(prev) = rows::prev_row(&doc.text, view.top) {
                view.top = prev;
                view.top_sub = lines_of(prev) - 1;
            } else {
                break;
            }
        }
    }
}

/// The visual line the caret is on: (row start, line within the row, x).
fn caret_spot(ui: &Ui, doc: &Document, cx: &Ctx, wrap_w: f32, at: u64) -> (u64, usize, f32) {
    let r = rows::row_start(&doc.text, at);
    let l = layout(ui, doc, r, cx, wrap_w, &[], false);
    // the caret at a row's very end (no newline: a long-line cut) shows at the start of the next
    let (line, x) = l.locate(l.char_of(at));
    (r, line, x)
}

fn reveal_caret(ui: &Ui, doc: &Document, view: &mut View, cx: &Ctx, wrap_w: f32, center: bool) {
    let (row, line, x) = caret_spot(ui, doc, cx, wrap_w, doc.sel.head);
    // horizontal
    if !cx.settings.wrap {
        let margin = 40.0;
        if x < view.scroll_x + margin * 0.5 {
            view.scroll_x = (x - margin).max(0.0);
        } else if x > view.scroll_x + wrap_w - margin {
            view.scroll_x = x - wrap_w + margin;
        }
    }
    let page = view.page.max(1);
    if center {
        view.top = row;
        view.top_sub = line;
        scroll_lines(ui, doc, view, cx, wrap_w, -((page / 2) as i64));
        return;
    }
    // above the view?
    if row < view.top || (row == view.top && line < view.top_sub) {
        view.top = row;
        view.top_sub = line;
        return;
    }
    // within the first page? walk down from the top
    let mut r = view.top;
    let mut sub = view.top_sub;
    let wrap = cx.settings.wrap;
    let lines_of = |r: u64| if wrap { layout(ui, doc, r, cx, wrap_w, &[], false).lines() } else { 1 };
    for _ in 0..page.saturating_sub(1) {
        if r == row && sub == line {
            return;
        }
        if sub + 1 < lines_of(r) {
            sub += 1;
        } else if let Some(n) = rows::next_row(&doc.text, r) {
            r = n;
            sub = 0;
        } else {
            return;
        }
    }
    if r == row && sub == line {
        return;
    }
    // below: put the caret's line at the bottom
    view.top = row;
    view.top_sub = line;
    scroll_lines(ui, doc, view, cx, wrap_w, -(page.saturating_sub(2) as i64));
}

/// Moves the caret by visual lines, keeping its x.
fn vertical(ui: &Ui, doc: &Document, view: &mut View, cx: &Ctx, wrap_w: f32, at: u64, n: i64) -> u64 {
    let (mut row, mut line, x0) = caret_spot(ui, doc, cx, wrap_w, at);
    let x = *view.want_x.get_or_insert(x0);
    let wrap = cx.settings.wrap;
    let lines_of = |r: u64| if wrap { layout(ui, doc, r, cx, wrap_w, &[], false).lines() } else { 1 };
    for _ in 0..n.unsigned_abs() {
        if n > 0 {
            if line + 1 < lines_of(row) {
                line += 1;
            } else if let Some(nr) = rows::next_row(&doc.text, row) {
                row = nr;
                line = 0;
            } else {
                return doc.len();
            }
        } else if line > 0 {
            line -= 1;
        } else if let Some(pr) = rows::prev_row(&doc.text, row) {
            row = pr;
            line = lines_of(pr) - 1;
        } else {
            return 0;
        }
    }
    let l = layout(ui, doc, row, cx, wrap_w, &[], false);
    l.byte_of(l.char_at(line, x))
}

fn matching_bracket(doc: &Document, at: u64) -> Option<(u64, u64)> {
    const LIMIT: u64 = 256 << 10;
    let pair = |b: u8| match b {
        b'(' => Some((b')', true)),
        b'[' => Some((b']', true)),
        b'{' => Some((b'}', true)),
        b')' => Some((b'(', false)),
        b']' => Some((b'[', false)),
        b'}' => Some((b'{', false)),
        _ => None,
    };
    let (pos, open, close, fwd) = [at, at.wrapping_sub(1)].into_iter().find_map(|p| {
        let b = doc.text.byte(p)?;
        let (other, fwd) = pair(b)?;
        Some((p, b, other, fwd))
    })?;
    let mut depth = 0i32;
    if fwd {
        let end = (pos + LIMIT).min(doc.len());
        let bytes = doc.text.read(pos..end);
        for (i, &b) in bytes.iter().enumerate() {
            if b == open {
                depth += 1;
            } else if b == close {
                depth -= 1;
                if depth == 0 {
                    return Some((pos, pos + i as u64));
                }
            }
        }
    } else {
        let start = pos.saturating_sub(LIMIT);
        let bytes = doc.text.read(start..pos + 1);
        for (i, &b) in bytes.iter().enumerate().rev() {
            if b == open {
                depth += 1;
            } else if b == close {
                depth -= 1;
                if depth == 0 {
                    return Some((start + i as u64, pos));
                }
            }
        }
    }
    None
}

fn handle_event(ui: &Ui, doc: &mut Document, view: &mut View, s: &EditorSettings, ev: &Event, wrap_w: f32, cx: &Ctx) {
    match ev {
        Event::Text(t) => {
            if t.is_empty() || t.chars().all(|c| c.is_control()) {
                return;
            }
            view.want_x = None;
            if view.overwrite {
                doc.overwrite(t.as_bytes());
            } else {
                doc.insert(t.as_bytes(), Kind::Typing);
            }
        }
        Event::Ime(egui::ImeEvent::Commit(t)) if !t.is_empty() => {
            doc.insert(t.as_bytes(), Kind::Typing);
        }
        Event::Paste(t) => {
            view.want_x = None;
            let fixed = normalize_eol(t, doc.eol.as_bytes());
            doc.insert(&fixed, Kind::Other);
        }
        Event::Copy | Event::Cut => {
            let cut = matches!(ev, Event::Cut);
            if doc.sel.is_empty() {
                // nothing selected: the whole line
                let span = doc.line_span();
                if span.end - span.start <= OP_MAX {
                    let text = String::from_utf8_lossy(&doc.text.read(span.clone())).into_owned();
                    ui.ctx().copy_text(text);
                    if cut {
                        doc.edit(vec![(span.clone(), vec![])], Kind::Other, |_| Sel::caret(span.start));
                    }
                }
            } else if let Some(b) = doc.selected_bytes() {
                ui.ctx().copy_text(String::from_utf8_lossy(&b).into_owned());
                if cut {
                    doc.delete_selection();
                }
            }
        }
        Event::Key { key, pressed: true, modifiers, .. } => key_press(ui, doc, view, s, *key, *modifiers, wrap_w, cx),
        _ => {}
    }
}

/// Line breaks in pasted text made to match the document's.
pub fn normalize_eol(t: &str, eol: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(t.len());
    let b = t.as_bytes();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'\r' => {
                out.extend_from_slice(eol);
                if b.get(i + 1) == Some(&b'\n') {
                    i += 1;
                }
            }
            b'\n' => out.extend_from_slice(eol),
            c => out.push(c),
        }
        i += 1;
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn key_press(ui: &Ui, doc: &mut Document, view: &mut View, s: &EditorSettings, key: Key, m: Modifiers, wrap_w: f32, cx: &Ctx) {
    let word = m.command || m.ctrl;
    let go = |doc: &mut Document, view: &mut View, to: u64, keep_x: bool| {
        if !keep_x {
            view.want_x = None;
        }
        if m.shift {
            doc.sel.head = to;
        } else {
            doc.sel = Sel::caret(to);
        }
    };
    let sel = doc.sel;
    match key {
        Key::ArrowLeft => {
            let to = if !sel.is_empty() && !m.shift { sel.range().start } else if word { doc.word_left(sel.head) } else { doc.prev_char(sel.head) };
            go(doc, view, to, false);
        }
        Key::ArrowRight => {
            let to = if !sel.is_empty() && !m.shift { sel.range().end } else if word { doc.word_right(sel.head) } else { doc.next_char(sel.head) };
            go(doc, view, to, false);
        }
        Key::ArrowUp if !m.alt && !(word && m.shift) => {
            if word {
                scroll_lines(ui, doc, view, cx, wrap_w, -1);
                return;
            }
            let to = vertical(ui, doc, view, cx, wrap_w, sel.head, -1);
            go(doc, view, to, true);
        }
        Key::ArrowDown if !m.alt && !(word && m.shift) => {
            if word {
                scroll_lines(ui, doc, view, cx, wrap_w, 1);
                return;
            }
            let to = vertical(ui, doc, view, cx, wrap_w, sel.head, 1);
            go(doc, view, to, true);
        }
        Key::PageUp | Key::PageDown => {
            let n = view.page.saturating_sub(2).max(1) as i64;
            let n = if key == Key::PageUp { -n } else { n };
            let to = vertical(ui, doc, view, cx, wrap_w, sel.head, n);
            scroll_lines(ui, doc, view, cx, wrap_w, n);
            go(doc, view, to, true);
        }
        Key::Home => {
            let to = if word {
                0
            } else {
                // first non-blank, then the line start
                let ls = doc.line_start_of(sel.head);
                let indent = doc.indent_of(sel.head).len() as u64;
                if sel.head == ls + indent { ls } else { ls + indent }
            };
            go(doc, view, to, false);
        }
        Key::End => {
            let to = if word { doc.len() } else { doc.line_end_of(sel.head) };
            go(doc, view, to, false);
        }
        Key::Backspace => {
            view.want_x = None;
            if word {
                doc.delete_word(false);
            } else {
                doc.backspace(s.insert_spaces.then_some(s.tab_width));
            }
        }
        Key::Delete => {
            view.want_x = None;
            if m.shift && sel.is_empty() {
                doc.delete_lines();
            } else if word {
                doc.delete_word(true);
            } else {
                doc.delete_forward();
            }
        }
        Key::Enter if !word => {
            view.want_x = None;
            doc.newline(s.auto_indent);
        }
        Key::Tab if !word => {
            view.want_x = None;
            let unit: Vec<u8> = if s.insert_spaces { vec![b' '; s.tab_width] } else { vec![b'\t'] };
            let multi = !sel.is_empty() && doc.text.find_byte(b'\n', sel.range()).is_some();
            if m.shift || multi {
                doc.indent(m.shift, &unit, s.tab_width);
            } else if s.insert_spaces {
                let col = (sel.range().start - doc.line_start_of(sel.range().start)) as usize;
                let n = s.tab_width - col % s.tab_width.max(1);
                doc.insert(&vec![b' '; n], Kind::Typing);
            } else {
                doc.insert(b"\t", Kind::Typing);
            }
        }
        Key::Insert if !m.shift && !word => view.overwrite = !view.overwrite,
        Key::Escape => {
            if !sel.is_empty() {
                doc.sel = Sel::caret(sel.head);
            }
        }
        _ => {}
    }
}

trait LineSpanOf {
    fn line_span_of(&self, at: u64) -> Range<u64>;
}

impl LineSpanOf for Document {
    /// The line holding `at`, with its line break.
    fn line_span_of(&self, at: u64) -> Range<u64> {
        let s = self.line_start_of(at);
        let e = self.line_end_of(at);
        let e = match self.text.byte(e) {
            Some(b'\r') if self.text.byte(e + 1) == Some(b'\n') => e + 2,
            Some(b'\n') => e + 1,
            _ => e,
        };
        s..e
    }
}
