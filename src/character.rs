//! Agent karakterleri — kodla çizilen, animasyonlu, özgün vektör karakterler.
//!
//! Her agent'ın bir karakter türü, rengi ve kişiliği vardır (agent dosyasında
//! `character`, `color`, `persona`). Karakter, agent'ın o anki durumuna göre
//! canlanır: boşta göz kırpar, düşünürken yukarı bakar, araç kullanırken
//! çalışır, konuşurken ağzı oynar, onay beklerken endişelenir, iş bitince sevinir.

use eframe::egui::{self, Color32, Painter, Pos2, Rect, Shape, Stroke, Vec2};
use std::f32::consts::{PI, TAU};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    Robot,
    Owl,
    Fox,
    Cat,
    Slime,
    Ghost,
}

impl Kind {
    pub const ALL: [Kind; 6] = [Kind::Robot, Kind::Owl, Kind::Fox, Kind::Cat, Kind::Slime, Kind::Ghost];

    pub fn id(&self) -> &'static str {
        match self {
            Kind::Robot => "robot",
            Kind::Owl => "owl",
            Kind::Fox => "fox",
            Kind::Cat => "cat",
            Kind::Slime => "slime",
            Kind::Ghost => "ghost",
        }
    }
    pub fn label(&self) -> &'static str {
        match self {
            Kind::Robot => "Robot",
            Kind::Owl => "Baykuş",
            Kind::Fox => "Tilki",
            Kind::Cat => "Kedi",
            Kind::Slime => "Jöle",
            Kind::Ghost => "Hayalet",
        }
    }
    pub fn parse(s: &str) -> Option<Kind> {
        Kind::ALL.into_iter().find(|k| k.id() == s.to_lowercase() || k.label().to_lowercase() == s.to_lowercase())
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Mood {
    Idle,
    Thinking,
    Working,
    Talking,
    /// Kullanıcı onayı bekliyor
    Waiting,
    Happy,
    Error,
    /// Uzun süredir boşta (uyuyor)
    Sleeping,
}

impl Mood {
    pub fn label(&self) -> &'static str {
        match self {
            Mood::Idle => "hazır",
            Mood::Thinking => "düşünüyor",
            Mood::Working => "çalışıyor",
            Mood::Talking => "konuşuyor",
            Mood::Waiting => "onay bekliyor",
            Mood::Happy => "bitti!",
            Mood::Error => "sorun var",
            Mood::Sleeping => "uyuyor",
        }
    }
}

/// Bir agent'ın görünümü
#[derive(Debug, Clone)]
pub struct Look {
    pub kind: Kind,
    pub color: Color32,
}

fn hash(s: &str) -> u32 {
    s.bytes().fold(2166136261u32, |h, b| (h ^ b as u32).wrapping_mul(16777619))
}

/// Ad için kararlı varsayılan görünüm (agent dosyasında belirtilmemişse)
pub fn default_look(name: &str) -> Look {
    const COLORS: [[u8; 3]; 8] = [
        [249, 115, 22], [59, 130, 246], [16, 185, 129], [168, 85, 247],
        [236, 72, 153], [234, 179, 8], [20, 184, 166], [239, 68, 68],
    ];
    let h = hash(name);
    let [r, g, b] = COLORS[(h as usize / 7) % COLORS.len()];
    Look { kind: Kind::ALL[h as usize % Kind::ALL.len()], color: Color32::from_rgb(r, g, b) }
}

pub fn parse_color(s: &str) -> Option<Color32> {
    let s = s.trim().trim_start_matches('#');
    if s.len() != 6 {
        return None;
    }
    let v = u32::from_str_radix(s, 16).ok()?;
    Some(Color32::from_rgb((v >> 16) as u8, (v >> 8) as u8, v as u8))
}

pub fn color_hex(c: Color32) -> String {
    format!("#{:02x}{:02x}{:02x}", c.r(), c.g(), c.b())
}

