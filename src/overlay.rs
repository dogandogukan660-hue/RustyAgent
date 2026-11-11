//! Ekran üstü sanal imleçler.
//!
//! Her agent'ın kendi renkli, isim etiketli imleci ekranda görünür: nereye
//! gittiği, nereye tıkladığı (dalga efekti) ve ne yazdığı. Katman şeffaf,
//! her zaman üstte ve tıklamaları alttaki pencerelere geçirir (kullanıcıyı engellemez).
//!
//! Katman ayrı bir süreçtir (`rusty-agent overlay`): ana uygulama olayları
//! stdin üzerinden JSON satırları olarak gönderir. Böylece pencere sistemi
//! ana süreçten bağımsız çalışır ve bir hata ana uygulamayı etkilemez.

use eframe::egui;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

// ------------------------------------------------------------------ istemci (ana süreç)

struct Client {
    stdin: Option<std::process::ChildStdin>,
    _child: Option<std::process::Child>,
    failed: bool,
}

fn client() -> &'static Mutex<Client> {
    static C: OnceLock<Mutex<Client>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(Client { stdin: None, _child: None, failed: false }))
}

fn enabled() -> bool {
    static E: OnceLock<bool> = OnceLock::new();
    *E.get_or_init(|| crate::config::Config::load().map(|c| c.cursor_overlay).unwrap_or(false))
}

/// Bir imleç olayı gönderir. x/y: fiziksel ekran pikseli.
/// kind: "move" | "click" | "right_click" | "press" | "release" | "drag" |
///       "type" (text=yazılan) | "key" (text=kısayol) | "scroll" | "hide"
pub fn emit(agent: &str, x: f64, y: f64, kind: &str, text: Option<&str>) {
    if !enabled() {
        return;
    }
    let mut c = client().lock().unwrap();
    if c.failed {
        return;
    }
    // Katman hiç açılmadıysa gizleme olayı için açma
    if c.stdin.is_none() && kind == "hide" {
        return;
    }
    if c.stdin.is_none() {
        let exe = match std::env::current_exe() {
            Ok(e) => e,
            Err(_) => {
                c.failed = true;
                return;
            }
        };
        let mut cmd = std::process::Command::new(exe);
        cmd.arg("overlay")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        // Katman için konsol penceresi açılmasın
        crate::hide_console_std(&mut cmd);
        match cmd.spawn() {
            Ok(mut child) => {
                c.stdin = child.stdin.take();
                c._child = Some(child);
            }
            Err(_) => {
                c.failed = true;
                return;
            }
        }
    }
    let line = json!({"agent": agent, "x": x, "y": y, "kind": kind, "text": text}).to_string();
    let ok = c.stdin.as_mut().map(|s| writeln!(s, "{line}").and_then(|_| s.flush()).is_ok()).unwrap_or(false);
    if !ok {
        // Katman kapatıldıysa bir sonraki olayda yeniden başlat
        c.stdin = None;
        c._child = None;
    }
}

pub fn hide(agent: &str) {
    emit(agent, 0.0, 0.0, "hide", None);
}

/// Agent adından sabit, ayırt edilebilir bir renk üretir
pub fn color_for(agent: &str) -> [u8; 3] {
    const PALETTE: [[u8; 3]; 8] = [
        [239, 68, 68],   // kırmızı
        [59, 130, 246],  // mavi
        [16, 185, 129],  // yeşil
        [245, 158, 11],  // turuncu
        [168, 85, 247],  // mor
        [236, 72, 153],  // pembe
        [20, 184, 166],  // turkuaz
        [234, 179, 8],   // sarı
    ];
    let h = agent.bytes().fold(5381u32, |h, b| h.wrapping_mul(33) ^ b as u32);
    PALETTE[(h as usize) % PALETTE.len()]
}

// ------------------------------------------------------------------ katman penceresi (ayrı süreç)

struct Cursor {
    target: egui::Pos2,
    pos: egui::Pos2,
    color: egui::Color32,
    last_active: Instant,
    /// (konum, zaman, sağ tık mı)
    ripples: Vec<(egui::Pos2, Instant, bool)>,
    /// Etiketin yanında kısa süre görünen not (yazılan metin, basılan tuş...)
    text: Option<(String, Instant)>,
    /// Mouse tuşu basılı mı (sürükleme)
    pressed: bool,
    /// Sürükleme izi
    trail: Vec<(egui::Pos2, Instant)>,
    visible: bool,
}

