//! Bilgisayar kontrolü: ekran görüntüsü + mouse + klavye (Windows/macOS/Linux).
//!
//! Mouse ışınlanmaz: insan gibi yumuşak bir eğriyle hedefe kayar ve agent'ın
//! renkli sanal imleci onunla birlikte görünür. Yazma harf harf yapılır.
//!
//! Not: İşletim sistemlerinde tek bir gerçek imleç vardır. Birden çok agent
//! masaüstünü kullanırken global bir kilit sırayla erişim sağlar; o an kimin
//! kontrol ettiği renkli sanal imleçle gösterilir. Gerçek paralellik için
//! web işlerinde `browser` aracı kullanılır (her agent kendi penceresinde).

use super::{Tool, ToolOutput};
use async_trait::async_trait;
use base64::Engine;
use enigo::{Axis, Button, Coordinate, Direction, Enigo, Key, Keyboard, Mouse, Settings};
use serde_json::{json, Value};
use std::sync::OnceLock;
use std::thread::sleep;
use std::time::Duration;
use tokio::sync::Mutex;

/// Tüm agent'lar arasında paylaşılan ekran kilidi
fn screen_lock() -> &'static Mutex<()> {
    static L: OnceLock<Mutex<()>> = OnceLock::new();
    L.get_or_init(|| Mutex::new(()))
}

/// Ekran görüntüsü bu genişliğe küçültülür; koordinatlar bu ölçekte verilir.
const SHOT_WIDTH: u32 = 1280;

pub struct Computer;

impl Computer {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Tool for Computer {
    fn name(&self) -> String {
        "computer".into()
    }
    fn description(&self) -> String {
        format!(
            "Masaüstünü görür ve gerçek mouse/klavyeyi kullanır (tüm uygulamalar). Önce 'screenshot' al. \
             Koordinatlar ekran görüntüsünün ({SHOT_WIDTH}px genişliğe ölçeklenmiş) piksel uzayındadır. \
             Mouse: move, click (button: left/right/middle), double_click, triple_click, right_click, \
             mouse_down / mouse_up (basılı tut/bırak), drag (x,y → to_x,to_y), scroll (amount: +aşağı/-yukarı, horizontal), cursor_position. \
             Klavye: type (metni harf harf yazar), key (tek tuş veya kombinasyon: 'enter', 'ctrl+c', 'ctrl+shift+esc', 'alt+tab', 'win+r'), \
             hold_key (text tuşunu amount ms basılı tut). Diğer: screenshot, wait (amount ms). \
             Tıklamadan önce hedefin doğru olduğundan emin ol; önemli eylemlerden sonra screenshot ile doğrula."
        )
    }
    fn input_schema(&self) -> Value {
        json!({"type":"object","properties":{
            "action":{"type":"string","enum":["screenshot","move","click","double_click","triple_click","right_click",
                "mouse_down","mouse_up","drag","scroll","cursor_position","type","key","hold_key","wait"]},
            "x":{"type":"integer"},"y":{"type":"integer"},
            "to_x":{"type":"integer"},"to_y":{"type":"integer"},
            "button":{"type":"string","enum":["left","right","middle"]},
            "text":{"type":"string","description":"type: yazılacak metin; key/hold_key: tuş veya kombinasyon"},
            "amount":{"type":"integer","description":"scroll miktarı (tık), wait/hold_key için ms"},
            "horizontal":{"type":"boolean","description":"scroll: yatay kaydır"}
        },"required":["action"],"additionalProperties":false})
    }

    async fn call(&self, input: Value) -> ToolOutput {
        self.call_ctx(input, &super::ToolCtx { agent: "agent".into(), session: "default".into() }).await
    }

