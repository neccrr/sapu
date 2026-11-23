//! Look & feel: a warm, Claude-inspired palette (ivory / charcoal with a clay
//! accent, serif headings) plus "liquid glass" surfaces: translucent cards
//! with a bright rim, a soft top sheen and a gentle shadow. On macOS the
//! window itself is translucent with the system blur behind it.

use eframe::egui::{
    self, epaint::Shadow, pos2, vec2, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, InnerResponse,
    Margin, Mesh, Rect, RichText, Shape, Stroke, Theme, Ui,
};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

static DARK: AtomicBool = AtomicBool::new(false);
static GLASS: AtomicBool = AtomicBool::new(false);

const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Color32 {
    Color32::from_rgba_unmultiplied_const(r, g, b, a)
}

#[derive(Clone, Copy)]
pub struct Palette {
    /// Window background (solid).
    pub bg: Color32,
    /// Window background when the macOS blur is behind it.
    pub bg_glass: Color32,
    /// Bars and side panels.
    pub panel: Color32,
    pub panel_glass: Color32,
    /// Glass cards.
    pub surface: Color32,
    pub surface_hi: Color32,
    /// Bright rim of glass cards, and the hairline around them.
    pub rim: Color32,
    pub edge: Color32,
    /// Top-half sheen on glass.
    pub sheen: Color32,
    pub shadow: Color32,
    pub hair: Color32,

    pub text: Color32,
    pub text2: Color32,
    pub dim: Color32,
    pub dir: Color32,
    pub link: Color32,

    pub accent: Color32,
    pub track: Color32,
    pub row_hover: Color32,
    pub row_sel: Color32,

    pub ok: Color32,
    pub warn: Color32,
    pub bad: Color32,
    pub likely: Color32,
    pub keep: Color32,
    pub ai: Color32,

    pub tiles: [Color32; 8],
    pub file_tile: Color32,
    pub other_tile: Color32,
    pub tile_ink: Color32,
}

const TILES: [Color32; 8] = [
    Color32::from_rgb(217, 119, 87),  // clay
    Color32::from_rgb(120, 150, 190), // slate blue
    Color32::from_rgb(143, 175, 128), // sage
    Color32::from_rgb(214, 172, 96),  // ochre
    Color32::from_rgb(176, 132, 166), // mauve
    Color32::from_rgb(98, 163, 158),  // teal
    Color32::from_rgb(201, 168, 132), // sand
    Color32::from_rgb(186, 106, 86),  // rust
];

pub const LIGHT: Palette = Palette {
    bg: Color32::from_rgb(245, 244, 238),
    bg_glass: rgba(245, 244, 238, 205),
    panel: Color32::from_rgb(240, 238, 230),
    panel_glass: rgba(240, 238, 230, 170),
    surface: rgba(255, 255, 255, 175),
    surface_hi: rgba(255, 255, 255, 235),
    rim: rgba(255, 255, 255, 230),
    edge: rgba(31, 30, 29, 22),
    sheen: rgba(255, 255, 255, 90),
    shadow: rgba(60, 40, 20, 22),
    hair: rgba(31, 30, 29, 25),

    text: Color32::from_rgb(31, 30, 29),
    text2: Color32::from_rgb(94, 93, 89),
    dim: Color32::from_rgb(138, 137, 131),
    dir: Color32::from_rgb(52, 86, 135),
    link: Color32::from_rgb(70, 110, 120),

    accent: Color32::from_rgb(217, 119, 87),
    track: rgba(31, 30, 29, 18),
    row_hover: rgba(31, 30, 29, 12),
    row_sel: rgba(217, 119, 87, 38),

    ok: Color32::from_rgb(70, 140, 88),
    warn: Color32::from_rgb(190, 135, 30),
    bad: Color32::from_rgb(196, 78, 58),
    likely: Color32::from_rgb(52, 138, 140),
    keep: Color32::from_rgb(100, 112, 148),
    ai: Color32::from_rgb(128, 92, 200),

    tiles: TILES,
    file_tile: Color32::from_rgb(170, 168, 160),
    other_tile: Color32::from_rgb(205, 203, 195),
    tile_ink: Color32::from_rgb(31, 30, 29),
};

