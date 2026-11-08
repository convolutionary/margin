//! Fonts, colours and the egui style. Dark first; light swaps the same roles.

use egui::{Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Margin, Shadow, Stroke, Visuals};
use std::sync::Arc;

pub const ICONS: &str = "icons";
pub const MONO_ITALIC: &str = "mono-italic";
pub const UI_MEDIUM: &str = "ui-medium";
pub const UI_SEMIBOLD: &str = "ui-semibold";

/// Every colour the editor paints with.
#[derive(Clone, Copy, Debug)]
pub struct Palette {
    pub dark: bool,
    pub chrome: Color32,  // menu bar, tab strip, status bar
    pub panel: Color32,   // find bar, results
    pub editor: Color32,  // the text area
    pub raised: Color32,  // inputs, hovered things
    pub pressed: Color32, // selected tab, pressed buttons
    pub line: Color32,
    pub line_strong: Color32,
    pub text: Color32,
    pub text2: Color32,
    pub text3: Color32,
    pub text4: Color32,
    pub accent: Color32,
    pub accent_ink: Color32,
    pub accent_soft: Color32,
    pub selection: Color32,
    pub selection_dim: Color32,
    pub current_line: Color32,
    pub match_bg: Color32,
    pub match_current: Color32,
    pub word_bg: Color32,
    pub caret: Color32,
    pub ok: Color32,
    pub warn: Color32,
    pub bad: Color32,
    // syntax
    pub keyword: Color32,
    pub string: Color32,
    pub comment: Color32,
    pub number: Color32,
    pub function: Color32,
    pub r#type: Color32,
    pub constant: Color32,
    pub tag: Color32,
    pub attribute: Color32,
    pub operator: Color32,
    pub punctuation: Color32,
    pub invalid: Color32,
}

const fn hex(c: u32) -> Color32 {
    Color32::from_rgb((c >> 16) as u8, (c >> 8) as u8, c as u8)
}

const fn alpha(c: u32, a: u8) -> Color32 {
    Color32::from_rgba_unmultiplied_const((c >> 16) as u8, (c >> 8) as u8, c as u8, a)
}

pub const DARK: Palette = Palette {
    dark: true,
    chrome: hex(0x121316),
    panel: hex(0x16181b),
    editor: hex(0x0e0f12),
    raised: hex(0x1d1f23),
    pressed: hex(0x25282d),
    line: hex(0x202227),
    line_strong: hex(0x2c2f35),
    text: hex(0xe6e7e9),
    text2: hex(0xa4a8b0),
    text3: hex(0x6d717a),
    text4: hex(0x494d55),
    accent: hex(0x8b9cff),
    accent_ink: hex(0x0b0c0e),
    accent_soft: alpha(0x8b9cff, 34),
    selection: alpha(0x8b9cff, 62),
    selection_dim: alpha(0x8b9cff, 34),
    current_line: alpha(0xffffff, 7),
    match_bg: alpha(0xe5b567, 46),
    match_current: alpha(0xe5b567, 120),
    word_bg: alpha(0xffffff, 18),
    caret: hex(0xc9d0ff),
    ok: hex(0x6fcf97),
    warn: hex(0xe5b567),
    bad: hex(0xef7b7b),
    keyword: hex(0xb7a4f6),
    string: hex(0x9fcf8f),
    comment: hex(0x6a6f78),
    number: hex(0xe5b567),
    function: hex(0x8bb4ff),
    r#type: hex(0x79c7c0),
    constant: hex(0xe88fa6),
    tag: hex(0xef9f76),
    attribute: hex(0xe5c07b),
    operator: hex(0x9aa4b5),
    punctuation: hex(0x8a8f99),
    invalid: hex(0xef7b7b),
};

pub const LIGHT: Palette = Palette {
    dark: false,
    chrome: hex(0xf3f3f1),
    panel: hex(0xf7f7f5),
    editor: hex(0xfdfdfc),
    raised: hex(0xeaeae7),
    pressed: hex(0xe0e0dc),
    line: hex(0xe4e4e0),
    line_strong: hex(0xd3d3ce),
    text: hex(0x1d1f23),
    text2: hex(0x50545c),
    text3: hex(0x80848c),
    text4: hex(0xa9acb2),
    accent: hex(0x4f5fd8),
    accent_ink: hex(0xffffff),
    accent_soft: alpha(0x4f5fd8, 26),
    selection: alpha(0x4f5fd8, 48),
    selection_dim: alpha(0x4f5fd8, 26),
    current_line: alpha(0x000000, 8),
    match_bg: alpha(0xd99a1e, 56),
    match_current: alpha(0xd99a1e, 140),
    word_bg: alpha(0x000000, 20),
    caret: hex(0x3443b8),
    ok: hex(0x2f8f5b),
    warn: hex(0xa86f12),
    bad: hex(0xc94545),
    keyword: hex(0x7a4fd0),
    string: hex(0x3f8a3a),
    comment: hex(0x8a8e96),
    number: hex(0xb06a10),
    function: hex(0x2f63c9),
    r#type: hex(0x1c8a82),
    constant: hex(0xc2416b),
    tag: hex(0xc2581f),
    attribute: hex(0x9a6c00),
    operator: hex(0x5c6370),
    punctuation: hex(0x6b7079),
    invalid: hex(0xc94545),
};

