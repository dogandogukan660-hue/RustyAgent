//! Arayüz tasarım sistemi: sıcak nötr palet (koyu/açık), sistem yazı tipleri ve çizgi ikonlar.
//! Emoji kullanılmaz; tüm ikonlar burada vektör olarak çizilir.

use eframe::egui::{self, Color32, CornerRadius, FontFamily, FontId, Painter, Pos2, Rect, Response, Sense, Stroke, Vec2};
use std::sync::Arc;

#[derive(Clone, Copy)]
pub struct Palette {
    pub dark: bool,
    pub bg: Color32,
    pub sidebar: Color32,
    pub surface: Color32,
    pub surface2: Color32,
    pub border: Color32,
    pub text: Color32,
    pub muted: Color32,
    pub faint: Color32,
    pub accent: Color32,
    pub on_accent: Color32,
    pub user_bubble: Color32,
    pub code_bg: Color32,
    pub danger: Color32,
    pub warn: Color32,
    pub ok: Color32,
    pub hover: Color32,
}

pub fn dark() -> Palette {
    Palette {
        dark: true,
        bg: Color32::from_rgb(38, 38, 36),
        sidebar: Color32::from_rgb(31, 30, 29),
        surface: Color32::from_rgb(48, 48, 46),
        surface2: Color32::from_rgb(56, 56, 53),
        border: Color32::from_rgb(68, 67, 63),
        text: Color32::from_rgb(236, 234, 227),
        muted: Color32::from_rgb(168, 165, 156),
        faint: Color32::from_rgb(120, 118, 111),
        accent: Color32::from_rgb(217, 119, 87),
        on_accent: Color32::from_rgb(255, 255, 255),
        user_bubble: Color32::from_rgb(20, 20, 19),
        code_bg: Color32::from_rgb(28, 28, 27),
        danger: Color32::from_rgb(235, 110, 100),
        warn: Color32::from_rgb(232, 180, 90),
        ok: Color32::from_rgb(120, 190, 130),
        hover: Color32::from_rgb(52, 51, 49),
    }
}

pub fn light() -> Palette {
    Palette {
        dark: false,
        bg: Color32::from_rgb(250, 249, 245),
        sidebar: Color32::from_rgb(241, 239, 232),
        surface: Color32::from_rgb(255, 255, 255),
        surface2: Color32::from_rgb(244, 242, 236),
        border: Color32::from_rgb(222, 219, 209),
        text: Color32::from_rgb(31, 30, 29),
        muted: Color32::from_rgb(110, 108, 102),
        faint: Color32::from_rgb(150, 147, 140),
        accent: Color32::from_rgb(201, 100, 66),
        on_accent: Color32::from_rgb(255, 255, 255),
        user_bubble: Color32::from_rgb(240, 238, 230),
        code_bg: Color32::from_rgb(244, 242, 236),
        danger: Color32::from_rgb(200, 60, 50),
        warn: Color32::from_rgb(180, 120, 20),
        ok: Color32::from_rgb(40, 140, 70),
        hover: Color32::from_rgb(232, 229, 220),
    }
}

/// Paleti egui görünümüne uygular
pub fn apply(ctx: &egui::Context, p: &Palette) {
    ctx.set_theme(if p.dark { egui::ThemePreference::Dark } else { egui::ThemePreference::Light });
    let mut v = if p.dark { egui::Visuals::dark() } else { egui::Visuals::light() };
    v.panel_fill = p.bg;
    v.window_fill = p.surface;
    v.extreme_bg_color = p.surface;
    v.faint_bg_color = p.surface2;
    v.code_bg_color = p.code_bg;
    v.override_text_color = Some(p.text);
    v.hyperlink_color = p.accent;
    v.selection.bg_fill = p.accent.gamma_multiply(0.35);
    v.selection.stroke = Stroke::new(1.0, p.accent);
    v.window_stroke = Stroke::new(1.0, p.border);
    v.window_corner_radius = CornerRadius::same(14);
    let r = CornerRadius::same(8);
    for (w, fill) in [
        (&mut v.widgets.noninteractive, p.bg),
        (&mut v.widgets.inactive, p.surface2),
        (&mut v.widgets.hovered, p.hover),
        (&mut v.widgets.active, p.hover),
        (&mut v.widgets.open, p.surface2),
    ] {
        w.bg_fill = fill;
        w.weak_bg_fill = fill;
        w.corner_radius = r;
        w.fg_stroke = Stroke::new(1.0, p.text);
    }
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, p.border);
    v.widgets.inactive.bg_stroke = Stroke::new(1.0, p.border);
    v.widgets.hovered.bg_stroke = Stroke::new(1.0, p.faint);
    v.widgets.active.bg_stroke = Stroke::new(1.0, p.accent);
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, p.muted);
    ctx.set_visuals(v);
}