fn darker(c: Color32, k: f32) -> Color32 {
    Color32::from_rgb((c.r() as f32 * k) as u8, (c.g() as f32 * k) as u8, (c.b() as f32 * k) as u8)
}
fn lighter(c: Color32, k: f32) -> Color32 {
    let l = |v: u8| (v as f32 + (255.0 - v as f32) * k) as u8;
    Color32::from_rgb(l(c.r()), l(c.g()), l(c.b()))
}

fn arc(c: Pos2, rx: f32, ry: f32, a0: f32, a1: f32, n: usize) -> Vec<Pos2> {
    (0..=n)
        .map(|i| {
            let a = a0 + (a1 - a0) * i as f32 / n as f32;
            c + Vec2::new(a.cos() * rx, a.sin() * ry)
        })
        .collect()
}

fn ellipse(c: Pos2, rx: f32, ry: f32) -> Vec<Pos2> {
    let mut p = arc(c, rx, ry, 0.0, TAU, 40);
    p.pop();
    p
}

/// Karakteri `rect` içine çizer. `t` = saniye cinsinden zaman (animasyon için).
pub fn draw(p: &Painter, rect: Rect, look: &Look, mood: Mood, t: f32) {
    let s = rect.width().min(rect.height());
    let u = s / 100.0; // birim
    // Duruma göre zıplama/salınım
    let bob = match mood {
        Mood::Happy => -((t * 9.0).sin().abs()) * 8.0 * u,
        Mood::Working => (t * 10.0).sin() * 1.5 * u,
        Mood::Sleeping => (t * 1.2).sin() * 1.0 * u,
        _ => (t * 2.2).sin() * 2.5 * u,
    };
    let float = if look.kind == Kind::Ghost { (t * 1.6).sin() * 4.0 * u } else { 0.0 };
    let c = rect.center() + Vec2::new(0.0, bob + float + 6.0 * u);
    let col = look.color;
    let outline = Stroke::new(2.2 * u.max(0.5), darker(col, 0.55));

    // Gölge
    p.add(Shape::convex_polygon(ellipse(rect.center() + Vec2::new(0.0, 44.0 * u), 26.0 * u - bob.abs() * 0.5, 5.0 * u), Color32::from_black_alpha(50), Stroke::NONE));

    // Gövde + türe özgü parçalar; yüz merkezi ve göz aralığı döner
    let (face, eye_dx, eye_r) = match look.kind {
        Kind::Robot => {
            // anten
            let top = c + Vec2::new(0.0, -36.0 * u);
            p.line_segment([top, top + Vec2::new(0.0, -12.0 * u)], outline);
            let glow = if mood == Mood::Working || mood == Mood::Thinking { 0.5 + 0.5 * (t * 8.0).sin() } else { 0.0 };
            p.circle_filled(top + Vec2::new(0.0, -14.0 * u), 4.5 * u, lighter(Color32::from_rgb(250, 204, 21), glow * 0.6));
            // kulak cıvataları
            for sx in [-1.0, 1.0] {
                p.rect_filled(Rect::from_center_size(c + Vec2::new(sx * 37.0 * u, -6.0 * u), Vec2::new(8.0 * u, 16.0 * u)), 2.0 * u, darker(col, 0.7));
            }
            // kafa
            let head = Rect::from_center_size(c + Vec2::new(0.0, -6.0 * u), Vec2::new(68.0 * u, 58.0 * u));
            p.rect(head, 14.0 * u, col, outline, egui::StrokeKind::Middle);
            // ekran-yüz
            let screen = head.shrink(8.0 * u);
            p.rect_filled(screen, 9.0 * u, Color32::from_rgb(22, 28, 40));
            // gövde
            let body = Rect::from_center_size(c + Vec2::new(0.0, 32.0 * u), Vec2::new(40.0 * u, 18.0 * u));
            p.rect(body, 6.0 * u, darker(col, 0.85), outline, egui::StrokeKind::Middle);
            (screen.center(), 13.0 * u, 6.5 * u)
        }
        Kind::Owl => {
            let body = c + Vec2::new(0.0, 0.0);
            // kulak tüyleri
            for sx in [-1.0f32, 1.0] {
                p.add(Shape::convex_polygon(
                    vec![body + Vec2::new(sx * 20.0 * u, -30.0 * u), body + Vec2::new(sx * 34.0 * u, -48.0 * u), body + Vec2::new(sx * 36.0 * u, -22.0 * u)],
                    darker(col, 0.8),
                    outline,
                ));
            }
            p.add(Shape::convex_polygon(ellipse(body, 38.0 * u, 42.0 * u), col, outline));
            // göğüs
            p.add(Shape::convex_polygon(ellipse(body + Vec2::new(0.0, 16.0 * u), 22.0 * u, 20.0 * u), lighter(col, 0.45), Stroke::NONE));
            // kanatlar (çalışırken çırpar)
            let flap = if mood == Mood::Working { (t * 14.0).sin() * 6.0 * u } else { 0.0 };
            for sx in [-1.0f32, 1.0] {
                p.add(Shape::convex_polygon(ellipse(body + Vec2::new(sx * 36.0 * u, 6.0 * u - flap * sx), 8.0 * u, 18.0 * u), darker(col, 0.75), outline));
            }
            // büyük göz halkaları
            for sx in [-1.0f32, 1.0] {
                p.circle(body + Vec2::new(sx * 15.0 * u, -12.0 * u), 13.0 * u, Color32::WHITE, outline);
            }
            (body + Vec2::new(0.0, -12.0 * u), 15.0 * u, 6.5 * u)
        }
        Kind::Fox | Kind::Cat => {
            let fox = look.kind == Kind::Fox;
            let head = c + Vec2::new(0.0, -2.0 * u);
            // kuyruk (sallanır)
            let wag = (t * if mood == Mood::Happy { 12.0 } else { 3.0 }).sin() * 0.35;
            let tail_base = c + Vec2::new(26.0 * u, 34.0 * u);
            let tail_tip = tail_base + Vec2::new((0.9 + wag).cos() * 30.0 * u, -(0.9 + wag).sin() * 30.0 * u);
            p.line_segment([tail_base, tail_tip], Stroke::new(if fox { 13.0 } else { 7.0 } * u, darker(col, 0.85)));
            if fox {
                p.circle_filled(tail_tip, 6.5 * u, Color32::WHITE);
            }
            // gövde
            p.add(Shape::convex_polygon(ellipse(c + Vec2::new(0.0, 34.0 * u), 22.0 * u, 13.0 * u), darker(col, 0.9), outline));
            // kulaklar
            let (ew, eh) = if fox { (16.0, 34.0) } else { (13.0, 24.0) };
            let twitch = if (t % 5.0) < 0.15 { 3.0 * u } else { 0.0 };
            for sx in [-1.0f32, 1.0] {
                let base = head + Vec2::new(sx * 20.0 * u, -16.0 * u);
                let pts = vec![base + Vec2::new(-ew * 0.6 * u, 6.0 * u), base + Vec2::new(sx * 6.0 * u, -eh * u + twitch), base + Vec2::new(ew * 0.6 * u, 6.0 * u)];
                p.add(Shape::convex_polygon(pts.clone(), col, outline));
                let inner: Vec<Pos2> = pts.iter().map(|q| base + (*q - base) * 0.55 + Vec2::new(0.0, 2.0 * u)).collect();
                p.add(Shape::convex_polygon(inner, Color32::from_rgb(255, 205, 210), Stroke::NONE));
            }
            p.add(Shape::convex_polygon(ellipse(head, 34.0 * u, 30.0 * u), col, outline));
            // ağız çevresi
            let muzzle = head + Vec2::new(0.0, 12.0 * u);
            p.add(Shape::convex_polygon(ellipse(muzzle, if fox { 18.0 } else { 14.0 } * u, 11.0 * u), Color32::from_rgb(255, 248, 240), Stroke::NONE));
            p.circle_filled(muzzle + Vec2::new(0.0, -5.0 * u), 3.2 * u, Color32::from_rgb(40, 30, 30));
            if !fox {
                for sx in [-1.0f32, 1.0] {
                    for dy in [-2.0f32, 3.0] {
                        p.line_segment(
                            [muzzle + Vec2::new(sx * 12.0 * u, dy * u), muzzle + Vec2::new(sx * 30.0 * u, dy * 1.8 * u)],
                            Stroke::new(1.2 * u, darker(col, 0.5)),
                        );
                    }
                }
            }
            (head + Vec2::new(0.0, -4.0 * u), 13.0 * u, 5.5 * u)
        }
        Kind::Slime => {
            // titreşen jöle gövde
            let n = 48;
            let wob = if mood == Mood::Working { 3.5 } else { 1.8 };
            let pts: Vec<Pos2> = (0..n)
                .map(|i| {
                    let a = i as f32 / n as f32 * TAU;
                    let r = 38.0 * u + (a * 3.0 + t * 3.0).sin() * wob * u;
                    let squash = if a.sin() > 0.0 { 0.78 } else { 1.0 };
                    c + Vec2::new(a.cos() * r * 1.08, a.sin() * r * squash + 6.0 * u)
                })
                .collect();
            p.add(Shape::closed_line(pts.clone(), outline));
            p.add(Shape::convex_polygon(pts, col.gamma_multiply(0.92), Stroke::NONE));
            p.add(Shape::convex_polygon(ellipse(c + Vec2::new(-14.0 * u, -16.0 * u), 8.0 * u, 5.0 * u), Color32::from_white_alpha(120), Stroke::NONE));
            (c + Vec2::new(0.0, -2.0 * u), 13.0 * u, 6.0 * u)
        }
        Kind::Ghost => {
            let mut pts = arc(c + Vec2::new(0.0, -6.0 * u), 34.0 * u, 34.0 * u, PI, TAU, 24);
            // dalgalı etek
            let bottom = c.y + 34.0 * u;
            for i in 0..=8 {
                let x = c.x + 34.0 * u - i as f32 * 8.5 * u;
                let y = bottom + if i % 2 == 0 { 0.0 } else { -7.0 * u } + (t * 4.0 + i as f32).sin() * 2.0 * u;
                pts.push(Pos2::new(x, y));
            }
            let body = Color32::from_rgba_unmultiplied(lighter(col, 0.65).r(), lighter(col, 0.65).g(), lighter(col, 0.65).b(), 235);
            p.add(Shape::Path(egui::epaint::PathShape::closed_line(pts.clone(), outline)));
            p.add(Shape::Path(egui::epaint::PathShape { points: pts, closed: true, fill: body, stroke: Stroke::NONE.into() }));
            (c + Vec2::new(0.0, -6.0 * u), 13.0 * u, 6.0 * u)
        }
    };

    draw_face(p, face, eye_dx, eye_r, u, look, mood, t);
    draw_mood_extras(p, rect, face, u, look, mood, t);
}