pub const DARK_P: Palette = Palette {
    bg: Color32::from_rgb(38, 38, 36),
    bg_glass: rgba(34, 34, 32, 200),
    panel: Color32::from_rgb(31, 30, 29),
    panel_glass: rgba(28, 27, 26, 165),
    surface: rgba(255, 255, 255, 16),
    surface_hi: rgba(255, 255, 255, 30),
    rim: rgba(255, 255, 255, 40),
    edge: rgba(255, 255, 255, 18),
    sheen: rgba(255, 255, 255, 14),
    shadow: rgba(0, 0, 0, 70),
    hair: rgba(255, 255, 255, 18),

    text: Color32::from_rgb(242, 241, 236),
    text2: Color32::from_rgb(189, 187, 179),
    dim: Color32::from_rgb(142, 140, 132),
    dir: Color32::from_rgb(160, 190, 235),
    link: Color32::from_rgb(130, 190, 200),

    accent: Color32::from_rgb(217, 119, 87),
    track: rgba(255, 255, 255, 20),
    row_hover: rgba(255, 255, 255, 12),
    row_sel: rgba(217, 119, 87, 48),

    ok: Color32::from_rgb(125, 190, 135),
    warn: Color32::from_rgb(232, 190, 95),
    bad: Color32::from_rgb(232, 112, 95),
    likely: Color32::from_rgb(110, 200, 198),
    keep: Color32::from_rgb(155, 168, 205),
    ai: Color32::from_rgb(185, 152, 255),

    tiles: TILES,
    file_tile: Color32::from_rgb(120, 118, 112),
    other_tile: Color32::from_rgb(78, 77, 73),
    tile_ink: Color32::from_rgb(31, 30, 29),
};

/// The palette for this frame (theme + whether the glass blur is active).
pub fn pal() -> Palette {
    let mut p = if DARK.load(Ordering::Relaxed) { DARK_P } else { LIGHT };
    if GLASS.load(Ordering::Relaxed) {
        p.bg = p.bg_glass;
        p.panel = p.panel_glass;
    }
    p
}