/// Sistem yazı tiplerini yükler (paketlenmez): arayüz için sans, yanıtlar için serif, kod için mono.
pub fn install_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    let try_load = |paths: &[&str]| -> Option<Vec<u8>> { paths.iter().find_map(|p| std::fs::read(p).ok()) };
    let sans = try_load(&[
        r"C:\Windows\Fonts\segoeui.ttf",
        "/System/Library/Fonts/SFNS.ttf",
        "/System/Library/Fonts/Supplemental/Arial.ttf",
        "/usr/share/fonts/truetype/noto/NotoSans-Regular.ttf",
        "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
    ]);
    let sans_bold = try_load(&[
        r"C:\Windows\Fonts\segoeuisb.ttf",
        r"C:\Windows\Fonts\segoeuib.ttf",
        "/System/Library/Fonts/Supplemental/Arial Bold.ttf",
        "/usr/share/fonts/truetype/noto/NotoSans-Bold.ttf",
        "/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf",
    ]);
    let serif = try_load(&[
        r"C:\Windows\Fonts\georgia.ttf",
        "/System/Library/Fonts/Supplemental/Georgia.ttf",
        "/usr/share/fonts/truetype/noto/NotoSerif-Regular.ttf",
        "/usr/share/fonts/truetype/dejavu/DejaVuSerif.ttf",
    ]);
    let serif_bold = try_load(&[
        r"C:\Windows\Fonts\georgiab.ttf",
        "/System/Library/Fonts/Supplemental/Georgia Bold.ttf",
        "/usr/share/fonts/truetype/noto/NotoSerif-Bold.ttf",
        "/usr/share/fonts/truetype/dejavu/DejaVuSerif-Bold.ttf",
    ]);
    let mono = try_load(&[
        r"C:\Windows\Fonts\CascadiaMono.ttf",
        r"C:\Windows\Fonts\consola.ttf",
        "/System/Library/Fonts/Supplemental/Courier New.ttf",
        "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf",
    ]);
    let defaults_prop = fonts.families.get(&FontFamily::Proportional).cloned().unwrap_or_default();
    let mut add = |name: &str, data: Option<Vec<u8>>, family: FontFamily| {
        let mut list = vec![];
        if let Some(d) = data {
            fonts.font_data.insert(name.into(), Arc::new(egui::FontData::from_owned(d)));
            list.push(name.to_string());
        }
        // Eksik karakterler için egui'nin varsayılan yazı tipleri yedek olarak kalır
        list.extend(defaults_prop.iter().cloned());
        fonts.families.insert(family, list);
    };
    add("sans", sans, FontFamily::Proportional);
    add("sans-bold", sans_bold, FontFamily::Name("bold".into()));
    add("serif", serif, FontFamily::Name("serif".into()));
    add("serif-bold", serif_bold, FontFamily::Name("serif-bold".into()));
    if let Some(d) = mono {
        fonts.font_data.insert("mono".into(), Arc::new(egui::FontData::from_owned(d)));
        fonts.families.entry(FontFamily::Monospace).or_default().insert(0, "mono".into());
    }
    ctx.set_fonts(fonts);
}

pub fn serif(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("serif".into()))
}
pub fn serif_bold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("serif-bold".into()))
}
pub fn bold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("bold".into()))
}

// ------------------------------------------------------------------ ikonlar