struct OverlayApp {
    events: Arc<Mutex<Vec<Value>>>,
    cursors: HashMap<String, Cursor>,
    /// Monitörün fiziksel sol-üst köşesi
    origin: (f64, f64),
}

impl OverlayApp {
    fn apply_events(&mut self, ppp: f32) {
        let evs: Vec<Value> = std::mem::take(&mut *self.events.lock().unwrap());
        for e in evs {
            let agent = e["agent"].as_str().unwrap_or("?").to_string();
            let kind = e["kind"].as_str().unwrap_or("move");
            if kind == "quit" {
                std::process::exit(0);
            }
            let p = egui::pos2(
                ((e["x"].as_f64().unwrap_or(0.0) - self.origin.0) / ppp as f64) as f32,
                ((e["y"].as_f64().unwrap_or(0.0) - self.origin.1) / ppp as f64) as f32,
            );
            let [r, g, b] = color_for(&agent);
            let c = self.cursors.entry(agent).or_insert_with(|| Cursor {
                target: p,
                pos: p,
                color: egui::Color32::from_rgb(r, g, b),
                last_active: Instant::now(),
                ripples: vec![],
                text: None,
                pressed: false,
                trail: vec![],
                visible: true,
            });
            c.last_active = Instant::now();
            let now = Instant::now();
            match kind {
                "hide" => {
                    c.visible = false;
                    c.pressed = false;
                    c.trail.clear();
                }
                _ => {
                    if !c.visible {
                        c.pos = p; // yeniden görününce ışınlan
                    }
                    c.visible = true;
                    c.target = p;
                    match kind {
                        "click" => c.ripples.push((p, now, false)),
                        "right_click" => {
                            c.ripples.push((p, now, true));
                            c.text = Some(("sağ tık".into(), now));
                        }
                        "press" => {
                            c.pressed = true;
                            c.trail.clear();
                            c.trail.push((p, now));
                        }
                        "drag" => {
                            c.pressed = true;
                            c.trail.push((p, now));
                        }
                        "release" => {
                            c.pressed = false;
                            c.ripples.push((p, now, false));
                        }
                        "type" => {
                            let t: String = e["text"].as_str().unwrap_or("").chars().take(40).collect();
                            c.text = Some((format!("yazıyor: {t}"), now));
                        }
                        "key" => c.text = Some((format!("tuş: {}", e["text"].as_str().unwrap_or("")), now)),
                        "scroll" => c.text = Some((format!("kaydırıyor {}", e["text"].as_str().unwrap_or("")), now)),
                        _ => {}
                    }
                }
            }
        }
    }
}