    async fn call_ctx(&self, input: Value, ctx: &super::ToolCtx) -> ToolOutput {
        let agent = ctx.agent.clone();
        let _guard = screen_lock().lock().await;
        let action = input["action"].as_str().unwrap_or("").to_string();
        if action == "wait" {
            let ms = input["amount"].as_u64().unwrap_or(1000).min(30_000);
            tokio::time::sleep(Duration::from_millis(ms)).await;
            return ToolOutput::Text("beklendi".into());
        }
        let res = tokio::task::spawn_blocking(move || run(&action, &input, &agent)).await;
        match res {
            Ok(Ok(o)) => o,
            Ok(Err(e)) => ToolOutput::Error(e.to_string()),
            Err(e) => ToolOutput::Error(e.to_string()),
        }
    }
}

fn primary_monitor() -> anyhow::Result<xcap::Monitor> {
    let mons = xcap::Monitor::all()?;
    mons.iter()
        .find(|m| m.is_primary().unwrap_or(false))
        .cloned()
        .or_else(|| mons.into_iter().next())
        .ok_or_else(|| anyhow::anyhow!("monitör bulunamadı"))
}

/// Görüntü koordinatı ↔ gerçek ekran koordinatı dönüşümü
struct Geo {
    f: f64,
    ox: f64,
    oy: f64,
}

impl Geo {
    fn get() -> anyhow::Result<Self> {
        let m = primary_monitor()?;
        Ok(Self { f: m.width()? as f64 / SHOT_WIDTH as f64, ox: m.x()? as f64, oy: m.y()? as f64 })
    }
    fn to_screen(&self, x: i64, y: i64) -> (i32, i32) {
        ((x as f64 * self.f + self.ox).round() as i32, (y as f64 * self.f + self.oy).round() as i32)
    }
    fn to_image(&self, x: i32, y: i32) -> (i64, i64) {
        (((x as f64 - self.ox) / self.f).round() as i64, ((y as f64 - self.oy) / self.f).round() as i64)
    }
}

fn xy(input: &Value, geo: &Geo, kx: &str, ky: &str) -> anyhow::Result<(i32, i32)> {
    let x = input[kx].as_i64().ok_or_else(|| anyhow::anyhow!("{kx} gerekli"))?;
    let y = input[ky].as_i64().ok_or_else(|| anyhow::anyhow!("{ky} gerekli"))?;
    Ok(geo.to_screen(x, y))
}

fn screenshot() -> anyhow::Result<ToolOutput> {
    let m = primary_monitor()?;
    let img = m.capture_image()?;
    let (w, h) = (img.width(), img.height());
    let nh = (h as f64 * SHOT_WIDTH as f64 / w as f64) as u32;
    let small = image::imageops::resize(&img, SHOT_WIDTH, nh, image::imageops::FilterType::Triangle);
    let mut png = Vec::new();
    image::DynamicImage::ImageRgba8(small).write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)?;
    let b64 = base64::engine::general_purpose::STANDARD.encode(&png);
    Ok(ToolOutput::Blocks(vec![
        json!({"type":"text","text": format!("Ekran {w}x{h}, görüntü {SHOT_WIDTH}x{nh}")}),
        json!({"type":"image","source":{"type":"base64","media_type":"image/png","data": b64}}),
    ]))
}

/// Gerçek mouse'u insan gibi yumuşak bir eğriyle hedefe kaydırır;
/// sanal imleç de aynı yoldan gider.
fn glide(e: &mut Enigo, agent: &str, tx: i32, ty: i32, held: bool) -> anyhow::Result<()> {
    let (sx, sy) = e.location().unwrap_or((tx, ty));
    let (dx, dy) = ((tx - sx) as f64, (ty - sy) as f64);
    let dist = (dx * dx + dy * dy).sqrt();
    if dist < 2.0 {
        e.move_mouse(tx, ty, Coordinate::Abs)?;
        crate::overlay::emit(agent, tx as f64, ty as f64, if held { "drag" } else { "move" }, None);
        return Ok(());
    }
    // Mesafeye göre 150-600 ms; hafif kavisli yol (düz çizgi robotik görünür)
    let steps = ((dist / 18.0) as usize).clamp(10, 45);
    let total_ms = (150.0 + dist * 0.35).min(600.0);
    let bend = (dist * 0.08).min(40.0);
    let (nx, ny) = (-dy / dist, dx / dist); // dik yön
    for i in 1..=steps {
        let t = i as f64 / steps as f64;
        let k = t * t * (3.0 - 2.0 * t); // ease-in-out
        let arc = (std::f64::consts::PI * t).sin() * bend;
        let x = sx as f64 + dx * k + nx * arc;
        let y = sy as f64 + dy * k + ny * arc;
        e.move_mouse(x.round() as i32, y.round() as i32, Coordinate::Abs)?;
        if i % 3 == 0 || i == steps {
            crate::overlay::emit(agent, x, y, if held { "drag" } else { "move" }, None);
        }
        sleep(Duration::from_millis((total_ms / steps as f64) as u64));
    }
    e.move_mouse(tx, ty, Coordinate::Abs)?;
    Ok(())
}

fn button_of(input: &Value) -> Button {
    match input["button"].as_str() {
        Some("right") => Button::Right,
        Some("middle") => Button::Middle,
        _ => Button::Left,
    }
}