#[derive(Clone, Copy, PartialEq)]
pub enum Icon {
    Plus,
    Chat,
    Brain,
    People,
    Chip,
    Plug,
    Book,
    Puzzle,
    Clock,
    Gear,
    Search,
    Send,
    Stop,
    Copy,
    Retry,
    Undo,
    Clip,
    Sidebar,
    Team,
    Close,
    Trash,
    Download,
    Edit,
    Warn,
}

/// İkonu `rect` içine çizgi olarak çizer
pub fn draw_icon(p: &Painter, rect: Rect, icon: Icon, color: Color32) {
    let s = rect.width().min(rect.height());
    let c = rect.center();
    let u = s / 16.0;
    let st = Stroke::new(1.4 * u.max(0.8), color);
    let pt = |x: f32, y: f32| c + Vec2::new(x * u, y * u);
    let line = |a: Pos2, b: Pos2| {
        p.line_segment([a, b], st);
    };
    let rr = |r: Rect, rad: f32| {
        p.rect_stroke(r, CornerRadius::same(rad as u8), st, egui::StrokeKind::Middle);
    };
    let rect_c = |cx: f32, cy: f32, w: f32, h: f32| Rect::from_center_size(pt(cx, cy), Vec2::new(w * u, h * u));
    match icon {
        Icon::Plus => {
            line(pt(-5.5, 0.0), pt(5.5, 0.0));
            line(pt(0.0, -5.5), pt(0.0, 5.5));
        }
        Icon::Chat => {
            rr(rect_c(0.0, -1.0, 12.0, 9.0), 3.0 * u);
            line(pt(-2.5, 3.5), pt(-4.0, 6.5));
            line(pt(-4.0, 6.5), pt(0.5, 3.5));
        }
        Icon::Brain => {
            p.circle_stroke(pt(-2.5, -1.0), 4.2 * u, st);
            p.circle_stroke(pt(2.5, -1.0), 4.2 * u, st);
            line(pt(0.0, -4.5), pt(0.0, 5.5));
            line(pt(-2.5, 3.0), pt(-1.0, 6.0));
        }
        Icon::People => {
            p.circle_stroke(pt(-2.5, -3.0), 2.3 * u, st);
            p.circle_stroke(pt(3.5, -2.0), 1.9 * u, st);
            p.add(egui::Shape::line(arc(pt(-2.5, 6.0), 4.5 * u, 4.0 * u, std::f32::consts::PI, std::f32::consts::TAU), st));
            p.add(egui::Shape::line(arc(pt(3.8, 6.0), 3.2 * u, 3.4 * u, std::f32::consts::PI * 1.15, std::f32::consts::TAU), st));
        }
        Icon::Chip => {
            rr(rect_c(0.0, 0.0, 9.0, 9.0), 2.0 * u);
            for k in [-2.5f32, 0.0, 2.5] {
                line(pt(k, -4.5), pt(k, -6.5));
                line(pt(k, 4.5), pt(k, 6.5));
                line(pt(-4.5, k), pt(-6.5, k));
                line(pt(4.5, k), pt(6.5, k));
            }
        }
        Icon::Plug => {
            rr(rect_c(0.0, -0.5, 8.0, 6.0), 2.0 * u);
            line(pt(-2.0, -3.5), pt(-2.0, -6.5));
            line(pt(2.0, -3.5), pt(2.0, -6.5));
            line(pt(0.0, 2.5), pt(0.0, 6.5));
        }
        Icon::Book => {
            rr(rect_c(0.0, 0.0, 11.0, 12.0), 1.5 * u);
            line(pt(-2.5, -6.0), pt(-2.5, 6.0));
            line(pt(0.0, -2.5), pt(3.5, -2.5));
        }
        Icon::Puzzle => {
            rr(rect_c(-0.5, 1.0, 10.0, 10.0), 2.0 * u);
            p.circle_stroke(pt(-0.5, -5.0), 1.8 * u, st);
            p.circle_stroke(pt(5.5, 1.0), 1.8 * u, st);
        }
        Icon::Clock => {
            p.circle_stroke(c, 6.2 * u, st);
            line(c, pt(0.0, -3.8));
            line(c, pt(2.8, 1.6));
        }
        Icon::Gear => {
            p.circle_stroke(c, 2.4 * u, st);
            p.circle_stroke(c, 5.2 * u, st);
            for i in 0..8 {
                let a = i as f32 * std::f32::consts::TAU / 8.0;
                line(c + Vec2::angled(a) * 5.2 * u, c + Vec2::angled(a) * 7.0 * u);
            }
        }
        Icon::Search => {
            p.circle_stroke(pt(-1.5, -1.5), 4.5 * u, st);
            line(pt(1.8, 1.8), pt(5.8, 5.8));
        }
        Icon::Send => {
            line(pt(0.0, 5.5), pt(0.0, -5.5));
            line(pt(0.0, -5.5), pt(-4.5, -1.0));
            line(pt(0.0, -5.5), pt(4.5, -1.0));
        }
        Icon::Stop => {
            p.rect_filled(rect_c(0.0, 0.0, 8.0, 8.0), CornerRadius::same((1.5 * u) as u8), color);
        }
        Icon::Copy => {
            rr(rect_c(1.5, 1.5, 8.0, 9.0), 1.5 * u);
            p.add(egui::Shape::line(vec![pt(-2.0, 3.5), pt(-4.5, 3.5), pt(-4.5, -5.5), pt(2.5, -5.5), pt(2.5, -3.5)], st));
        }
        Icon::Undo => {
            // sola dönen ok
            p.add(egui::Shape::line(arc(pt(0.5, 1.0), 5.0 * u, 5.0 * u, -std::f32::consts::PI * 0.9, std::f32::consts::PI * 0.55), st));
            line(pt(-4.3, -0.6), pt(-5.8, -4.2));
            line(pt(-4.3, -0.6), pt(-0.6, -1.6));
        }
        Icon::Retry => {
            p.add(egui::Shape::line(arc(c, 5.5 * u, 5.5 * u, -0.6, 4.6), st));
            line(pt(4.6, -3.0), pt(5.3, 0.6));
            line(pt(4.6, -3.0), pt(1.4, -2.0));
        }
        Icon::Clip => {
            p.add(egui::Shape::line(vec![pt(3.5, -2.0), pt(-2.0, 3.5), pt(-3.5, 2.0), pt(2.5, -4.0), pt(4.5, -2.0), pt(-1.5, 4.5), pt(-4.5, 5.0)], st));
        }
        Icon::Sidebar => {
            rr(rect_c(0.0, 0.0, 13.0, 11.0), 2.0 * u);
            line(pt(-2.0, -5.5), pt(-2.0, 5.5));
        }
        Icon::Team => {
            p.circle_stroke(pt(0.0, -2.5), 3.0 * u, st);
            p.add(egui::Shape::line(arc(pt(0.0, 6.5), 5.5 * u, 4.5 * u, std::f32::consts::PI, std::f32::consts::TAU), st));
            p.circle_filled(pt(-1.0, -3.0), 0.7 * u, color);
            p.circle_filled(pt(1.0, -3.0), 0.7 * u, color);
        }
        Icon::Close => {
            line(pt(-4.5, -4.5), pt(4.5, 4.5));
            line(pt(-4.5, 4.5), pt(4.5, -4.5));
        }
        Icon::Trash => {
            rr(rect_c(0.0, 1.5, 8.0, 9.0), 1.5 * u);
            line(pt(-5.5, -3.5), pt(5.5, -3.5));
            line(pt(-1.5, -5.5), pt(1.5, -5.5));
        }
        Icon::Download => {
            line(pt(0.0, -6.0), pt(0.0, 3.0));
            line(pt(0.0, 3.0), pt(-3.5, -0.5));
            line(pt(0.0, 3.0), pt(3.5, -0.5));
            line(pt(-5.5, 6.0), pt(5.5, 6.0));
        }
        Icon::Edit => {
            line(pt(-5.0, 5.0), pt(4.0, -4.0));
            line(pt(-5.0, 5.0), pt(-5.5, 5.5));
            line(pt(2.5, -5.5), pt(5.5, -2.5));
        }
        Icon::Warn => {
            p.add(egui::Shape::closed_line(vec![pt(0.0, -6.0), pt(6.5, 5.5), pt(-6.5, 5.5)], st));
            line(pt(0.0, -2.0), pt(0.0, 1.8));
            p.circle_filled(pt(0.0, 3.8), 0.8 * u, color);
        }
    }
}