pub fn set_dark(dark: bool) {
    DARK.store(dark, Ordering::Relaxed);
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn set_glass(on: bool) {
    GLASS.store(on, Ordering::Relaxed);
}

pub fn glass_active() -> bool {
    GLASS.load(Ordering::Relaxed)
}

// ── Type ────────────────────────────────────────────────────────────

pub fn serif_family() -> FontFamily {
    FontFamily::Name("serif".into())
}

/// Headline text in the serif face.
pub fn serif(text: impl Into<String>, size: f32) -> RichText {
    RichText::new(text).family(serif_family()).size(size)
}

/// Use a system serif for headings (New York / Georgia on macOS).
fn install_fonts(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    let candidates = [
        "/System/Library/Fonts/Supplemental/Georgia.ttf",
        "/Library/Fonts/Georgia.ttf",
        "/System/Library/Fonts/NewYork.ttf",
        "/usr/share/fonts/truetype/dejavu/DejaVuSerif.ttf",
    ];
    let mut family = Vec::new();
    if let Some(bytes) = candidates.iter().find_map(|p| std::fs::read(p).ok()) {
        fonts.font_data.insert("serif".into(), Arc::new(FontData::from_owned(bytes)));
        family.push("serif".to_string());
    }
    // Fall back to the normal UI font (and its emoji) for missing glyphs.
    if let Some(prop) = fonts.families.get(&FontFamily::Proportional) {
        family.extend(prop.iter().cloned());
    }
    fonts.families.insert(serif_family(), family);
    ctx.set_fonts(fonts);
}

// ── Style ───────────────────────────────────────────────────────────

fn style_for(p: &Palette, style: &mut egui::Style) {
    let v = &mut style.visuals;
    v.override_text_color = Some(p.text);
    v.panel_fill = p.bg;
    v.window_fill = if v.dark_mode { Color32::from_rgb(44, 43, 41) } else { Color32::from_rgb(250, 249, 245) };
    v.window_stroke = Stroke::new(1.0, p.edge);
    v.window_corner_radius = CornerRadius::same(18);
    v.window_shadow = Shadow { offset: [0, 14], blur: 40, spread: 0, color: p.shadow.gamma_multiply(1.6) };
    v.popup_shadow = Shadow { offset: [0, 8], blur: 24, spread: 0, color: p.shadow };
    v.menu_corner_radius = CornerRadius::same(12);
    v.extreme_bg_color = p.track;
    v.faint_bg_color = p.row_hover;
    v.selection.bg_fill = p.accent.gamma_multiply(0.55);
    v.selection.stroke = Stroke::new(1.0, p.accent);
    v.hyperlink_color = p.link;

    let w = &mut v.widgets;
    for (wv, fill) in [
        (&mut w.inactive, p.surface),
        (&mut w.hovered, p.surface_hi),
        (&mut w.active, p.accent.gamma_multiply(0.35)),
        (&mut w.open, p.surface_hi),
    ] {
        wv.bg_fill = fill;
        wv.weak_bg_fill = fill;
        wv.corner_radius = CornerRadius::same(10);
        wv.bg_stroke = Stroke::new(1.0, p.edge);
        wv.fg_stroke = Stroke::new(1.0, p.text);
    }
    w.noninteractive.bg_stroke = Stroke::new(1.0, p.hair);
    w.noninteractive.fg_stroke = Stroke::new(1.0, p.text);
    w.hovered.bg_stroke = Stroke::new(1.0, p.accent.gamma_multiply(0.6));

    style.spacing.item_spacing = vec2(8.0, 3.0);
    style.spacing.button_padding = vec2(12.0, 6.0);
    style.spacing.interact_size.y = 26.0;
    style.animation_time = 0.18;
}

pub fn apply(ctx: &egui::Context) {
    install_fonts(ctx);
    // Follows macOS Light/Dark. Override with SAPU_THEME=light|dark.
    let pref = match std::env::var("SAPU_THEME").as_deref() {
        Ok("light") => egui::ThemePreference::Light,
        Ok("dark") => egui::ThemePreference::Dark,
        _ => egui::ThemePreference::System,
    };
    ctx.set_theme(pref);
    ctx.style_mut_of(Theme::Light, |s| style_for(&LIGHT, s));
    ctx.style_mut_of(Theme::Dark, |s| style_for(&DARK_P, s));
}

// ── Glass ───────────────────────────────────────────────────────────

/// A liquid-glass card: soft shadow, translucent fill, top sheen, bright rim.
pub fn glass<R>(
    ui: &mut Ui,
    radius: u8,
    margin: Margin,
    tint: Option<Color32>,
    add: impl FnOnce(&mut Ui) -> R,
) -> InnerResponse<R> {
    let p = pal();
    // Reserve paint slots *under* the content, fill them in once we know the size.
    let shadow_i = ui.painter().add(Shape::Noop);
    let fill_i = ui.painter().add(Shape::Noop);
    let sheen_i = ui.painter().add(Shape::Noop);

    let inner = egui::Frame::new().inner_margin(margin).show(ui, add);
    let rect = inner.response.rect;
    let cr = CornerRadius::same(radius);
    let fill = tint.unwrap_or(p.surface);

    let shadow = Shadow { offset: [0, 6], blur: 22, spread: 0, color: p.shadow };
    ui.painter().set(shadow_i, shadow.as_shape(rect, cr));
    ui.painter()
        .set(fill_i, egui::epaint::RectShape::new(rect, cr, fill, Stroke::new(1.0, p.edge), egui::StrokeKind::Inside));
    ui.painter().set(sheen_i, sheen_shape(rect, radius as f32, p.sheen, p.rim));
    inner
}

/// Top-half gradient plus a 1px highlight along the top edge.
pub fn sheen_shape(rect: Rect, radius: f32, sheen: Color32, rim: Color32) -> Shape {
    let inset = radius * 0.7;
    let top = rect.top() + 1.0;
    let body = Rect::from_min_max(
        pos2(rect.left() + inset * 0.5, top),
        pos2(rect.right() - inset * 0.5, rect.top() + rect.height() * 0.55),
    );
    let mut mesh = Mesh::default();
    let i = mesh.vertices.len() as u32;
    mesh.colored_vertex(body.left_top(), sheen);
    mesh.colored_vertex(body.right_top(), sheen);
    mesh.colored_vertex(body.left_bottom(), Color32::TRANSPARENT);
    mesh.colored_vertex(body.right_bottom(), Color32::TRANSPARENT);
    mesh.add_triangle(i, i + 1, i + 2);
    mesh.add_triangle(i + 1, i + 3, i + 2);
    let rim_line = Shape::line_segment(
        [pos2(rect.left() + radius, top + 0.5), pos2(rect.right() - radius, top + 0.5)],
        Stroke::new(1.0, rim),
    );
    Shape::Vec(vec![Shape::mesh(mesh), rim_line])
}

// ── Motion ──────────────────────────────────────────────────────────

/// 0→1 with a soft landing.
pub fn ease_out(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    1.0 - (1.0 - t).powi(3)
}