pub fn fonts() -> FontDefinitions {
    let mut f = FontDefinitions::default();
    let add = |f: &mut FontDefinitions, name: &str, bytes: &'static [u8]| {
        f.font_data.insert(name.into(), Arc::new(FontData::from_static(bytes)));
    };
    add(&mut f, "Inter", include_bytes!("../assets/fonts/Inter_18pt-Regular.ttf"));
    add(&mut f, "Inter-Medium", include_bytes!("../assets/fonts/Inter_18pt-Medium.ttf"));
    add(&mut f, "Inter-SemiBold", include_bytes!("../assets/fonts/Inter_18pt-SemiBold.ttf"));
    add(&mut f, "JetBrainsMono", include_bytes!("../assets/fonts/JetBrainsMono-Regular.ttf"));
    add(&mut f, "JetBrainsMono-Italic", include_bytes!("../assets/fonts/JetBrainsMono-Italic.ttf"));
    add(&mut f, "lucide", include_bytes!("../assets/fonts/lucide.ttf"));

    let fallback: Vec<String> = f.families.get(&FontFamily::Proportional).cloned().unwrap_or_default();
    let mono_fallback: Vec<String> = f.families.get(&FontFamily::Monospace).cloned().unwrap_or_default();
    let with = |first: &str, rest: &[String]| {
        let mut v = vec![first.to_string()];
        v.extend(rest.iter().cloned());
        v
    };
    f.families.insert(FontFamily::Proportional, with("Inter", &fallback));
    f.families.insert(FontFamily::Monospace, with("JetBrainsMono", &mono_fallback));
    f.families.insert(FontFamily::Name(UI_MEDIUM.into()), with("Inter-Medium", &fallback));
    f.families.insert(FontFamily::Name(UI_SEMIBOLD.into()), with("Inter-SemiBold", &fallback));
    f.families.insert(FontFamily::Name(MONO_ITALIC.into()), with("JetBrainsMono-Italic", &mono_fallback));
    f.families.insert(FontFamily::Name(ICONS.into()), vec!["lucide".into()]);
    f
}

pub fn icon_font(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(ICONS.into()))
}

pub fn ui_font(size: f32) -> FontId {
    FontId::new(size, FontFamily::Proportional)
}

pub fn medium(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(UI_MEDIUM.into()))
}

pub fn semibold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(UI_SEMIBOLD.into()))
}

/// Lucide icons by code point.
#[allow(dead_code)]
pub mod icon {
    pub const NEW: &str = "\u{e0cd}";
    pub const OPEN: &str = "\u{e247}";
    pub const SAVE: &str = "\u{e151}";
    pub const CLOSE: &str = "\u{e1b2}";
    pub const SEARCH: &str = "\u{e155}";
    pub const REPLACE: &str = "\u{e3df}";
    pub const UNDO: &str = "\u{e2a1}";
    pub const REDO: &str = "\u{e2a0}";
    pub const WRAP: &str = "\u{e248}";
    pub const PILCROW: &str = "\u{e3a7}";
    pub const BOOKMARK: &str = "\u{e064}";
    pub const CHEVRON_DOWN: &str = "\u{e071}";
    pub const CHEVRON_UP: &str = "\u{e074}";
    pub const CHEVRON_RIGHT: &str = "\u{e073}";
    pub const ARROW_DOWN: &str = "\u{e046}";
    pub const ARROW_UP: &str = "\u{e04e}";
    pub const REGEX: &str = "\u{e1fc}";
    pub const CASE: &str = "\u{e3dd}";
    pub const WORD: &str = "\u{e3e3}";
    pub const DOT: &str = "\u{e349}";
    pub const FILE: &str = "\u{e0d0}";
    pub const SETTINGS: &str = "\u{e245}";
    pub const LIST: &str = "\u{e10c}";
    pub const COPY: &str = "\u{e0a2}";
    pub const REFRESH: &str = "\u{e149}";
    pub const LOCK: &str = "\u{e10f}";
    pub const ALERT: &str = "\u{e193}";
    pub const CHECK: &str = "\u{e070}";
    pub const PLUS: &str = "\u{e141}";
    pub const FOLDER_SEARCH: &str = "\u{e33d}";
    pub const ENTER: &str = "\u{e0a5}";
    pub const TEXT_SEARCH: &str = "\u{e5b2}";
    pub const SELECTION: &str = "\u{e10c}";
}