fn arc(c: Pos2, rx: f32, ry: f32, a0: f32, a1: f32) -> Vec<Pos2> {
    (0..=16).map(|i| {
        let a = a0 + (a1 - a0) * i as f32 / 16.0;
        c + Vec2::new(a.cos() * rx, a.sin() * ry)
    }).collect()
}

/// Kenarlıksız ikon butonu (üzerine gelince arka plan)
pub fn icon_button(ui: &mut egui::Ui, icon: Icon, tip: &str, p: &Palette) -> Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(30.0), Sense::click());
    if resp.hovered() {
        ui.painter().rect_filled(rect, CornerRadius::same(7), p.hover);
    }
    draw_icon(ui.painter(), rect.shrink(7.0), icon, if resp.hovered() { p.text } else { p.muted });
    resp.on_hover_text(tip)
}

/// Kenar çubuğu satırı: ikon + yazı, seçiliyken/üzerindeyken arka plan
pub fn nav_item(ui: &mut egui::Ui, icon: Icon, label: &str, selected: bool, collapsed: bool, p: &Palette) -> Response {
    let w = if collapsed { 40.0 } else { ui.available_width() };
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(w, 34.0), Sense::click());
    if selected {
        ui.painter().rect_filled(rect, CornerRadius::same(8), p.surface2);
    } else if resp.hovered() {
        ui.painter().rect_filled(rect, CornerRadius::same(8), p.hover);
    }
    let ir = Rect::from_center_size(Pos2::new(rect.left() + 20.0, rect.center().y), Vec2::splat(17.0));
    let col = if selected { p.text } else { p.muted };
    draw_icon(ui.painter(), ir, icon, col);
    if !collapsed {
        ui.painter().text(
            Pos2::new(rect.left() + 38.0, rect.center().y),
            egui::Align2::LEFT_CENTER,
            label,
            FontId::proportional(14.5),
            if selected { p.text } else { p.muted.lerp_to_gamma(p.text, 0.35) },
        );
    }
    if collapsed { resp.on_hover_text(label) } else { resp }
}