fn run(action: &str, input: &Value, agent: &str) -> anyhow::Result<ToolOutput> {
    if action == "screenshot" {
        return screenshot();
    }
    let geo = Geo::get()?;
    let mut e = Enigo::new(&Settings::default())?;
    let here = |e: &Enigo| e.location().unwrap_or((0, 0));
    let show = |kind: &str, (x, y): (i32, i32), text: Option<&str>| crate::overlay::emit(agent, x as f64, y as f64, kind, text);

    let result = match action {
        "cursor_position" => {
            let (x, y) = geo.to_image(here(&e).0, here(&e).1);
            format!("imleç: x={x}, y={y}")
        }
        "move" => {
            let (x, y) = xy(input, &geo, "x", "y")?;
            glide(&mut e, agent, x, y, false)?;
            "taşındı".into()
        }
        "click" | "double_click" | "triple_click" | "right_click" => {
            if input.get("x").is_some() {
                let (x, y) = xy(input, &geo, "x", "y")?;
                glide(&mut e, agent, x, y, false)?;
                sleep(Duration::from_millis(60));
            }
            let btn = if action == "right_click" { Button::Right } else { button_of(input) };
            let count = match action {
                "double_click" => 2,
                "triple_click" => 3,
                _ => 1,
            };
            let kind = if btn == Button::Right { "right_click" } else { "click" };
            for _ in 0..count {
                show(kind, here(&e), None);
                e.button(btn, Direction::Click)?;
                sleep(Duration::from_millis(70));
            }
            format!("{action} tamam")
        }
        "mouse_down" | "mouse_up" => {
            if input.get("x").is_some() {
                let (x, y) = xy(input, &geo, "x", "y")?;
                glide(&mut e, agent, x, y, action == "mouse_up")?;
            }
            let dir = if action == "mouse_down" { Direction::Press } else { Direction::Release };
            show(if action == "mouse_down" { "press" } else { "release" }, here(&e), None);
            e.button(button_of(input), dir)?;
            format!("{action} tamam")
        }
        "drag" => {
            let (x, y) = xy(input, &geo, "x", "y")?;
            let (tx, ty) = xy(input, &geo, "to_x", "to_y")?;
            glide(&mut e, agent, x, y, false)?;
            show("press", (x, y), None);
            e.button(Button::Left, Direction::Press)?;
            sleep(Duration::from_millis(120));
            glide(&mut e, agent, tx, ty, true)?;
            sleep(Duration::from_millis(120));
            show("release", (tx, ty), None);
            e.button(Button::Left, Direction::Release)?;
            "sürüklendi".into()
        }
        "scroll" => {
            if input.get("x").is_some() {
                let (x, y) = xy(input, &geo, "x", "y")?;
                glide(&mut e, agent, x, y, false)?;
            }
            let amount = input["amount"].as_i64().unwrap_or(3) as i32;
            let axis = if input["horizontal"].as_bool().unwrap_or(false) { Axis::Horizontal } else { Axis::Vertical };
            show("scroll", here(&e), Some(if amount > 0 { "aşağı" } else { "yukarı" }));
            // Tek tek kaydır: hareket görünür ve uygulamalar daha güvenilir işler
            for _ in 0..amount.abs() {
                e.scroll(amount.signum(), axis)?;
                sleep(Duration::from_millis(40));
            }
            "kaydırıldı".into()
        }
        "type" => {
            let t = input["text"].as_str().unwrap_or("");
            show("type", here(&e), Some(t));
            if t.chars().count() > 300 {
                e.text(t)?; // uzun metin: tek seferde
            } else {
                for c in t.chars() {
                    if c == '\n' {
                        e.key(Key::Return, Direction::Click)?;
                    } else {
                        e.text(&c.to_string())?;
                    }
                    sleep(Duration::from_millis(12));
                }
            }
            format!("{} karakter yazıldı", t.chars().count())
        }
        "key" | "hold_key" => {
            let combo = input["text"].as_str().unwrap_or("");
            let keys: Vec<Key> = combo.split('+').map(|k| parse_key(k.trim())).collect::<Result<_, _>>()?;
            if keys.is_empty() {
                anyhow::bail!("tuş gerekli");
            }
            show("key", here(&e), Some(combo));
            for k in &keys {
                e.key(*k, Direction::Press)?;
                sleep(Duration::from_millis(20));
            }
            let hold = if action == "hold_key" { input["amount"].as_u64().unwrap_or(500).min(10_000) } else { 30 };
            sleep(Duration::from_millis(hold));
            for k in keys.iter().rev() {
                e.key(*k, Direction::Release)?;
                sleep(Duration::from_millis(15));
            }
            format!("{combo} basıldı")
        }
        other => anyhow::bail!("bilinmeyen eylem: {other}"),
    };
    sleep(Duration::from_millis(120));
    Ok(ToolOutput::Text(result))
}

fn parse_key(k: &str) -> anyhow::Result<Key> {
    Ok(match k.to_lowercase().as_str() {
        "ctrl" | "control" => Key::Control,
        "alt" | "option" => Key::Alt,
        "shift" => Key::Shift,
        "win" | "super" | "meta" | "cmd" | "command" | "windows" => Key::Meta,
        "enter" | "return" => Key::Return,
        "tab" => Key::Tab,
        "esc" | "escape" => Key::Escape,
        "space" => Key::Space,
        "backspace" => Key::Backspace,
        "delete" | "del" => Key::Delete,
        "up" | "arrowup" => Key::UpArrow,
        "down" | "arrowdown" => Key::DownArrow,
        "left" | "arrowleft" => Key::LeftArrow,
        "right" | "arrowright" => Key::RightArrow,
        "home" => Key::Home,
        "end" => Key::End,
        "pageup" => Key::PageUp,
        "pagedown" => Key::PageDown,
        "capslock" => Key::CapsLock,
        "f1" => Key::F1, "f2" => Key::F2, "f3" => Key::F3, "f4" => Key::F4,
        "f5" => Key::F5, "f6" => Key::F6, "f7" => Key::F7, "f8" => Key::F8,
        "f9" => Key::F9, "f10" => Key::F10, "f11" => Key::F11, "f12" => Key::F12,
        s if s.chars().count() == 1 => Key::Unicode(s.chars().next().unwrap()),
        other => anyhow::bail!("bilinmeyen tuş: {other}"),
    })
}