/// The egui style for a palette: flat, quiet, one accent.
pub fn apply(ctx: &egui::Context, p: &Palette, theme: egui::Theme) {
    let mut v = if p.dark { Visuals::dark() } else { Visuals::light() };
    v.override_text_color = Some(p.text);
    v.panel_fill = p.chrome;
    v.window_fill = p.panel;
    v.window_stroke = Stroke::new(1.0, p.line_strong);
    v.window_corner_radius = CornerRadius::same(10);
    v.menu_corner_radius = CornerRadius::same(8);
    v.window_shadow = Shadow { offset: [0, 10], blur: 28, spread: 0, color: Color32::from_black_alpha(if p.dark { 110 } else { 40 }) };
    v.popup_shadow = Shadow { offset: [0, 6], blur: 18, spread: 0, color: Color32::from_black_alpha(if p.dark { 100 } else { 30 }) };
    v.extreme_bg_color = p.editor;
    v.text_edit_bg_color = Some(p.raised);
    v.faint_bg_color = p.raised;
    v.code_bg_color = p.raised;
    v.hyperlink_color = p.accent;
    v.warn_fg_color = p.warn;
    v.error_fg_color = p.bad;
    v.selection.bg_fill = p.selection;
    v.selection.stroke = Stroke::new(1.0, p.accent);
    v.text_cursor.stroke = Stroke::new(2.0, p.caret);
    v.button_frame = true;
    v.indent_has_left_vline = false;
    let r = CornerRadius::same(6);
    let w = &mut v.widgets;
    w.noninteractive.bg_fill = p.panel;
    w.noninteractive.weak_bg_fill = p.panel;
    w.noninteractive.bg_stroke = Stroke::new(1.0, p.line);
    w.noninteractive.fg_stroke = Stroke::new(1.0, p.text2);
    w.noninteractive.corner_radius = r;
    w.inactive.bg_fill = p.pressed; // checkboxes, radio buttons, slider rails
    w.inactive.weak_bg_fill = Color32::TRANSPARENT;
    w.inactive.bg_stroke = Stroke::NONE;
    w.inactive.fg_stroke = Stroke::new(1.0, p.text2);
    w.inactive.corner_radius = r;
    w.hovered.bg_fill = p.pressed;
    w.hovered.weak_bg_fill = p.raised;
    w.hovered.bg_stroke = Stroke::NONE;
    w.hovered.fg_stroke = Stroke::new(1.0, p.text);
    w.hovered.corner_radius = r;
    w.hovered.expansion = 0.0;
    w.active.bg_fill = p.pressed;
    w.active.weak_bg_fill = p.pressed;
    w.active.bg_stroke = Stroke::NONE;
    w.active.fg_stroke = Stroke::new(1.0, p.text);
    w.active.corner_radius = r;
    w.active.expansion = 0.0;
    w.open.bg_fill = p.raised;
    w.open.weak_bg_fill = p.raised;
    w.open.bg_stroke = Stroke::NONE;
    w.open.fg_stroke = Stroke::new(1.0, p.text);
    w.open.corner_radius = r;
    ctx.set_visuals_of(theme, v);
    ctx.style_mut_of(theme, |s| {
        s.spacing.item_spacing = egui::vec2(6.0, 4.0);
        s.spacing.button_padding = egui::vec2(8.0, 3.0);
        s.spacing.menu_margin = Margin::same(5);
        s.spacing.window_margin = Margin::same(14);
        s.spacing.interact_size.y = 22.0;
        s.spacing.combo_width = 120.0;
        s.text_styles.insert(egui::TextStyle::Body, ui_font(13.0));
        s.text_styles.insert(egui::TextStyle::Button, ui_font(13.0));
        s.text_styles.insert(egui::TextStyle::Small, ui_font(11.5));
        s.text_styles.insert(egui::TextStyle::Heading, semibold(16.0));
        s.text_styles.insert(egui::TextStyle::Monospace, FontId::monospace(13.0));
        s.interaction.tooltip_delay = 0.35;
    });
}