/// Belirgin (dolu) buton
pub fn primary_button(ui: &mut egui::Ui, text: &str, p: &Palette) -> Response {
    ui.add(
        egui::Button::new(egui::RichText::new(text).color(p.on_accent).size(14.0))
            .fill(p.accent)
            .corner_radius(8)
            .min_size(Vec2::new(0.0, 32.0)),
    )
}

/// Sade (çerçeveli) buton
pub fn ghost_button(ui: &mut egui::Ui, text: &str, p: &Palette) -> Response {
    ui.add(
        egui::Button::new(egui::RichText::new(text).color(p.text).size(13.5))
            .fill(p.surface2)
            .stroke(Stroke::new(1.0, p.border))
            .corner_radius(8)
            .min_size(Vec2::new(0.0, 28.0)),
    )
}

/// İkonlu sade buton
pub fn icon_text_button(ui: &mut egui::Ui, icon: Icon, text: &str, p: &Palette) -> Response {
    let font = FontId::proportional(13.5);
    let galley = ui.painter().layout_no_wrap(text.to_string(), font.clone(), p.text);
    let size = Vec2::new(galley.size().x + 40.0, 30.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let fill = if resp.hovered() { p.hover } else { p.surface2 };
    ui.painter().rect(rect, CornerRadius::same(8), fill, Stroke::new(1.0, p.border), egui::StrokeKind::Inside);
    draw_icon(ui.painter(), Rect::from_center_size(Pos2::new(rect.left() + 16.0, rect.center().y), Vec2::splat(14.0)), icon, p.text);
    ui.painter().galley(Pos2::new(rect.left() + 30.0, rect.center().y - galley.size().y / 2.0), galley, p.text);
    resp
}
