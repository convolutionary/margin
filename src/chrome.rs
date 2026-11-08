//! Everything around the text: menus, tabs, the find bar, results, the status bar and dialogs.

use std::path::PathBuf;
use std::sync::atomic::Ordering;

use egui::text::{LayoutJob, TextFormat};
use egui::{Align, Align2, Color32, CornerRadius, FontId, Frame, Id, Key, Layout, Margin, Panel, Rect, RichText, Sense, Stroke, TextEdit, Ui, pos2, vec2};
use margin_core::file::{Encoding, Eol};

use crate::app::{App, Ask, Cmd, DiskNote, FifDialog, Tone, shortcut_label};
use crate::editor::{self, Reveal};
use crate::syntax::Lang;
use crate::theme::{icon, icon_font, medium, semibold, ui_font};

fn human(n: u64) -> String {
    const U: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1000.0 && i < U.len() - 1 {
        v /= 1000.0;
        i += 1;
    }
    if i == 0 { format!("{n} B") } else if v < 10.0 { format!("{v:.2} {}", U[i]) } else if v < 100.0 { format!("{v:.1} {}", U[i]) } else { format!("{v:.0} {}", U[i]) }
}

fn thousands(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// A small square button with a Lucide icon.
fn icon_button(ui: &mut Ui, glyph: &str, tip: &str, on: bool) -> egui::Response {
    let p = ui.visuals().clone();
    let (rect, resp) = ui.allocate_exact_size(vec2(26.0, 24.0), Sense::click());
    let hovered = resp.hovered();
    let bg = if on {
        p.selection.bg_fill
    } else if hovered {
        p.widgets.hovered.weak_bg_fill
    } else {
        Color32::TRANSPARENT
    };
    ui.painter().rect_filled(rect, CornerRadius::same(5), bg);
    let color = if on { p.selection.stroke.color } else if hovered { p.widgets.hovered.fg_stroke.color } else { p.widgets.inactive.fg_stroke.color };
    ui.painter().text(rect.center(), Align2::CENTER_CENTER, glyph, icon_font(14.0), color);
    resp.on_hover_text(tip)
}

impl App {
    pub fn draw(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        let mut cmds: Vec<Cmd> = Vec::new();
        let p = self.palette;

        Panel::top("menu").exact_size(30.0).frame(Frame::new().fill(p.chrome).inner_margin(Margin::symmetric(6, 0))).show(ui, |ui| {
            self.menu_bar(ui, &mut cmds);
        });
        Panel::top("tabs").exact_size(34.0).frame(Frame::new().fill(p.chrome)).show(ui, |ui| {
            self.tab_strip(ui, &mut cmds);
        });
        Panel::bottom("status").exact_size(25.0).frame(Frame::new().fill(p.chrome).inner_margin(Margin::symmetric(10, 0))).show(ui, |ui| {
            self.status_bar(ui, &mut cmds);
        });
        if self.results_open && self.results.is_some() {
            Panel::bottom("results").resizable(true).default_size(230.0).min_size(90.0).frame(Frame::new().fill(p.panel)).show(ui, |ui| {
                self.results_panel(ui);
            });
        }
        if self.find.open {
            Panel::bottom("find").frame(Frame::new().fill(p.panel).inner_margin(Margin { left: 10, right: 10, top: 7, bottom: 7 })).show(ui, |ui| {
                self.find_bar(ui, &mut cmds);
            });
        }
        egui::CentralPanel::default().frame(Frame::new().fill(p.editor)).show(ui, |ui| {
            self.editor_area(ui, &mut cmds);
        });

        self.dialogs(&ctx);
        self.draw_toasts(&ctx);
        for c in cmds {
            self.run(c, &ctx);
        }
        if self.focus_editor && self.goto.is_none() && self.fif.is_none() && !self.settings_open && self.ask.is_none() && !self.find.focus {
            let id = self.tab().editor_id();
            ctx.memory_mut(|m| m.request_focus(id));
            self.focus_editor = false;
        }
    }

    // ------------------------------------------------------------ menus

    fn menu_bar(&mut self, ui: &mut Ui, cmds: &mut Vec<Cmd>) {
        let item = |ui: &mut Ui, label: &str, cmd: Cmd, cmds: &mut Vec<Cmd>| {
            if ui.add(egui::Button::new(label).shortcut_text(shortcut_label(cmd))).clicked() {
                cmds.push(cmd);
                ui.close();
            }
        };
        let check = |ui: &mut Ui, label: &str, on: bool, cmd: Cmd, cmds: &mut Vec<Cmd>| {
            let text = format!("{}  {label}", if on { "✓" } else { "   " });
            if ui.add(egui::Button::new(text).shortcut_text(shortcut_label(cmd))).clicked() {
                cmds.push(cmd);
                ui.close();
            }
        };
        ui.horizontal_centered(|ui| {
            // the mark
            let (r, _) = ui.allocate_exact_size(vec2(22.0, 22.0), Sense::hover());
            paint_mark(ui, r, &self.palette);
            ui.add_space(4.0);
            egui::MenuBar::new().ui(ui, |ui| {
                ui.menu_button("File", |ui| {
                    ui.set_min_width(230.0);
                    item(ui, "New", Cmd::New, cmds);
                    item(ui, "Open…", Cmd::Open, cmds);
                    ui.menu_button("Open recent", |ui| {
                        ui.set_min_width(300.0);
                        if self.settings.recent.is_empty() {
                            ui.label(RichText::new("Nothing yet").color(self.palette.text3));
                        }
                        let recent = self.settings.recent.clone();
                        for r in recent {
                            let name = r.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                            let dir = r.parent().map(|d| d.display().to_string()).unwrap_or_default();
                            if ui.add(egui::Button::new(name).right_text(RichText::new(short_dir(&dir)).color(self.palette.text3))).on_hover_text(r.display().to_string()).clicked() {
                                self.open_path(&r, None);
                                ui.close();
                            }
                        }
                        if !self.settings.recent.is_empty() {
                            ui.separator();
                            if ui.button("Clear list").clicked() {
                                self.settings.recent.clear();
                                ui.close();
                            }
                        }
                    });
                    item(ui, "Reopen closed tab", Cmd::Reopen, cmds);
                    ui.separator();
                    item(ui, "Save", Cmd::Save, cmds);
                    item(ui, "Save as…", Cmd::SaveAs, cmds);
                    item(ui, "Save all", Cmd::SaveAll, cmds);
                    item(ui, "Reload from disk", Cmd::Reload, cmds);
                    ui.separator();
                    item(ui, "Close tab", Cmd::Close, cmds);
                    item(ui, "Close other tabs", Cmd::CloseOthers, cmds);
                    item(ui, "Close all", Cmd::CloseAll, cmds);
                    ui.separator();
                    if ui.button("Quit").clicked() {
                        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                        ui.close();
                    }
                });
                ui.menu_button("Edit", |ui| {
                    ui.set_min_width(250.0);
                    item(ui, "Undo", Cmd::Undo, cmds);
                    item(ui, "Redo", Cmd::Redo, cmds);
                    ui.separator();
                    item(ui, "Cut", Cmd::Cut, cmds);
                    item(ui, "Copy", Cmd::Copy, cmds);
                    item(ui, "Paste", Cmd::Paste, cmds);
                    item(ui, "Select all", Cmd::SelectAll, cmds);
                    ui.separator();
                    item(ui, "Duplicate line", Cmd::Duplicate, cmds);
                    item(ui, "Delete line", Cmd::DeleteLine, cmds);
                    item(ui, "Move line up", Cmd::MoveUp, cmds);
                    item(ui, "Move line down", Cmd::MoveDown, cmds);
                    item(ui, "Join lines", Cmd::Join, cmds);
                    item(ui, "Toggle comment", Cmd::Comment, cmds);
                    ui.separator();
                    item(ui, "UPPERCASE", Cmd::Upper, cmds);
                    item(ui, "lowercase", Cmd::Lower, cmds);
                    ui.menu_button("Line operations", |ui| {
                        ui.set_min_width(230.0);
                        item(ui, "Sort lines A → Z", Cmd::SortAsc, cmds);
                        item(ui, "Sort lines Z → A", Cmd::SortDesc, cmds);
                        item(ui, "Remove duplicate lines", Cmd::Dedupe, cmds);
                        item(ui, "Trim trailing whitespace", Cmd::TrimTrailing, cmds);
                        ui.separator();
                        item(ui, "Tabs to spaces", Cmd::TabsToSpaces, cmds);
                        item(ui, "Leading spaces to tabs", Cmd::SpacesToTabs, cmds);
                    });
                });
                ui.menu_button("Search", |ui| {
                    ui.set_min_width(250.0);
                    item(ui, "Find…", Cmd::Find, cmds);
                    item(ui, "Replace…", Cmd::Replace, cmds);
                    item(ui, "Find next", Cmd::FindNext, cmds);
                    item(ui, "Find previous", Cmd::FindPrev, cmds);
                    item(ui, "Find selection", Cmd::SelectionToFind, cmds);
                    item(ui, "Find all in this file", Cmd::FindAll, cmds);
                    item(ui, "Find in files…", Cmd::FindInFiles, cmds);
                    ui.separator();
                    item(ui, "Go to line…", Cmd::GotoLine, cmds);
                    ui.separator();
                    item(ui, "Toggle bookmark", Cmd::Bookmark, cmds);
                    item(ui, "Next bookmark", Cmd::NextBookmark, cmds);
                    item(ui, "Previous bookmark", Cmd::PrevBookmark, cmds);
                    item(ui, "Clear bookmarks", Cmd::ClearBookmarks, cmds);
                });
                ui.menu_button("View", |ui| {
                    ui.set_min_width(250.0);
                    let e = self.settings.editor.clone();
                    check(ui, "Word wrap", e.wrap, Cmd::Wrap, cmds);
                    check(ui, "Show whitespace", e.show_whitespace, Cmd::Whitespace, cmds);
                    check(ui, "Line numbers", e.line_numbers, Cmd::LineNumbers, cmds);
                    let hl = format!("{}  Highlight current line", if e.highlight_line { "✓" } else { "   " });
                    if ui.button(hl).clicked() {
                        self.settings.editor.highlight_line = !e.highlight_line;
                        ui.close();
                    }
                    ui.separator();
                    item(ui, "Bigger text", Cmd::ZoomIn, cmds);
                    item(ui, "Smaller text", Cmd::ZoomOut, cmds);
                    item(ui, "Reset text size", Cmd::ZoomReset, cmds);
                    ui.separator();
                    check(ui, "Follow the file (tail -f)", self.tab().follow, Cmd::Follow, cmds);
                    check(ui, "Results panel", self.results_open, Cmd::Results, cmds);
                    ui.separator();
                    ui.menu_button("Theme", |ui| {
                        for (v, label) in [("system", "Follow the system"), ("dark", "Dark"), ("light", "Light")] {
                            let text = format!("{}  {label}", if self.settings.theme == v { "✓" } else { "   " });
                            if ui.button(text).clicked() {
                                self.settings.theme = v.into();
                                self.apply_theme_pref(ui.ctx());
                                ui.close();
                            }
                        }
                    });
                });
                ui.menu_button("Encoding", |ui| {
                    ui.set_min_width(240.0);
                    self.encoding_menu(ui);
                });
                ui.menu_button("Language", |ui| {
                    self.language_menu(ui);
                });
                ui.menu_button("Settings", |ui| {
                    ui.set_min_width(200.0);
                    item(ui, "Preferences…", Cmd::Settings, cmds);
                });
            });
        });
    }

    fn encoding_menu(&mut self, ui: &mut Ui) {
        let (cur, len, has_path) = {
            let t = self.tab();
            (t.doc.encoding, t.doc.len(), t.doc.path.is_some())
        };
        ui.label(RichText::new(format!("Now: {}", cur.name())).color(self.palette.text3).small());
        ui.separator();
        let mut choices = vec![Encoding::Utf8, Encoding::Utf8Bom, Encoding::Utf16Le, Encoding::Utf16Be];
        choices.extend(Encoding::legacy_choices().into_iter().map(Encoding::Legacy));
        ui.menu_button("Save as encoding", |ui| {
            ui.set_min_width(200.0);
            for e in &choices {
                let text = format!("{}  {}", if *e == cur { "✓" } else { "   " }, e.name());
                let ok = !e.converts() || len <= margin_core::file::CONVERT_MAX;
                if ui.add_enabled(ok, egui::Button::new(text)).clicked() {
                    let t = self.tab_mut();
                    t.doc.encoding = *e;
                    t.doc.mark_unsaved();
                    ui.close();
                }
            }
        });
        ui.menu_button("Reopen with encoding", |ui| {
            ui.set_min_width(200.0);
            for e in &choices {
                if ui.add_enabled(has_path, egui::Button::new(e.name())).clicked() {
                    let i = self.active;
                    if self.tabs[i].doc.dirty() {
                        self.toast("Save or undo your changes first", Tone::Info);
                    } else {
                        self.tabs[i].doc.encoding = *e;
                        self.reload(i, false);
                    }
                    ui.close();
                }
            }
        });
        ui.separator();
        ui.menu_button("Line endings", |ui| {
            let cur = self.tab().doc.eol;
            for (e, label) in [(Eol::Lf, "LF  (Linux, macOS)"), (Eol::CrLf, "CRLF  (Windows)"), (Eol::Cr, "CR  (classic Mac)")] {
                let text = format!("{}  {label}", if e == cur { "✓" } else { "   " });
                if ui.button(text).clicked() {
                    let t = self.tab_mut();
                    if !t.doc.convert_eol(e) {
                        self.toast("Too big to convert line endings (256 MB)", Tone::Bad);
                    }
                    ui.close();
                }
            }
        });
    }

    fn language_menu(&mut self, ui: &mut Ui) {
        ui.set_min_width(240.0);
        let id = Id::unique("lang-filter");
        let mut filter: String = ui.data_mut(|d| d.get_temp(id).unwrap_or_default());
        let r = ui.add(TextEdit::singleline(&mut filter).hint_text("Filter languages").desired_width(f32::INFINITY));
        if ui.memory(|m| m.focused().is_none()) {
            r.request_focus();
        }
        ui.data_mut(|d| d.insert_temp(id, filter.clone()));
        let cur = self.tab().lang.clone();
        let mut pick: Option<(Lang, bool)> = None;
        if filter.is_empty() {
            if ui.button(format!("{}  Detect automatically", if !self.tab().lang_fixed { "✓" } else { "   " })).clicked() {
                pick = Some((Lang::Plain, false));
            }
            ui.separator();
        }
        egui::ScrollArea::vertical().max_height(420.0).show(ui, |ui| {
            let f = filter.to_lowercase();
            let mut all = vec![Lang::Plain, Lang::Log];
            all.extend(self.languages.iter().map(|n| Lang::Syntax(n.clone())));
            for l in all {
                if !f.is_empty() && !l.name().to_lowercase().contains(&f) {
                    continue;
                }
                let text = format!("{}  {}", if l == cur { "✓" } else { "   " }, l.name());
                if ui.button(text).clicked() {
                    pick = Some((l, true));
                }
            }
        });
        if let Some((l, fixed)) = pick {
            let t = self.tab_mut();
            if fixed {
                t.lang = l;
            } else {
                let first = String::from_utf8_lossy(&t.doc.text.read(0..t.doc.len().min(256))).lines().next().unwrap_or("").to_string();
                t.lang = crate::syntax::detect(t.doc.path.as_deref(), &first, t.doc.len());
            }
            t.lang_fixed = fixed;
            ui.data_mut(|d| d.insert_temp(id, String::new()));
            ui.close();
        }
    }

    // ------------------------------------------------------------ tabs

    fn tab_strip(&mut self, ui: &mut Ui, cmds: &mut Vec<Cmd>) {
        let p = self.palette;
        let full = ui.max_rect();
        ui.painter().line_segment([pos2(full.left(), full.bottom() - 0.5), pos2(full.right(), full.bottom() - 0.5)], Stroke::new(1.0, p.line));
        let mut close: Option<u64> = None;
        let mut activate: Option<usize> = None;
        let mut swap: Option<(usize, usize)> = None;
        let mut menu_cmds: Vec<(Cmd, usize)> = Vec::new();
        egui::ScrollArea::horizontal().scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                ui.add_space(6.0);
                let mut rects = Vec::new();
                for i in 0..self.tabs.len() {
                    let t = &self.tabs[i];
                    let active = i == self.active;
                    let title = t.title();
                    let font = if active { medium(12.5) } else { ui_font(12.5) };
                    let galley = ui.painter().layout_no_wrap(title.clone(), font, if active { p.text } else { p.text2 });
                    let w = (galley.size().x + 14.0 + 26.0).clamp(90.0, 260.0);
                    let (rect, resp) = ui.allocate_exact_size(vec2(w, 33.0), Sense::click_and_drag());
                    rects.push(rect);
                    let hovered = resp.hovered();
                    let r = Rect::from_min_max(pos2(rect.left(), rect.top() + 4.0), rect.max);
                    if active {
                        ui.painter().rect_filled(r, CornerRadius { nw: 7, ne: 7, sw: 0, se: 0 }, p.editor);
                        ui.painter().rect_stroke(r, CornerRadius { nw: 7, ne: 7, sw: 0, se: 0 }, Stroke::new(1.0, p.line), egui::StrokeKind::Inside);
                        // cover the strip's bottom line under the active tab
                        ui.painter().line_segment([pos2(r.left() + 1.0, r.bottom() - 0.5), pos2(r.right() - 1.0, r.bottom() - 0.5)], Stroke::new(1.5, p.editor));
                    } else if hovered {
                        ui.painter().rect_filled(r.shrink2(vec2(1.0, 2.0)), CornerRadius::same(6), p.raised);
                    }
                    let text_pos = pos2(r.left() + 12.0, r.center().y - galley.size().y / 2.0);
                    let clip = Rect::from_min_max(r.min, pos2(r.right() - 26.0, r.max.y));
                    ui.painter().with_clip_rect(clip).galley(text_pos, galley, p.text);
                    // close button / unsaved dot
                    let cb = Rect::from_center_size(pos2(r.right() - 15.0, r.center().y), vec2(18.0, 18.0));
                    let over_close = ui.input(|i| i.pointer.hover_pos()).is_some_and(|pp| cb.contains(pp));
                    if hovered || active && !t.doc.dirty() {
                        if over_close {
                            ui.painter().rect_filled(cb, CornerRadius::same(4), p.pressed);
                        }
                        ui.painter().text(cb.center(), Align2::CENTER_CENTER, icon::CLOSE, icon_font(12.0), if over_close { p.text } else { p.text3 });
                    } else if t.doc.dirty() {
                        ui.painter().circle_filled(cb.center(), 3.5, if active { p.accent } else { p.text3 });
                    }
                    if t.saving.is_some() {
                        let f = t.saving.as_ref().map(|s| s.progress.fraction()).unwrap_or(0.0);
                        ui.painter().rect_filled(Rect::from_min_size(pos2(r.left(), r.bottom() - 2.0), vec2(r.width() * f, 2.0)), CornerRadius::ZERO, p.accent);
                    }
                    if resp.clicked() {
                        if over_close {
                            close = Some(t.id);
                        } else {
                            activate = Some(i);
                        }
                    }
                    if resp.middle_clicked() {
                        close = Some(t.id);
                    }
                    if resp.dragged()
                        && let Some(pp) = ui.input(|i| i.pointer.hover_pos())
                    {
                        activate = Some(i);
                        if pp.x < rect.left() && i > 0 {
                            swap = Some((i, i - 1));
                        } else if pp.x > rect.right() && i + 1 < self.tabs.len() {
                            swap = Some((i, i + 1));
                        }
                    }
                    let path = t.doc.path.clone();
                    let resp = if let Some(pth) = &path { resp.on_hover_text(pth.display().to_string()) } else { resp };
                    resp.context_menu(|ui| {
                        ui.set_min_width(200.0);
                        if ui.button("Close").clicked() {
                            menu_cmds.push((Cmd::Close, i));
                            ui.close();
                        }
                        if ui.button("Close others").clicked() {
                            menu_cmds.push((Cmd::CloseOthers, i));
                            ui.close();
                        }
                        if ui.button("Close all").clicked() {
                            menu_cmds.push((Cmd::CloseAll, i));
                            ui.close();
                        }
                        ui.separator();
                        if let Some(pth) = &path {
                            if ui.button("Copy path").clicked() {
                                ui.ctx().copy_text(pth.display().to_string());
                                ui.close();
                            }
                            if ui.button("Copy file name").clicked() {
                                ui.ctx().copy_text(pth.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default());
                                ui.close();
                            }
                            if ui.button("Show in folder").clicked() {
                                if let Some(dir) = pth.parent() {
                                    let _ = open_folder(dir);
                                }
                                ui.close();
                            }
                            if ui.button("Reload from disk").clicked() {
                                menu_cmds.push((Cmd::Reload, i));
                                ui.close();
                            }
                        }
                    });
                }
                // new tab
                ui.add_space(4.0);
                let (r, resp) = ui.allocate_exact_size(vec2(28.0, 33.0), Sense::click());
                let rr = Rect::from_center_size(pos2(r.center().x, r.center().y + 2.0), vec2(24.0, 24.0));
                if resp.hovered() {
                    ui.painter().rect_filled(rr, CornerRadius::same(5), p.raised);
                }
                ui.painter().text(rr.center(), Align2::CENTER_CENTER, icon::PLUS, icon_font(14.0), if resp.hovered() { p.text } else { p.text3 });
                if resp.on_hover_text(format!("New tab  {}", shortcut_label(Cmd::New))).clicked() {
                    cmds.push(Cmd::New);
                }
                // double-click the empty strip for a new tab
                let rest = ui.available_rect_before_wrap();
                let er = ui.interact(rest, Id::unique("tab-strip-empty"), Sense::click());
                if er.double_clicked() {
                    cmds.push(Cmd::New);
                }
            });
        });
        if let Some((a, b)) = swap {
            self.tabs.swap(a, b);
            self.active = b;
        } else if let Some(i) = activate
            && i != self.active
        {
            self.active = i;
            self.focus_editor = true;
        } else if activate.is_some() {
            self.focus_editor = true;
        }
        for (c, i) in menu_cmds {
            self.active = i.min(self.tabs.len() - 1);
            cmds.push(c);
        }
        if let Some(id) = close {
            self.close_tabs(vec![id]);
        }
    }

    // ------------------------------------------------------------ the editor

    fn editor_area(&mut self, ui: &mut Ui, cmds: &mut Vec<Cmd>) {
        let p = self.palette;
        // a note when the file changed underneath
        if let Some(note) = self.tab().disk_note {
            Frame::new().fill(p.raised).inner_margin(Margin::symmetric(12, 6)).show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(icon::ALERT).font(icon_font(13.0)).color(p.warn));
                    match note {
                        DiskNote::Changed => {
                            ui.label("This file changed on disk while you were editing it.");
                            if ui.button("Load the disk version").clicked() {
                                let i = self.active;
                                self.reload(i, false);
                            }
                            if ui.button("Keep mine").clicked() {
                                self.tab_mut().disk_note = None;
                            }
                        }
                        DiskNote::Deleted => {
                            ui.label("This file was deleted or moved.");
                            if ui.button("Save it again").clicked() {
                                let i = self.active;
                                self.tab_mut().doc.mark_unsaved();
                                self.save(i, false);
                            }
                            if ui.button("Dismiss").clicked() {
                                self.tab_mut().disk_note = None;
                            }
                        }
                    }
                });
            });
        }
        let re = if self.find.open { self.compiled() } else { None };
        let settings = self.settings.editor.clone();
        let theme = self.syn_theme.clone();
        let t = &mut self.tabs[self.active];
        let id = t.editor_id();
        let cx = editor::Ctx { settings: &settings, palette: &p, theme: &theme, lang: &t.lang, find: re.as_ref() };
        let lang = t.lang.clone();
        let cx = editor::Ctx { lang: &lang, ..cx };
        let out = editor::show(ui, &mut t.doc, &mut t.view, &cx, id);
        if out.edited {
            t.follow = false;
        }
        out.response.context_menu(|ui| {
            ui.set_min_width(220.0);
            for (label, c) in [("Cut", Cmd::Cut), ("Copy", Cmd::Copy), ("Paste", Cmd::Paste), ("Select all", Cmd::SelectAll)] {
                if ui.add(egui::Button::new(label).shortcut_text(shortcut_label(c))).clicked() {
                    cmds.push(c);
                    ui.close();
                }
            }
            ui.separator();
            for (label, c) in [("Toggle comment", Cmd::Comment), ("Duplicate line", Cmd::Duplicate), ("Delete line", Cmd::DeleteLine), ("Find selection", Cmd::SelectionToFind), ("Toggle bookmark", Cmd::Bookmark)] {
                if ui.add(egui::Button::new(label).shortcut_text(shortcut_label(c))).clicked() {
                    cmds.push(c);
                    ui.close();
                }
            }
        });
    }

    // ------------------------------------------------------------ find

    fn find_bar(&mut self, ui: &mut Ui, cmds: &mut Vec<Cmd>) {
        let p = self.palette;
        let r = ui.max_rect();
        ui.painter().line_segment([pos2(r.left() - 10.0, r.top() - 7.5), pos2(r.right() + 10.0, r.top() - 7.5)], Stroke::new(1.0, p.line));
        let field_w = (ui.available_width() * 0.36).clamp(220.0, 520.0);
        let mut changed = false;
        let mut enter: Option<bool> = None;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            ui.label(RichText::new(icon::SEARCH).font(icon_font(14.0)).color(p.text3));
            let err = matches!(self.find.compiled, Some(Err(_))) && !self.find.query.pattern.is_empty();
            let edit = TextEdit::singleline(&mut self.find.query.pattern)
                .hint_text("Find")
                .font(FontId::monospace(13.0))
                .desired_width(field_w)
                .text_color(if err { p.bad } else { p.text })
                .margin(Margin::symmetric(8, 4));
            let resp = ui.add(edit);
            if self.find.focus {
                resp.request_focus();
                if let Some(mut st) = egui::TextEdit::load_state(ui.ctx(), resp.id) {
                    let n = self.find.query.pattern.chars().count();
                    st.cursor.set_char_range(Some(egui::text::CCursorRange::two(egui::text::CCursor::new(0), egui::text::CCursor::new(n))));
                    st.store(ui.ctx(), resp.id);
                }
                self.find.focus = false;
            }
            if resp.changed() {
                changed = true;
            }
            if resp.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                enter = Some(!ui.input(|i| i.modifiers.shift));
                resp.request_focus();
            }
            // history
            if !self.settings.find_history.is_empty() {
                let hist = self.settings.find_history.clone();
                ui.menu_button(RichText::new(icon::CHEVRON_DOWN).font(icon_font(12.0)).color(p.text3), |ui| {
                    ui.set_min_width(240.0);
                    for h in hist {
                        if ui.button(RichText::new(&h).monospace()).clicked() {
                            self.find.query.pattern = h;
                            changed = true;
                            ui.close();
                        }
                    }
                });
            }
            if icon_button(ui, icon::CASE, "Match case", self.find.query.case).clicked() {
                self.find.query.case = !self.find.query.case;
                changed = true;
            }
            if icon_button(ui, icon::WORD, "Whole words", self.find.query.word).clicked() {
                self.find.query.word = !self.find.query.word;
                changed = true;
            }
            if icon_button(ui, icon::REGEX, "Regular expression", self.find.query.regex).clicked() {
                self.find.query.regex = !self.find.query.regex;
                changed = true;
            }
            ui.add_space(6.0);
            // count / error
            let status = match (&self.find.compiled, self.find.count) {
                (Some(Err(e)), _) if !self.find.query.pattern.is_empty() => RichText::new(e.clone()).color(p.bad),
                _ if self.find.query.pattern.is_empty() => RichText::new(""),
                (_, Some((0, _))) => RichText::new("No matches").color(p.bad),
                (_, Some((n, capped))) => RichText::new(format!("{}{} match{}", thousands(n as u64), if capped { "+" } else { "" }, if n == 1 { "" } else { "es" })).color(p.text2),
                _ => RichText::new(""),
            };
            ui.label(status.size(12.0));
            if self.find.count.is_none() && !self.find.query.pattern.is_empty() && self.tab().doc.len() > crate::app::SYNC_MAX && self.find.job.is_none() && ui.small_button("Count").clicked() {
                self.count_now();
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if icon_button(ui, icon::CLOSE, "Close  Esc", false).clicked() {
                    self.find.open = false;
                    self.cancel_job();
                    self.focus_editor = true;
                }
                if icon_button(ui, icon::REPLACE, "Replace", self.find.replace).clicked() {
                    self.find.replace = !self.find.replace;
                }
                ui.add_space(6.0);
                if ui.button("Find all").on_hover_text("List every match below").clicked() {
                    cmds.push(Cmd::FindAll);
                }
                if icon_button(ui, icon::ARROW_DOWN, &format!("Next match  Enter / {}", shortcut_label(Cmd::FindNext)), false).clicked() {
                    cmds.push(Cmd::FindNext);
                }
                if icon_button(ui, icon::ARROW_UP, &format!("Previous match  Shift+Enter / {}", shortcut_label(Cmd::FindPrev)), false).clicked() {
                    cmds.push(Cmd::FindPrev);
                }
            });
        });
        if self.find.replace {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                ui.label(RichText::new(icon::REPLACE).font(icon_font(14.0)).color(p.text3));
                let hint = if self.find.query.regex { "Replace with ($1 for groups, \\n for a line break)" } else { "Replace with" };
                let resp = ui.add(TextEdit::singleline(&mut self.find.replacement).hint_text(hint).font(FontId::monospace(13.0)).desired_width(field_w).margin(Margin::symmetric(8, 4)));
                if resp.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                    self.replace_one();
                    resp.request_focus();
                }
                if ui.button("Replace").clicked() {
                    self.replace_one();
                }
                if ui.button("Replace all").clicked() {
                    self.replace_all();
                }
                if icon_button(ui, icon::SELECTION, "Only in the selection", self.find.in_selection).clicked() {
                    self.find.in_selection = !self.find.in_selection;
                }
            });
        }
        if let Some(job) = &self.find.job
            && self.job_age().is_some_and(|a| a.as_millis() > 250)
        {
            ui.add_space(5.0);
            let f = job.progress.fraction();
            let label = job.label;
            ui.horizontal(|ui| {
                ui.add(egui::ProgressBar::new(f).desired_width(field_w).desired_height(4.0).fill(p.accent));
                ui.label(RichText::new(format!("{label}… {:.0}%", f * 100.0)).size(12.0).color(p.text2));
                if ui.small_button("Stop").clicked() {
                    self.cancel_job();
                }
            });
        }
        if changed {
            self.find_as_you_type();
        }
        if let Some(fwd) = enter {
            self.find_next(fwd);
        }
    }

    // ------------------------------------------------------------ results

    fn results_panel(&mut self, ui: &mut Ui) {
        let p = self.palette;
        let r = ui.max_rect();
        ui.painter().line_segment([pos2(r.left(), r.top() + 0.5), pos2(r.right(), r.top() + 0.5)], Stroke::new(1.0, p.line));
        let mut open: Option<usize> = None;
        let mut close = false;
        let Some(res) = &self.results else { return };
        Frame::new().inner_margin(Margin { left: 12, right: 8, top: 6, bottom: 4 }).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new(&res.title).font(semibold(12.5)));
                let n = res.hits.len() as u64;
                let summary = if res.files > 1 || res.hits.first().is_some_and(|h| h.path.is_some()) {
                    format!("{} hit{} in {} file{}{}", thousands(n), if n == 1 { "" } else { "s" }, res.files, if res.files == 1 { "" } else { "s" }, if res.capped { " (stopped at 200,000)" } else { "" })
                } else {
                    format!("{} hit{}{}", thousands(n), if n == 1 { "" } else { "s" }, if res.capped { " (stopped at 200,000)" } else { "" })
                };
                ui.label(RichText::new(summary).size(12.0).color(p.text3));
                if !res.done {
                    if let Some(pr) = &res.progress {
                        let f = pr.fraction();
                        if res.hits.first().is_some_and(|h| h.path.is_some()) || res.files == 0 {
                            ui.label(RichText::new(format!("searching… {} files", pr.done.load(Ordering::Relaxed))).size(12.0).color(p.text3));
                        } else {
                            ui.add(egui::ProgressBar::new(f).desired_width(120.0).desired_height(4.0).fill(p.accent));
                        }
                        if ui.small_button("Stop").clicked() {
                            pr.cancel.store(true, Ordering::Relaxed);
                        }
                    }
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if icon_button(ui, icon::CLOSE, "Close", false).clicked() {
                        close = true;
                    }
                });
            });
        });
        let row_h = 21.0;
        let font = FontId::monospace(12.0);
        let tabs_lines: Vec<(u64, Option<u64>)> = Vec::new();
        let _ = tabs_lines;
        egui::ScrollArea::vertical().auto_shrink([false, false]).show_rows(ui, row_h, res.hits.len(), |ui, range| {
            for i in range {
                let h = &res.hits[i];
                let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), row_h), Sense::click());
                let selected = res.selected == Some(i);
                if selected {
                    ui.painter().rect_filled(rect, CornerRadius::ZERO, p.accent_soft);
                } else if resp.hovered() {
                    ui.painter().rect_filled(rect, CornerRadius::ZERO, p.raised);
                }
                let mut x = rect.left() + 12.0;
                if let Some(path) = &h.path {
                    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    let g = ui.painter().layout_no_wrap(name, ui_font(12.0), p.text2);
                    let w = g.size().x.min(220.0);
                    ui.painter().with_clip_rect(Rect::from_min_size(pos2(x, rect.top()), vec2(220.0, row_h))).galley(pos2(x, rect.center().y - g.size().y / 2.0), g, p.text2);
                    x += w + 8.0;
                }
                let line = h.line.map(|l| (l + 1).to_string()).unwrap_or_else(|| "·".into());
                let g = ui.painter().layout_no_wrap(line, font.clone(), p.text4);
                ui.painter().galley(pos2(x + 44.0 - g.size().x, rect.center().y - g.size().y / 2.0), g, p.text4);
                x += 54.0;
                let mut job = LayoutJob::default();
                let fmt = |c: Color32, bg: Color32| TextFormat { font_id: font.clone(), color: c, background: bg, ..Default::default() };
                let (a, b) = (h.mark.start.min(h.preview.len()), h.mark.end.min(h.preview.len()));
                if h.preview.is_char_boundary(a) && h.preview.is_char_boundary(b) {
                    job.append(&h.preview[..a], 0.0, fmt(p.text2, Color32::TRANSPARENT));
                    job.append(&h.preview[a..b], 0.0, fmt(p.text, p.match_bg));
                    job.append(&h.preview[b..], 0.0, fmt(p.text2, Color32::TRANSPARENT));
                } else {
                    job.append(&h.preview, 0.0, fmt(p.text2, Color32::TRANSPARENT));
                }
                let g = ui.fonts_mut(|f| f.layout_job(job));
                ui.painter().with_clip_rect(rect).galley(pos2(x, rect.center().y - g.size().y / 2.0), g, p.text);
                if resp.clicked() {
                    open = Some(i);
                }
                if let Some(path) = &h.path {
                    resp.on_hover_text(path.display().to_string());
                }
            }
        });
        if close {
            self.results_open = false;
        }
        if let Some(i) = open {
            self.open_hit(i);
        }
    }

    // ------------------------------------------------------------ status

    fn status_bar(&mut self, ui: &mut Ui, cmds: &mut Vec<Cmd>) {
        let p = self.palette;
        let r = ui.max_rect();
        ui.painter().line_segment([pos2(r.left() - 10.0, r.top() + 0.5), pos2(r.right() + 10.0, r.top() + 0.5)], Stroke::new(1.0, p.line));
        let t = self.tab();
        let doc = &t.doc;
        let caret = doc.sel.head;
        let ls = doc.line_start_of(caret);
        let col = String::from_utf8_lossy(&doc.text.read(ls.max(caret.saturating_sub(65536))..caret)).chars().count() + 1;
        let line = doc.text.line_of(caret).map(|l| thousands(l + 1)).unwrap_or_else(|| "…".into());
        let small = |s: String| RichText::new(s).size(12.0).color(p.text2);
        let mut toggle_indent = false;
        ui.horizontal_centered(|ui| {
            ui.spacing_mut().item_spacing.x = 14.0;
            ui.label(small(format!("Ln {line}, Col {col}")));
            if !doc.sel.is_empty() {
                let r = doc.sel.range();
                let n = r.end - r.start;
                let label = if n <= 1 << 20 {
                    let chars = String::from_utf8_lossy(&doc.text.read(r.clone())).chars().count();
                    let lines = doc.text.read(r).iter().filter(|b| **b == b'\n').count();
                    if lines > 0 { format!("{} selected, {} lines", thousands(chars as u64), lines + 1) } else { format!("{} selected", thousands(chars as u64)) }
                } else {
                    format!("{} selected", human(n))
                };
                ui.label(small(label));
            }
            if !doc.text.indexed() {
                ui.label(RichText::new(format!("Counting lines {:.0}%", doc.text.index_progress() * 100.0)).size(12.0).color(p.text3));
            } else if let Some(n) = doc.text.line_count() {
                ui.label(RichText::new(format!("{} lines", thousands(n))).size(12.0).color(p.text3));
            }
            if let Some(s) = &t.saving {
                ui.label(RichText::new(format!("Saving {:.0}%", s.progress.fraction() * 100.0)).size(12.0).color(p.accent));
                if ui.small_button("Stop").clicked() {
                    s.progress.cancel.store(true, Ordering::Relaxed);
                }
            }
            if t.follow {
                ui.label(RichText::new("Following").size(12.0).color(p.ok)).on_hover_text("New lines written to the file appear here; editing stops following");
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                ui.spacing_mut().button_padding = vec2(7.0, 2.0);
                let seg = |ui: &mut Ui, text: String| ui.add(egui::Button::new(RichText::new(text).size(12.0).color(p.text2)).frame_when_inactive(false).min_size(vec2(0.0, 20.0)));
                if seg(ui, t.lang.name().to_string()).on_hover_text("Language").clicked() {
                    ui.ctx().data_mut(|d| d.insert_temp(Id::unique("status-pop"), 1u8));
                }
                if seg(ui, t.doc.encoding.name()).on_hover_text("Encoding").clicked() {
                    ui.ctx().data_mut(|d| d.insert_temp(Id::unique("status-pop"), 2u8));
                }
                if seg(ui, t.doc.eol.name().to_string()).on_hover_text("Line endings").clicked() {
                    ui.ctx().data_mut(|d| d.insert_temp(Id::unique("status-pop"), 3u8));
                }
                let ins = if t.view.overwrite { "OVR" } else { "INS" };
                seg(ui, ins.into()).on_hover_text("Insert key switches overwrite mode");
                let ws = if self.settings.editor.insert_spaces { format!("Spaces: {}", self.settings.editor.tab_width) } else { format!("Tabs: {}", self.settings.editor.tab_width) };
                if seg(ui, ws).on_hover_text("Indentation: click to switch between spaces and tabs").clicked() {
                    toggle_indent = true;
                }
                seg(ui, human(t.doc.len()));
                if !t.doc.bookmarks.is_empty() {
                    seg(ui, format!("{} bookmark{}", t.doc.bookmarks.len(), if t.doc.bookmarks.len() == 1 { "" } else { "s" }));
                }
                if self.settings.editor.wrap && seg(ui, "Wrap".into()).on_hover_text("Word wrap is on").clicked() {
                    cmds.push(Cmd::Wrap);
                }
            });
        });
        if toggle_indent {
            self.settings.editor.insert_spaces = !self.settings.editor.insert_spaces;
        }
        // the popups the right-hand segments open
        let which: u8 = ui.ctx().data(|d| d.get_temp(Id::unique("status-pop")).unwrap_or(0));
        if which != 0 {
            let mut open = true;
            egui::Window::new(match which {
                1 => "Language",
                2 => "Encoding",
                _ => "Line endings",
            })
            .id(Id::unique("status-window"))
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::RIGHT_BOTTOM, vec2(-12.0, -32.0))
            .open(&mut open)
            .show(ui.ctx(), |ui| match which {
                1 => self.language_menu(ui),
                2 => self.encoding_menu(ui),
                _ => {
                    let cur = self.tab().doc.eol;
                    for e in [Eol::Lf, Eol::CrLf, Eol::Cr] {
                        if ui.selectable_label(cur == e, e.name()).clicked() {
                            if !self.tab_mut().doc.convert_eol(e) {
                                self.toast("Too big to convert line endings (256 MB)", Tone::Bad);
                            }
                            ui.close();
                        }
                    }
                }
            });
            let closed_inside = ui.ctx().data(|d| d.get_temp::<bool>(Id::unique("status-close")).unwrap_or(false));
            if !open || closed_inside || ui.input(|i| i.key_pressed(Key::Escape)) {
                ui.ctx().data_mut(|d| d.insert_temp(Id::unique("status-pop"), 0u8));
            }
        }
    }

    // ------------------------------------------------------------ dialogs

    fn dialogs(&mut self, ctx: &egui::Context) {
        let p = self.palette;
        if let Some(mut text) = self.goto.take() {
            let mut keep = true;
            let total = self.tab().doc.text.line_count();
            egui::Modal::new(Id::unique("goto")).show(ctx, |ui| {
                ui.set_width(320.0);
                ui.label(RichText::new("Go to line").font(semibold(15.0)));
                ui.add_space(6.0);
                let hint = match total {
                    Some(n) => format!("1 – {}, or a percentage like 50%", thousands(n)),
                    None => format!("A line, or a percentage (still counting lines: {:.0}%)", self.tab().doc.text.index_progress() * 100.0),
                };
                let r = ui.add(TextEdit::singleline(&mut text).hint_text(hint).desired_width(f32::INFINITY).font(FontId::monospace(14.0)).margin(Margin::symmetric(8, 6)));
                r.request_focus();
                if ui.input(|i| i.key_pressed(Key::Enter)) {
                    let s = text.trim().replace([',', '_', ' '], "");
                    if let Some(pct) = s.strip_suffix('%').and_then(|v| v.parse::<f64>().ok()) {
                        let t = self.tab_mut();
                        let at = match t.doc.text.line_count() {
                            Some(n) => t.doc.text.line_start(((n as f64 - 1.0) * pct / 100.0).max(0.0) as u64).unwrap_or(0),
                            None => (t.doc.len() as f64 * pct / 100.0) as u64,
                        };
                        let at = margin_core::rows::row_start(&t.doc.text, at);
                        t.doc.sel = margin_core::doc::Sel::caret(at);
                        t.view.reveal = Reveal::Center;
                        keep = false;
                    } else if let Ok(n) = s.parse::<u64>() {
                        if self.goto_line(n.saturating_sub(1)) {
                            keep = false;
                        } else {
                            self.toast("That line hasn't been counted yet, or doesn't exist", Tone::Info);
                        }
                    }
                    self.focus_editor = !keep;
                }
                if ui.input(|i| i.key_pressed(Key::Escape)) {
                    keep = false;
                    self.focus_editor = true;
                }
            });
            if keep {
                self.goto = Some(text);
            }
        }

        if self.fif.is_some() {
            let mut go: Option<(PathBuf, String, bool)> = None;
            let mut cancel = false;
            egui::Modal::new(Id::unique("fif")).show(ctx, |ui| {
                ui.set_width(520.0);
                let Some(f) = self.fif.as_mut() else { return };
                ui.label(RichText::new("Find in files").font(semibold(15.0)));
                ui.add_space(8.0);
                egui::Grid::new("fif-grid").num_columns(2).spacing(vec2(10.0, 8.0)).show(ui, |ui| {
                    ui.label(RichText::new("Find").color(p.text2));
                    let r = ui.add(TextEdit::singleline(&mut self.find.query.pattern).font(FontId::monospace(13.0)).desired_width(f32::INFINITY).margin(Margin::symmetric(8, 5)));
                    if f.focus {
                        r.request_focus();
                        f.focus = false;
                    }
                    ui.end_row();
                    ui.label("");
                    ui.horizontal(|ui| {
                        ui.checkbox(&mut self.find.query.case, "Match case");
                        ui.checkbox(&mut self.find.query.word, "Whole words");
                        ui.checkbox(&mut self.find.query.regex, "Regular expression");
                    });
                    ui.end_row();
                    ui.label(RichText::new("In folder").color(p.text2));
                    ui.horizontal(|ui| {
                        ui.add(TextEdit::singleline(&mut f.dir).desired_width(ui.available_width() - 80.0).margin(Margin::symmetric(8, 5)));
                        if ui.button("Browse…").clicked()
                            && let Some(d) = rfd::FileDialog::new().set_directory(&f.dir).pick_folder()
                        {
                            f.dir = d.display().to_string();
                        }
                    });
                    ui.end_row();
                    ui.label(RichText::new("Files").color(p.text2));
                    ui.add(TextEdit::singleline(&mut f.filter).hint_text("*.rs; *.toml; !target   (empty: every file)").desired_width(f32::INFINITY).margin(Margin::symmetric(8, 5)));
                    ui.end_row();
                    ui.label("");
                    ui.checkbox(&mut f.hidden, "Include hidden files (.gitignore is always respected)");
                    ui.end_row();
                });
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.add(egui::Button::new(RichText::new("Search").color(p.accent_ink)).fill(p.accent)).clicked() || ui.input(|i| i.key_pressed(Key::Enter)) {
                            go = Some((PathBuf::from(f.dir.trim()), f.filter.clone(), f.hidden));
                        }
                        if ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(Key::Escape)) {
                            cancel = true;
                        }
                    });
                });
            });
            if let Some((dir, filter, hidden)) = go {
                if dir.is_dir() {
                    self.fif = None;
                    self.find_in_files(dir, filter, hidden);
                } else {
                    self.toast("That folder doesn't exist", Tone::Bad);
                }
            }
            if cancel {
                self.fif = None;
                self.focus_editor = true;
            }
        }

        if self.settings_open {
            let mut open = true;
            egui::Modal::new(Id::unique("settings")).show(ctx, |ui| {
                ui.set_width(440.0);
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Preferences").font(semibold(15.0)));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if icon_button(ui, icon::CLOSE, "Close", false).clicked() {
                            open = false;
                        }
                    });
                });
                ui.add_space(8.0);
                egui::Grid::new("settings-grid").num_columns(2).spacing(vec2(16.0, 9.0)).show(ui, |ui| {
                    ui.label(RichText::new("Theme").color(p.text2));
                    let mut theme = self.settings.theme.clone();
                    egui::ComboBox::from_id_salt("theme").selected_text(match theme.as_str() {
                        "dark" => "Dark",
                        "light" => "Light",
                        _ => "Follow the system",
                    }).show_ui(ui, |ui| {
                        ui.selectable_value(&mut theme, "system".to_string(), "Follow the system");
                        ui.selectable_value(&mut theme, "dark".to_string(), "Dark");
                        ui.selectable_value(&mut theme, "light".to_string(), "Light");
                    });
                    if theme != self.settings.theme {
                        self.settings.theme = theme;
                        let ctx = ui.ctx().clone();
                        self.apply_theme_pref(&ctx);
                    }
                    let e = &mut self.settings.editor;
                    ui.end_row();
                    ui.label(RichText::new("Text size").color(p.text2));
                    ui.add(egui::Slider::new(&mut e.font_size, 8.0..=32.0).step_by(0.5).suffix(" pt"));
                    ui.end_row();
                    ui.label(RichText::new("Line spacing").color(p.text2));
                    ui.add(egui::Slider::new(&mut e.line_spacing, 1.0..=2.2).step_by(0.05));
                    ui.end_row();
                    ui.label(RichText::new("Tab width").color(p.text2));
                    ui.add(egui::Slider::new(&mut e.tab_width, 1..=12));
                    ui.end_row();
                    ui.label(RichText::new("Indent with").color(p.text2));
                    ui.horizontal(|ui| {
                        ui.radio_value(&mut e.insert_spaces, true, "Spaces");
                        ui.radio_value(&mut e.insert_spaces, false, "Tabs");
                    });
                    ui.end_row();
                    ui.label(RichText::new("Editing").color(p.text2));
                    ui.vertical(|ui| {
                        ui.checkbox(&mut e.auto_indent, "Keep indentation on new lines");
                        ui.checkbox(&mut e.wrap, "Wrap long lines");
                        ui.checkbox(&mut e.smart_highlight, "Highlight other uses of the selected word");
                    });
                    ui.end_row();
                    ui.label(RichText::new("Showing").color(p.text2));
                    ui.vertical(|ui| {
                        ui.checkbox(&mut e.line_numbers, "Line numbers");
                        ui.checkbox(&mut e.highlight_line, "Current line");
                        ui.checkbox(&mut e.show_whitespace, "Spaces, tabs and line ends");
                        ui.checkbox(&mut e.caret_blink, "Blinking caret");
                    });
                    ui.end_row();
                    ui.label(RichText::new("Closing").color(p.text2));
                    ui.checkbox(&mut self.settings.restore_session, "Remember open files and unsaved work");
                    ui.end_row();
                    ui.label(RichText::new("Searching").color(p.text2));
                    ui.checkbox(&mut self.settings.wrap_search, "Continue from the top after the last match");
                    ui.end_row();
                });
                ui.add_space(6.0);
                ui.label(RichText::new(format!("Margin {}  ·  settings in {}", env!("CARGO_PKG_VERSION"), crate::session::config_dir().display())).size(11.5).color(p.text3));
                if ui.input(|i| i.key_pressed(Key::Escape)) {
                    open = false;
                }
            });
            if !open {
                self.settings_open = false;
                self.focus_editor = true;
            }
        }

        if let Some(ask) = self.ask.take() {
            let mut keep = true;
            match &ask {
                Ask::Unsaved { tabs, quit } => {
                    let names: Vec<String> = tabs.iter().filter_map(|id| self.tabs.iter().find(|t| t.id == *id && t.doc.dirty())).map(|t| t.title()).collect();
                    let tabs = tabs.clone();
                    let quit = *quit;
                    egui::Modal::new(Id::unique("unsaved")).show(ctx, |ui| {
                        ui.set_width(400.0);
                        let title = if names.len() == 1 { format!("Save changes to {}?", names[0]) } else { format!("Save changes to {} files?", names.len()) };
                        ui.label(RichText::new(title).font(semibold(15.0)));
                        ui.add_space(4.0);
                        if names.len() > 1 {
                            for n in names.iter().take(8) {
                                ui.label(RichText::new(format!("  {n}")).color(p.text2));
                            }
                        }
                        ui.label(RichText::new("If you don't, your changes are lost.").color(p.text3));
                        ui.add_space(10.0);
                        ui.horizontal(|ui| {
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if ui.add(egui::Button::new(RichText::new("Save").color(p.accent_ink)).fill(p.accent)).clicked() || ui.input(|i| i.key_pressed(Key::Enter)) {
                                    let mut ok = true;
                                    for id in &tabs {
                                        if let Some(i) = self.tabs.iter().position(|t| t.id == *id && t.doc.dirty())
                                            && !self.save(i, false)
                                        {
                                            ok = false;
                                            break;
                                        }
                                    }
                                    keep = false;
                                    if ok {
                                        if self.tabs.iter().any(|t| t.saving.is_some()) {
                                            self.toast("Saving in the background…", Tone::Info);
                                        } else if quit {
                                            self.save_session();
                                            self.quitting = true;
                                            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                                        } else {
                                            self.drop_tabs(&tabs);
                                        }
                                    }
                                }
                                if ui.button("Don't save").clicked() {
                                    keep = false;
                                    if quit {
                                        for id in &tabs {
                                            if let Some(t) = self.tabs.iter_mut().find(|t| t.id == *id) {
                                                t.doc.mark_saved();
                                                t.backup = None;
                                            }
                                        }
                                        self.save_session();
                                        self.quitting = true;
                                        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                                    } else {
                                        self.drop_tabs(&tabs);
                                    }
                                }
                                if ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(Key::Escape)) {
                                    keep = false;
                                }
                            });
                        });
                    });
                }
                Ask::Elevate { tab, path } => {
                    let (tab, path) = (*tab, path.clone());
                    egui::Modal::new(Id::unique("elevate")).show(ctx, |ui| {
                        ui.set_width(420.0);
                        ui.label(RichText::new("You can't write to this file").font(semibold(15.0)));
                        ui.label(RichText::new(path.display().to_string()).monospace().color(p.text2));
                        ui.add_space(10.0);
                        ui.horizontal(|ui| {
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if ui.add(egui::Button::new(RichText::new("Save as administrator").color(p.accent_ink)).fill(p.accent)).clicked() {
                                    keep = false;
                                    self.save_elevated(tab, path.clone());
                                }
                                if ui.button("Save elsewhere…").clicked() {
                                    keep = false;
                                    if let Some(i) = self.tabs.iter().position(|t| t.id == tab) {
                                        self.save(i, true);
                                    }
                                }
                                if ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(Key::Escape)) {
                                    keep = false;
                                }
                            });
                        });
                    });
                }
            }
            if keep {
                self.ask = Some(ask);
            }
        }
    }

    fn draw_toasts(&self, ctx: &egui::Context) {
        if self.toasts.is_empty() {
            return;
        }
        let p = self.palette;
        egui::Area::new(Id::unique("toasts")).anchor(Align2::RIGHT_BOTTOM, vec2(-16.0, -36.0)).order(egui::Order::Foreground).interactable(false).show(ctx, |ui| {
            ui.with_layout(Layout::bottom_up(Align::Max), |ui| {
                for t in self.toasts.iter().rev() {
                    let age = t.at.elapsed().as_secs_f32();
                    let fade = ((4.2 - age) / 0.4).clamp(0.0, 1.0);
                    let (glyph, color) = match t.tone {
                        Tone::Ok => (icon::CHECK, p.ok),
                        Tone::Bad => (icon::ALERT, p.bad),
                        Tone::Info => (icon::DOT, p.text3),
                    };
                    Frame::new()
                        .fill(p.panel.gamma_multiply(fade))
                        .stroke(Stroke::new(1.0, p.line_strong.gamma_multiply(fade)))
                        .corner_radius(CornerRadius::same(8))
                        .inner_margin(Margin::symmetric(12, 8))
                        .shadow(egui::Shadow { offset: [0, 4], blur: 14, spread: 0, color: Color32::from_black_alpha((60.0 * fade) as u8) })
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.label(RichText::new(glyph).font(icon_font(13.0)).color(color.gamma_multiply(fade)));
                                ui.label(RichText::new(&t.text).size(12.5).color(p.text.gamma_multiply(fade)));
                            });
                        });
                    ui.add_space(6.0);
                }
            });
        });
    }
}