#[allow(clippy::too_many_arguments)]
fn draw_face(p: &Painter, face: Pos2, eye_dx: f32, eye_r: f32, u: f32, look: &Look, mood: Mood, t: f32) {
    let robot = look.kind == Kind::Robot;
    let eye_col = if robot { Color32::from_rgb(94, 234, 212) } else { Color32::from_rgb(30, 30, 40) };
    let blink = (t % 3.7) < 0.13;
    // Bakış yönü
    let look_off = match mood {
        Mood::Thinking => Vec2::new(3.0 * u, -3.5 * u),
        Mood::Working => Vec2::new((t * 2.5).sin() * 3.0 * u, 1.5 * u),
        _ => Vec2::new((t * 0.7).sin() * 1.5 * u, 0.0),
    };
    for sx in [-1.0f32, 1.0] {
        let e = face + Vec2::new(sx * eye_dx, 0.0);
        match mood {
            Mood::Happy => {
                // ^^ gözler
                p.add(Shape::line(arc(e + Vec2::new(0.0, 2.0 * u), eye_r, eye_r, PI * 1.1, PI * 1.9, 10), Stroke::new(2.6 * u, eye_col)));
            }
            Mood::Sleeping => {
                p.add(Shape::line(arc(e, eye_r, eye_r * 0.6, PI * 0.1, PI * 0.9, 10), Stroke::new(2.4 * u, eye_col)));
            }
            Mood::Error => {
                let d = eye_r * 0.8;
                p.line_segment([e + Vec2::new(-d, -d), e + Vec2::new(d, d)], Stroke::new(2.6 * u, eye_col));
                p.line_segment([e + Vec2::new(-d, d), e + Vec2::new(d, -d)], Stroke::new(2.6 * u, eye_col));
            }
            _ if blink => {
                p.line_segment([e + Vec2::new(-eye_r, 0.0), e + Vec2::new(eye_r, 0.0)], Stroke::new(2.4 * u, eye_col));
            }
            _ => {
                let squint = if mood == Mood::Working { 0.65 } else { 1.0 };
                let r = if mood == Mood::Waiting { eye_r * 1.15 } else { eye_r };
                p.add(Shape::convex_polygon(ellipse(e + look_off * 0.5, r * 0.85, r * squint), eye_col, Stroke::NONE));
                // parıltı
                p.circle_filled(e + look_off * 0.5 + Vec2::new(-r * 0.3, -r * 0.35 * squint), r * 0.28, Color32::from_white_alpha(220));
            }
        }
        // Kaşlar: endişe / odak
        if matches!(mood, Mood::Waiting | Mood::Working | Mood::Error) {
            let tilt = match mood {
                Mood::Waiting => -0.35, // iç uç yukarı (endişe)
                _ => 0.3,               // iç uç aşağı (odak/kızgın)
            } * sx;
            let b = e + Vec2::new(0.0, -eye_r * 1.9);
            let d = Vec2::new(eye_r, eye_r * tilt);
            p.line_segment([b - d, b + d], Stroke::new(2.2 * u, if robot { eye_col } else { Color32::from_rgb(50, 40, 40) }));
        }
    }
    // Yanaklar
    if !robot && matches!(mood, Mood::Happy | Mood::Talking | Mood::Idle) {
        for sx in [-1.0f32, 1.0] {
            p.circle_filled(face + Vec2::new(sx * (eye_dx + eye_r * 1.3), eye_r * 1.6), eye_r * 0.7, Color32::from_rgba_unmultiplied(255, 120, 140, 70));
        }
    }
    // Ağız
    let m = face + Vec2::new(0.0, eye_r * 2.4);
    let mouth_col = if robot { eye_col } else { Color32::from_rgb(60, 30, 35) };
    if look.kind == Kind::Owl {
        // gaga: konuşurken açılır
        let open = if mood == Mood::Talking { (t * 16.0).sin().abs() * 4.0 * u } else { 0.0 };
        let gaga = Color32::from_rgb(251, 191, 36);
        p.add(Shape::convex_polygon(vec![m + Vec2::new(-6.0 * u, -4.0 * u), m + Vec2::new(6.0 * u, -4.0 * u), m + Vec2::new(0.0, 3.0 * u)], gaga, Stroke::NONE));
        if open > 0.5 {
            p.add(Shape::convex_polygon(vec![m + Vec2::new(-5.0 * u, 3.0 * u), m + Vec2::new(5.0 * u, 3.0 * u), m + Vec2::new(0.0, 4.0 * u + open)], darker(gaga, 0.8), Stroke::NONE));
        }
        return;
    }
    match mood {
        Mood::Talking => {
            let open = 2.0 * u + (t * 15.0).sin().abs() * 5.0 * u + (t * 23.0).sin().abs() * 2.0 * u;
            p.add(Shape::convex_polygon(ellipse(m, 6.0 * u, open), mouth_col, Stroke::NONE));
        }
        Mood::Happy => {
            let pts = arc(m + Vec2::new(0.0, -3.0 * u), 9.0 * u, 7.0 * u, 0.0, PI, 14);
            p.add(Shape::convex_polygon(pts, mouth_col, Stroke::NONE));
        }
        Mood::Error | Mood::Waiting => {
            p.add(Shape::line(arc(m + Vec2::new(0.0, 5.0 * u), 7.0 * u, 4.0 * u, PI * 1.15, PI * 1.85, 10), Stroke::new(2.2 * u, mouth_col)));
        }
        Mood::Thinking => {
            p.line_segment([m + Vec2::new(-3.0 * u, 1.0 * u), m + Vec2::new(5.0 * u, -1.0 * u)], Stroke::new(2.2 * u, mouth_col));
        }
        Mood::Sleeping => {
            p.circle_stroke(m, 2.5 * u, Stroke::new(1.8 * u, mouth_col));
        }
        _ => {
            p.add(Shape::line(arc(m + Vec2::new(0.0, -2.0 * u), 7.0 * u, 4.5 * u, PI * 0.15, PI * 0.85, 10), Stroke::new(2.2 * u, mouth_col)));
        }
    }
}