impl eframe::App for OverlayApp {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0, 0.0, 0.0, 0.0]
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let ppp = ctx.pixels_per_point();
        self.apply_events(ppp);
        let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, ui.scope_id().with("ra-cursors")));
        let now = Instant::now();
        let mut animating = false;

        for (name, c) in self.cursors.iter_mut() {
            if !c.visible {
                continue;
            }
            let idle = now.duration_since(c.last_active).as_secs_f32();
            // 8 sn hareketsizlikten sonra solarak kaybol
            let alpha = if idle < 8.0 { 1.0 } else { (1.0 - (idle - 8.0) / 2.0).max(0.0) };
            if alpha <= 0.0 {
                c.visible = false;
                continue;
            }
            // Yumuşak hareket (insan gibi kayarak gider)
            let d = c.target - c.pos;
            if d.length() > 0.5 {
                c.pos += d * 0.25;
                animating = true;
            } else {
                c.pos = c.target;
            }
            let col = c.color.gamma_multiply(alpha);
            let white = egui::Color32::WHITE.gamma_multiply(alpha);

            // Sürükleme izi (2 sn içinde solar)
            c.trail.retain(|(_, t)| now.duration_since(*t) < Duration::from_secs(2));
            for w in c.trail.windows(2) {
                let fade = 1.0 - now.duration_since(w[1].1).as_secs_f32() / 2.0;
                painter.line_segment([w[0].0, w[1].0], egui::Stroke::new(4.0, c.color.gamma_multiply(0.6 * fade * alpha)));
            }
            if !c.trail.is_empty() {
                animating = true;
            }

            // Tıklama dalgaları: sol tık tek halka, sağ tık çift kesik halka
            c.ripples.retain(|(_, t, _)| now.duration_since(*t) < Duration::from_millis(600));
            for (p, t, right) in &c.ripples {
                let k = now.duration_since(*t).as_secs_f32() / 0.6;
                let stroke = egui::Stroke::new(3.0 * (1.0 - k), c.color.gamma_multiply(1.0 - k));
                painter.circle_stroke(*p, 6.0 + 22.0 * k, stroke);
                if *right {
                    painter.circle_stroke(*p, 3.0 + 12.0 * k, stroke);
                }
                animating = true;
            }

            // Mouse tuşu basılıyken imleç ucunda dolu nokta
            if c.pressed {
                painter.circle_filled(c.pos, 7.0, c.color.gamma_multiply(0.8 * alpha));
            }

            // Ok biçimli imleç
            let o = c.pos;
            let pts: Vec<egui::Pos2> = [(0.0, 0.0), (0.0, 18.0), (4.5, 14.0), (8.0, 21.5), (11.0, 20.0), (7.5, 12.8), (13.0, 12.8)]
                .iter()
                .map(|(x, y)| o + egui::vec2(*x, *y) * 1.25)
                .collect();
            painter.add(egui::Shape::convex_polygon(pts, col, egui::Stroke::new(1.5, white)));

            // İsim etiketi (+ yazılan metin)
            let label = match &c.text {
                Some((t, at)) if now.duration_since(*at) < Duration::from_secs(3) => format!("{name}: {t}"),
                _ => name.clone(),
            };
            let galley = painter.layout_no_wrap(label, egui::FontId::proportional(13.0), white);
            let rect = egui::Rect::from_min_size(o + egui::vec2(18.0, 22.0), galley.size() + egui::vec2(12.0, 6.0));
            painter.rect_filled(rect, 6.0, col);
            painter.galley(rect.min + egui::vec2(6.0, 3.0), galley, white);
            if idle >= 8.0 {
                animating = true;
            }
        }

        if animating {
            ctx.request_repaint();
        } else {
            ctx.request_repaint_after(Duration::from_millis(500));
        }
    }
}

/// `rusty-agent overlay` alt komutu: katman penceresini çalıştırır
pub fn run_overlay() -> anyhow::Result<()> {
    // Birincil monitörü kapla
    let mons = xcap::Monitor::all()?;
    let m = mons
        .iter()
        .find(|m| m.is_primary().unwrap_or(false))
        .cloned()
        .or_else(|| mons.into_iter().next())
        .ok_or_else(|| anyhow::anyhow!("monitör yok"))?;
    let scale = m.scale_factor().unwrap_or(1.0) as f64;
    let (mx, my, mw, mh) = (m.x()? as f64, m.y()? as f64, m.width()? as f64, m.height()? as f64);

    let events: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
    let ev2 = events.clone();
    let ctx_slot: Arc<Mutex<Option<egui::Context>>> = Arc::new(Mutex::new(None));
    let ctx2 = ctx_slot.clone();
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines() {
            let Ok(line) = line else { break };
            // Bazı kabuklar akışın başına UTF-8 BOM ekler
            if let Ok(v) = serde_json::from_str::<Value>(line.trim_start_matches('\u{feff}')) {
                ev2.lock().unwrap().push(v);
                if let Some(c) = ctx2.lock().unwrap().as_ref() {
                    c.request_repaint();
                }
            }
        }
        // Ana uygulama kapandı → katmanı kapat
        ev2.lock().unwrap().push(json!({"kind": "quit"}));
        if let Some(c) = ctx2.lock().unwrap().as_ref() {
            c.request_repaint();
        }
    });

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("RustyAgent imleçleri")
            .with_transparent(true)
            .with_decorations(false)
            .with_always_on_top()
            .with_mouse_passthrough(true)
            .with_taskbar(false)
            .with_resizable(false)
            .with_active(false)
            .with_position([(mx / scale) as f32, (my / scale) as f32])
            .with_inner_size([(mw / scale) as f32, (mh / scale) as f32]),
        ..Default::default()
    };
    eframe::run_native(
        "rusty-agent-overlay",
        options,
        Box::new(move |cc| {
            *ctx_slot.lock().unwrap() = Some(cc.egui_ctx.clone());
            Ok(Box::new(OverlayApp { events, cursors: HashMap::new(), origin: (mx, my) }))
        }),
    )
    .map_err(|e| anyhow::anyhow!("katman açılamadı: {e}"))
}