/// The app's mark: a page with a margin rule.
pub fn paint_mark(ui: &Ui, r: Rect, p: &crate::theme::Palette) {
    let page = r.shrink(3.0);
    ui.painter().rect_filled(page, CornerRadius::same(4), p.raised);
    ui.painter().rect_stroke(page, CornerRadius::same(4), Stroke::new(1.0, p.line_strong), egui::StrokeKind::Inside);
    let x = page.left() + page.width() * 0.32;
    ui.painter().line_segment([pos2(x, page.top() + 2.5), pos2(x, page.bottom() - 2.5)], Stroke::new(1.5, p.accent));
    for (k, w) in [0.62f32, 0.45, 0.55].iter().enumerate() {
        let y = page.top() + 5.0 + k as f32 * 3.6;
        ui.painter().line_segment([pos2(x + 2.5, y), pos2(x + 2.5 + (page.right() - x - 5.0) * w, y)], Stroke::new(1.2, p.text3));
    }
}

fn short_dir(d: &str) -> String {
    let home = dirs::home_dir().map(|h| h.display().to_string()).unwrap_or_default();
    let d = if !home.is_empty() && d.starts_with(&home) { format!("~{}", &d[home.len()..]) } else { d.to_string() };
    if d.chars().count() > 40 { format!("…{}", d.chars().rev().take(38).collect::<String>().chars().rev().collect::<String>()) } else { d }
}

fn open_folder(dir: &std::path::Path) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        std::process::Command::new("explorer").arg(dir).spawn().map(|_| ())
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open").arg(dir).spawn().map(|_| ())
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        std::process::Command::new("xdg-open").arg(dir).spawn().map(|_| ())
    }
}

#[allow(dead_code)]
fn unused(_: FifDialog) {}