fn draw_mood_extras(p: &Painter, rect: Rect, face: Pos2, u: f32, look: &Look, mood: Mood, t: f32) {
    let corner = Pos2::new(rect.right() - 16.0 * u, rect.top() + 14.0 * u);
    match mood {
        Mood::Thinking => {
            // düşünce balonu noktaları
            for i in 0..3 {
                let a = ((t * 3.0 - i as f32 * 0.6).sin() * 0.5 + 0.5) * 0.8 + 0.2;
                p.circle_filled(corner + Vec2::new(-(2 - i) as f32 * 9.0 * u, (2 - i) as f32 * 5.0 * u), (2.5 + i as f32) * u, Color32::from_white_alpha((a * 230.0) as u8));
            }
        }
        Mood::Working => {
            // dönen dişli
            let g = corner;
            let rot = t * 3.0;
            let teeth: Vec<Pos2> = (0..16)
                .map(|i| {
                    let a = rot + i as f32 / 16.0 * TAU;
                    let r = if i % 2 == 0 { 10.0 } else { 7.0 } * u;
                    g + Vec2::new(a.cos() * r, a.sin() * r)
                })
                .collect();
            p.add(Shape::convex_polygon(teeth, Color32::from_rgb(203, 213, 225), Stroke::NONE));
            p.circle_filled(g, 3.0 * u, Color32::from_rgb(71, 85, 105));
        }
        Mood::Waiting => {
            let bounce = (t * 6.0).sin().abs() * 3.0 * u;
            p.text(corner + Vec2::new(0.0, -bounce), egui::Align2::CENTER_CENTER, "?", egui::FontId::proportional(22.0 * u), Color32::from_rgb(250, 204, 21));
        }
        Mood::Sleeping => {
            for i in 0..3 {
                let k = ((t * 0.8 + i as f32 * 0.33) % 1.0) as f32;
                p.text(
                    face + Vec2::new(22.0 * u + k * 14.0 * u, -20.0 * u - k * 22.0 * u),
                    egui::Align2::CENTER_CENTER,
                    "z",
                    egui::FontId::proportional((9.0 + k * 8.0) * u),
                    Color32::from_white_alpha(((1.0 - k) * 200.0) as u8),
                );
            }
        }
        Mood::Happy => {
            for i in 0..5 {
                let a = t * 2.0 + i as f32 * TAU / 5.0;
                let q = rect.center() + Vec2::new(a.cos() * 46.0 * u, a.sin() * 40.0 * u - 6.0 * u);
                p.circle_filled(q, 2.2 * u, lighter(look.color, 0.5));
            }
        }
        Mood::Error => {
            p.text(corner, egui::Align2::CENTER_CENTER, "!", egui::FontId::proportional(22.0 * u), Color32::from_rgb(248, 113, 113));
        }
        _ => {}
    }
}
