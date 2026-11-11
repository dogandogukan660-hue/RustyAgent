//! Tarayıcı aracı — Chrome DevTools Protocol (CDP).
//!
//! Her agent oturumu kendi tarayıcı PENCERESİNİ alır. Tıklama/yazma olayları
//! CDP ile doğrudan sayfaya gönderilir; işletim sisteminin gerçek imleci hiç
//! hareket etmez. Bu yüzden birden çok agent AYNI ANDA farklı pencerelerde
//! çalışabilir ve kullanıcı bu sırada kendi mouse'unu kullanmaya devam eder.
//! Her agent'ın tıkladığı yer ekranda kendi renkli sanal imleciyle gösterilir.
//!
//! Profil kalıcıdır (~/.rustyagent/browser-profile): sitelere bir kez giriş
//! yaparsan agent'lar oturumu kullanabilir.

use super::{Tool, ToolCtx, ToolOutput};
use crate::config::{self, Config};
use crate::overlay;
use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use futures::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{oneshot, Mutex};
use tokio_tungstenite::tungstenite::Message;

const PORT: u16 = 9333;

// ------------------------------------------------------------------ CDP bağlantısı

type Pending = Arc<std::sync::Mutex<HashMap<u64, oneshot::Sender<Value>>>>;

struct Cdp {
    tx: tokio::sync::mpsc::UnboundedSender<String>,
    pending: Pending,
    next: AtomicU64,
    /// Bağlantı koptu mu (tarayıcı/pencere kapatıldı)
    closed: Arc<std::sync::atomic::AtomicBool>,
}

impl Cdp {
    async fn connect(url: &str) -> Result<Arc<Self>> {
        let (ws, _) = tokio_tungstenite::connect_async(url).await.with_context(|| format!("CDP bağlantısı: {url}"))?;
        let (mut sink, mut stream) = ws.split();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        tokio::spawn(async move {
            while let Some(m) = rx.recv().await {
                if sink.send(Message::Text(m.into())).await.is_err() {
                    break;
                }
            }
        });
        let pending: Pending = Arc::new(std::sync::Mutex::new(HashMap::new()));
        let p2 = pending.clone();
        let closed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let closed2 = closed.clone();
        tokio::spawn(async move {
            while let Some(Ok(msg)) = stream.next().await {
                let Message::Text(t) = msg else { continue };
                let Ok(v) = serde_json::from_str::<Value>(&t) else { continue };
                if let Some(id) = v["id"].as_u64() {
                    if let Some(s) = p2.lock().unwrap().remove(&id) {
                        let _ = s.send(v);
                    }
                }
            }
            // Bağlantı koptu: bekleyenleri serbest bırak, sonraki çağrılar hemen hata versin
            closed2.store(true, Ordering::SeqCst);
            p2.lock().unwrap().clear();
        });
        Ok(Arc::new(Self { tx, pending, next: AtomicU64::new(1), closed }))
    }

    fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }

    async fn call(&self, method: &str, params: Value) -> Result<Value> {
        if self.is_closed() {
            bail!("tarayıcı bağlantısı kapalı");
        }
        let id = self.next.fetch_add(1, Ordering::SeqCst);
        let (s, r) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, s);
        self.tx
            .send(json!({"id": id, "method": method, "params": params}).to_string())
            .map_err(|_| anyhow::anyhow!("tarayıcı bağlantısı kapalı"))?;
        let v = tokio::time::timeout(Duration::from_secs(60), r)
            .await
            .with_context(|| format!("{method} zaman aşımı"))?
            .map_err(|_| anyhow::anyhow!("tarayıcı bağlantısı koptu (pencere kapatılmış olabilir)"))?;
        if let Some(e) = v.get("error") {
            bail!("{method}: {}", e["message"].as_str().unwrap_or(&e.to_string()));
        }
        Ok(v["result"].clone())
    }

    /// Sayfada JS çalıştırıp değerini döndürür
    async fn eval(&self, expr: &str) -> Result<Value> {
        let r = self
            .call("Runtime.evaluate", json!({"expression": expr, "returnByValue": true, "awaitPromise": true}))
            .await?;
        if let Some(ex) = r.get("exceptionDetails") {
            let msg = ex["exception"]["description"].as_str().or(ex["text"].as_str()).unwrap_or("JS hatası");
            bail!("{msg}");
        }
        Ok(r["result"]["value"].clone())
    }
}

// ------------------------------------------------------------------ tarayıcı + sayfalar

struct Page {
    cdp: Arc<Cdp>,
    target_id: String,
    /// Sayfa içindeki son mouse konumu (yumuşak hareket buradan başlar)
    mouse: std::sync::Mutex<(f64, f64)>,
}

struct Browser {
    root: Arc<Cdp>,
    pages: Mutex<HashMap<String, Arc<Page>>>,
}

static BROWSER: Mutex<Option<Arc<Browser>>> = Mutex::const_new(None);

fn find_browser(cfg_path: &str) -> Option<String> {
    if !cfg_path.is_empty() {
        return Some(cfg_path.to_string());
    }
    let mut c: Vec<String> = Vec::new();
    if cfg!(windows) {
        for base in [std::env::var("ProgramFiles").ok(), std::env::var("ProgramFiles(x86)").ok(), std::env::var("LOCALAPPDATA").ok()]
            .into_iter()
            .flatten()
        {
            c.push(format!(r"{base}\Google\Chrome\Application\chrome.exe"));
            c.push(format!(r"{base}\Microsoft\Edge\Application\msedge.exe"));
            c.push(format!(r"{base}\BraveSoftware\Brave-Browser\Application\brave.exe"));
        }
    } else if cfg!(target_os = "macos") {
        c.push("/Applications/Google Chrome.app/Contents/MacOS/Google Chrome".into());
        c.push("/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge".into());
        c.push("/Applications/Brave Browser.app/Contents/MacOS/Brave Browser".into());
        c.push("/Applications/Chromium.app/Contents/MacOS/Chromium".into());
    } else {
        for n in ["google-chrome", "google-chrome-stable", "chromium", "chromium-browser", "microsoft-edge", "brave-browser"] {
            for dir in ["/usr/bin", "/usr/local/bin", "/snap/bin"] {
                c.push(format!("{dir}/{n}"));
            }
        }
    }
    c.into_iter().find(|p| std::path::Path::new(p).exists())
}

/// "Sayfa çevrilsin mi?" balonu agent pencerelerinin üstünü kapatmasın
/// (tarayıcı kapalıyken profil tercihine yazılır)
fn disable_translate_prompt(prefs: &std::path::Path) {
    let mut v: Value = std::fs::read_to_string(prefs)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_else(|| json!({}));
    if !v.is_object() {
        return;
    }
    v["translate"] = json!({"enabled": false});
    let _ = std::fs::write(prefs, v.to_string());
}

async fn version_info(http: &reqwest::Client) -> Option<Value> {
    http.get(format!("http://127.0.0.1:{PORT}/json/version")).send().await.ok()?.json().await.ok()
}

/// Çalışan tarayıcıya bağlantıyı döndürür; yoksa veya kopmuşsa (kullanıcı tarayıcıyı kapattıysa) yeniden başlatır
async fn browser() -> Result<Arc<Browser>> {
    let mut guard = BROWSER.lock().await;
    if let Some(b) = guard.as_ref() {
        if !b.root.is_closed() {
            return Ok(b.clone());
        }
        *guard = None;
    }
    let b = Arc::new(launch().await?);
    *guard = Some(b.clone());
    Ok(b)
}

async fn current_browser() -> Option<Arc<Browser>> {
    BROWSER.lock().await.clone().filter(|b| !b.root.is_closed())
}

async fn launch() -> Result<Browser> {
    {
        {
            let http = reqwest::Client::builder().timeout(Duration::from_secs(3)).build()?;
            let mut info = version_info(&http).await;
            if info.is_none() {
                let cfg = Config::load()?;
                let exe = find_browser(&cfg.browser_path)
                    .context("Chrome/Edge/Brave bulunamadı; config.toml'da browser_path ayarla")?;
                let profile = config::home_dir().join("browser-profile");
                std::fs::create_dir_all(profile.join("Default"))?;
                disable_translate_prompt(&profile.join("Default").join("Preferences"));
                std::process::Command::new(&exe)
                    .arg(format!("--remote-debugging-port={PORT}"))
                    .arg(format!("--user-data-dir={}", profile.display()))
                    .args([
                        "--no-first-run",
                        "--no-default-browser-check",
                        "--remote-allow-origins=*",
                        // Çeviri balonları sayfayı kapatmasın; arka plan pencereleri yavaşlatılmasın
                        "--disable-features=Translate,TranslateUI,msTranslations",
                        "--disable-background-timer-throttling",
                        "--disable-backgrounding-occluded-windows",
                        "--disable-renderer-backgrounding",
                        "about:blank",
                    ])
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .spawn()
                    .with_context(|| format!("{exe} başlatılamadı"))?;
                for _ in 0..50 {
                    tokio::time::sleep(Duration::from_millis(200)).await;
                    info = version_info(&http).await;
                    if info.is_some() {
                        break;
                    }
                }
            }
            let info = info.context("tarayıcının hata ayıklama portuna bağlanılamadı")?;
            let ws = info["webSocketDebuggerUrl"].as_str().context("webSocketDebuggerUrl yok")?;
            let root = Cdp::connect(ws).await?;
            anyhow::Ok(Browser { root, pages: Mutex::new(HashMap::new()) })
        }
    }
}

/// Oturumun sayfasını döndürür; yoksa yeni pencere açar ve pencereleri döşer
async fn page_for(session: &str) -> Result<Arc<Page>> {
    let b = browser().await?;
    let mut pages = b.pages.lock().await;
    if let Some(p) = pages.get(session) {
        // Kullanıcı pencereyi kapattıysa yenisini aç (canlılık denetimi kısa süreli)
        let alive = !p.cdp.is_closed() && matches!(tokio::time::timeout(Duration::from_secs(5), p.cdp.eval("1")).await, Ok(Ok(_)));
        if alive {
            return Ok(p.clone());
        }
        pages.remove(session);
    }
    let r = b.root.call("Target.createTarget", json!({"url": "about:blank", "newWindow": true})).await?;
    let target_id = r["targetId"].as_str().context("targetId yok")?.to_string();
    let cdp = Cdp::connect(&format!("ws://127.0.0.1:{PORT}/devtools/page/{target_id}")).await?;
    let _ = cdp.call("Page.enable", json!({})).await;
    // Arka planda kalan pencere de odaktaymış gibi davransın: paralel agent'ların
    // klavye/odak olayları, pencere öne gelmeden çalışır
    let _ = cdp.call("Emulation.setFocusEmulationEnabled", json!({"enabled": true})).await;
    let page = Arc::new(Page { cdp, target_id, mouse: std::sync::Mutex::new((20.0, 20.0)) });
    pages.insert(session.to_string(), page.clone());
    let ids: Vec<String> = pages.values().map(|p| p.target_id.clone()).collect();
    drop(pages);
    tile_windows(&b, &ids).await;
    Ok(page)
}

/// Agent pencerelerini ekranda ızgara halinde yerleştirir (hepsi aynı anda görünsün)
async fn tile_windows(b: &Browser, target_ids: &[String]) {
    let (sw, sh, sx, sy) = screen_css().unwrap_or((1600.0, 900.0, 0.0, 0.0));
    let n = target_ids.len().max(1);
    let cols = if n == 1 { 1 } else { 2 };
    let rows = n.div_ceil(cols);
    let (w, h) = (sw / cols as f64, (sh - 40.0) / rows as f64);
    for (i, t) in target_ids.iter().enumerate() {
        let Ok(r) = b.root.call("Browser.getWindowForTarget", json!({"targetId": t})).await else { continue };
        let Some(wid) = r["windowId"].as_i64() else { continue };
        let (c, rw) = (i % cols, i / cols);
        let _ = b.root.call("Browser.setWindowBounds", json!({"windowId": wid, "bounds": {"windowState": "normal"}})).await;
        let _ = b
            .root
            .call("Browser.setWindowBounds", json!({"windowId": wid, "bounds": {
                "left": (sx + c as f64 * w) as i64, "top": (sy + rw as f64 * h) as i64,
                "width": w as i64, "height": h as i64
            }}))
            .await;
    }
}

/// Birincil ekran boyutu (CSS pikseli) ve konumu
fn screen_css() -> Option<(f64, f64, f64, f64)> {
    let mons = xcap::Monitor::all().ok()?;
    let m = mons.iter().find(|m| m.is_primary().unwrap_or(false)).cloned().or_else(|| mons.into_iter().next())?;
    let s = m.scale_factor().ok()? as f64;
    Some((m.width().ok()? as f64 / s, m.height().ok()? as f64 / s, m.x().ok()? as f64 / s, m.y().ok()? as f64 / s))
}

/// İzin sistemi için: index'li öğenin görünen etiketi (ör. "Satın Al"). Sayfa yoksa None.
pub async fn element_label(session: &str, index: i64) -> Option<String> {
    let b = current_browser().await?;
    let p = b.pages.lock().await.get(session)?.clone();
    let v = p
        .cdp
        .eval(&format!(
            "(() => {{ const e = document.querySelector('[data-ra-id=\"{index}\"]'); if (!e) return ''; \
             const f = e.closest('form'); const btn = f && f.querySelector('[type=submit],button:not([type=button])'); \
             return [(e.innerText || e.value || e.getAttribute('aria-label') || e.getAttribute('title') || ''), \
                     (btn ? (btn.innerText || btn.value || '') : '')].join(' ').trim().replace(/\\s+/g, ' ').slice(0, 120); }})()"
        ))
        .await
        .ok()?;
    v.as_str().map(String::from)
}

/// Oturum bitince penceresini kapat
pub fn release(session: &str) {
    let Ok(h) = tokio::runtime::Handle::try_current() else { return };
    let session = session.to_string();
    h.spawn(async move {
        let Some(b) = current_browser().await else { return };
        if let Some(p) = b.pages.lock().await.remove(&session) {
            let _ = b.root.call("Target.closeTarget", json!({"targetId": p.target_id})).await;
        }
    });
}

// ------------------------------------------------------------------ sayfa işlemleri

/// Sayfa içi koordinatı fiziksel ekran koordinatına çevirir (sanal imleç için)
async fn to_screen(p: &Page, x: f64, y: f64) -> Option<(f64, f64)> {
    let v = p
        .cdp
        .eval("[screenX, screenY, outerWidth - innerWidth, outerHeight - innerHeight, devicePixelRatio]")
        .await
        .ok()?;
    let a: Vec<f64> = v.as_array()?.iter().filter_map(|n| n.as_f64()).collect();
    let (sx, sy, dw, dh, dpr) = (a[0], a[1], a[2], a[3], a[4]);
    let border = dw / 2.0;
    Some(((sx + border + x) * dpr, (sy + dh - border + y) * dpr))
}

async fn wait_ready(p: &Page) {
    for _ in 0..40 {
        if let Ok(v) = p.cdp.eval("document.readyState").await {
            if v == "complete" {
                return;
            }
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

const SNAPSHOT_JS: &str = r#"(() => {
  document.querySelectorAll('[data-ra-id]').forEach(e => e.removeAttribute('data-ra-id'));
  const sel = 'a[href],button,input,select,textarea,summary,[role=button],[role=link],[role=tab],[role=menuitem],[role=checkbox],[role=option],[role=combobox],[contenteditable=true],[onclick]';
  const out = []; let i = 0;
  for (const e of document.querySelectorAll(sel)) {
    const r = e.getBoundingClientRect(); const st = getComputedStyle(e);
    if (r.width < 2 || r.height < 2 || st.visibility === 'hidden' || st.display === 'none' || e.disabled) continue;
    if (r.bottom < -200 || r.top > innerHeight * 3) continue;
    e.setAttribute('data-ra-id', i);
    let label = (e.innerText || e.value || e.getAttribute('aria-label') || e.getAttribute('placeholder') || e.getAttribute('title') || e.getAttribute('name') || e.getAttribute('alt') || '').trim().replace(/\s+/g, ' ').slice(0, 80);
    let tag = e.tagName.toLowerCase();
    if (tag === 'input') tag += '[' + (e.type || 'text') + ']';
    if (tag === 'a' && e.getAttribute('href')) label += ' → ' + e.getAttribute('href').slice(0, 70);
    const where = r.top > innerHeight ? ' (aşağıda)' : (r.bottom < 0 ? ' (yukarıda)' : '');
    out.push('[' + i + '] ' + tag + ' "' + label + '"' + where);
    if (++i >= 250) break;
  }
  const text = document.body ? document.body.innerText.replace(/\n{3,}/g, '\n\n').slice(0, 8000) : '';
  return 'URL: ' + location.href + '\nBaşlık: ' + document.title + '\n\n# Etkileşimli öğeler (index ile kullan)\n' + out.join('\n') + '\n\n# Sayfa metni\n' + text;
})()"#;

/// index'li öğeyi görünür alana kaydırıp merkez koordinatını döndürür
async fn element_center(p: &Page, index: i64) -> Result<(f64, f64)> {
    let v = p
        .cdp
        .eval(&format!(
            "(() => {{ const e = document.querySelector('[data-ra-id=\"{index}\"]'); if (!e) return null; \
             e.scrollIntoView({{block: 'center', inline: 'center'}}); const r = e.getBoundingClientRect(); \
             return [r.left + r.width / 2, r.top + r.height / 2]; }})()"
        ))
        .await?;
    let a = v.as_array().context("öğe bulunamadı — sayfa değişmiş olabilir, önce 'snapshot' al")?;
    Ok((a[0].as_f64().unwrap_or(0.0), a[1].as_f64().unwrap_or(0.0)))
}

/// Sayfa içi mouse'u son konumundan hedefe yumuşakça kaydırır. Ara mouseMoved
/// olayları gönderildiği için sayfadaki hover efektleri de tetiklenir; sanal imleç aynı yoldan gider.
/// buttons: 0 = boş, 1 = sol tuş basılı (sürükleme)
async fn move_to(p: &Page, agent: &str, x: f64, y: f64, buttons: u8) -> Result<()> {
    let (sx, sy) = *p.mouse.lock().unwrap();
    let dist = ((x - sx).powi(2) + (y - sy).powi(2)).sqrt();
    let steps = ((dist / 25.0) as usize).clamp(1, 20);
    // Ekran dönüşümü bir kez hesaplanır (pencere hareket etmiyor)
    let origin = to_screen(p, 0.0, 0.0).await;
    let dpr_scale = match (origin, to_screen(p, 100.0, 0.0).await) {
        (Some(a), Some(b)) => (b.0 - a.0) / 100.0,
        _ => 1.0,
    };
    for i in 1..=steps {
        let t = i as f64 / steps as f64;
        let k = t * t * (3.0 - 2.0 * t);
        let (cx, cy) = (sx + (x - sx) * k, sy + (y - sy) * k);
        let mut ev = json!({"type": "mouseMoved", "x": cx, "y": cy, "buttons": buttons});
        if buttons == 1 {
            ev["button"] = json!("left");
        }
        p.cdp.call("Input.dispatchMouseEvent", ev).await?;
        if let Some((ox, oy)) = origin {
            if i % 2 == 0 || i == steps {
                overlay::emit(agent, ox + cx * dpr_scale, oy + cy * dpr_scale, if buttons == 1 { "drag" } else { "move" }, None);
            }
        }
        tokio::time::sleep(Duration::from_millis(14)).await;
    }
    *p.mouse.lock().unwrap() = (x, y);
    Ok(())
}

async fn mouse_button(p: &Page, kind: &str, x: f64, y: f64, button: &str, count: u32) -> Result<()> {
    p.cdp
        .call("Input.dispatchMouseEvent", json!({"type": kind, "x": x, "y": y, "button": button, "clickCount": count,
            "buttons": if kind == "mousePressed" { match button { "right" => 2, "middle" => 4, _ => 1 } } else { 0 }}))
        .await?;
    Ok(())
}

/// Tıklama: button = left | right | middle, count = 1 (tek) / 2 (çift) / 3 (üçlü)
async fn click_at(p: &Page, agent: &str, x: f64, y: f64, button: &str, count: u32) -> Result<()> {
    move_to(p, agent, x, y, 0).await?;
    tokio::time::sleep(Duration::from_millis(60)).await;
    let screen = to_screen(p, x, y).await;
    for n in 1..=count {
        if let Some((sx, sy)) = screen {
            overlay::emit(agent, sx, sy, if button == "right" { "right_click" } else { "click" }, None);
        }
        mouse_button(p, "mousePressed", x, y, button, n).await?;
        mouse_button(p, "mouseReleased", x, y, button, n).await?;
        tokio::time::sleep(Duration::from_millis(60)).await;
    }
    Ok(())
}

async fn drag(p: &Page, agent: &str, from: (f64, f64), to: (f64, f64)) -> Result<()> {
    move_to(p, agent, from.0, from.1, 0).await?;
    if let Some((sx, sy)) = to_screen(p, from.0, from.1).await {
        overlay::emit(agent, sx, sy, "press", None);
    }
    mouse_button(p, "mousePressed", from.0, from.1, "left", 1).await?;
    tokio::time::sleep(Duration::from_millis(100)).await;
    move_to(p, agent, to.0, to.1, 1).await?;
    tokio::time::sleep(Duration::from_millis(100)).await;
    mouse_button(p, "mouseReleased", to.0, to.1, "left", 1).await?;
    if let Some((sx, sy)) = to_screen(p, to.0, to.1).await {
        overlay::emit(agent, sx, sy, "release", None);
    }
    Ok(())
}

/// Tuş tanımı: (key, code, windowsVirtualKeyCode, üretilen metin)
fn key_def(k: &str) -> Result<(String, String, i64, String)> {
    let s = |a: &str, b: &str, vk: i64, t: &str| Ok((a.to_string(), b.to_string(), vk, t.to_string()));
    match k.to_lowercase().as_str() {
        "enter" | "return" => s("Enter", "Enter", 13, "\r"),
        "tab" => s("Tab", "Tab", 9, ""),
        "escape" | "esc" => s("Escape", "Escape", 27, ""),
        "backspace" => s("Backspace", "Backspace", 8, ""),
        "delete" | "del" => s("Delete", "Delete", 46, ""),
        "arrowdown" | "down" => s("ArrowDown", "ArrowDown", 40, ""),
        "arrowup" | "up" => s("ArrowUp", "ArrowUp", 38, ""),
        "arrowleft" | "left" => s("ArrowLeft", "ArrowLeft", 37, ""),
        "arrowright" | "right" => s("ArrowRight", "ArrowRight", 39, ""),
        "pagedown" => s("PageDown", "PageDown", 34, ""),
        "pageup" => s("PageUp", "PageUp", 33, ""),
        "home" => s("Home", "Home", 36, ""),
        "end" => s("End", "End", 35, ""),
        "space" => s(" ", "Space", 32, " "),
        f if f.len() >= 2 && f.starts_with('f') && f[1..].parse::<i64>().is_ok() => {
            let n: i64 = f[1..].parse().unwrap();
            s(&format!("F{n}"), &format!("F{n}"), 111 + n, "")
        }
        c if c.chars().count() == 1 => {
            let ch = k.chars().next().unwrap();
            let up = ch.to_ascii_uppercase();
            let (code, vk) = if ch.is_ascii_alphabetic() {
                (format!("Key{up}"), up as i64)
            } else if ch.is_ascii_digit() {
                (format!("Digit{ch}"), ch as i64)
            } else {
                (String::new(), 0)
            };
            s(&ch.to_string(), &code, vk, &ch.to_string())
        }
        other => bail!("desteklenmeyen tuş: {other}"),
    }
}

/// Tek tuş veya kombinasyon: "Enter", "ctrl+a", "ctrl+shift+t", "shift+tab"
async fn press_key(p: &Page, combo: &str) -> Result<()> {
    let parts: Vec<&str> = combo.split('+').map(|x| x.trim()).filter(|x| !x.is_empty()).collect();
    let Some((last, mods)) = parts.split_last() else { bail!("tuş gerekli") };
    let mut modifiers = 0;
    for m in mods {
        modifiers |= match m.to_lowercase().as_str() {
            "alt" | "option" => 1,
            "ctrl" | "control" => 2,
            "meta" | "cmd" | "command" | "win" => 4,
            "shift" => 8,
            other => bail!("bilinmeyen değiştirici tuş: {other}"),
        };
    }
    let (key, code, vk, text) = key_def(last)?;
    let ctrl_like = modifiers & (2 | 4) != 0;
    // Düzenleme kısayollarını tarayıcı komutlarına eşle (CDP olayları bunları tek başına tetiklemez)
    let commands: Vec<&str> = if ctrl_like {
        match key.to_lowercase().as_str() {
            "a" => vec!["selectAll"],
            "c" => vec!["copy"],
            "v" => vec!["paste"],
            "x" => vec!["cut"],
            "z" if modifiers & 8 != 0 => vec!["redo"],
            "z" => vec!["undo"],
            "y" => vec!["redo"],
            _ => vec![],
        }
    } else {
        vec![]
    };
    let produces_text = !text.is_empty() && modifiers & (1 | 2 | 4) == 0;
    let mut down = json!({"type": if produces_text { "keyDown" } else { "rawKeyDown" }, "key": key, "code": code,
        "windowsVirtualKeyCode": vk, "modifiers": modifiers});
    if produces_text {
        let t = if modifiers & 8 != 0 { text.to_uppercase() } else { text.clone() };
        down["text"] = json!(t);
        down["unmodifiedText"] = json!(text);
    }
    if !commands.is_empty() {
        down["commands"] = json!(commands);
    }
    p.cdp.call("Input.dispatchKeyEvent", down).await?;
    p.cdp
        .call("Input.dispatchKeyEvent", json!({"type": "keyUp", "key": key, "code": code, "windowsVirtualKeyCode": vk, "modifiers": modifiers}))
        .await?;
    Ok(())
}

/// index veya x/y'den sayfa koordinatı
async fn target_point(p: &Page, input: &Value, ix: &str, kx: &str, ky: &str) -> Result<(f64, f64)> {
    match input[ix].as_i64() {
        Some(i) => element_center(p, i).await,
        None => Ok((
            input[kx].as_f64().with_context(|| format!("{ix} veya {kx}/{ky} gerekli"))?,
            input[ky].as_f64().with_context(|| format!("{ky} gerekli"))?,
        )),
    }
}

async fn snapshot_after(p: &Page, prefix: &str) -> Result<ToolOutput> {
    tokio::time::sleep(Duration::from_millis(500)).await;
    wait_ready(p).await;
    Ok(ToolOutput::Text(format!("{prefix}. Yeni durum:\n{}", p.cdp.eval(SNAPSHOT_JS).await?.as_str().unwrap_or(""))))
}

async fn run(input: &Value, ctx: &ToolCtx) -> Result<ToolOutput> {
    let action = input["action"].as_str().unwrap_or("");
    // Kapatma isteği yeni pencere açmamalı
    if action == "close" {
        release(&ctx.session);
        overlay::hide(&ctx.agent);
        return Ok(ToolOutput::Text("pencere kapatıldı".into()));
    }
    let p = page_for(&ctx.session).await?;
    let idx = input["index"].as_i64();
    match action {
        "navigate" => {
            let mut url = input["url"].as_str().context("url gerekli")?.to_string();
            let has_scheme = url.contains("://") || ["about:", "data:", "file:", "chrome:", "edge:"].iter().any(|s| url.starts_with(s));
            if !has_scheme {
                url = format!("https://{url}");
            }
            let r = p.cdp.call("Page.navigate", json!({"url": url})).await?;
            if let Some(e) = r["errorText"].as_str() {
                bail!("açılamadı: {e}");
            }
            wait_ready(&p).await;
            Ok(ToolOutput::Text(p.cdp.eval(SNAPSHOT_JS).await?.as_str().unwrap_or("").to_string()))
        }
        "snapshot" => Ok(ToolOutput::Text(p.cdp.eval(SNAPSHOT_JS).await?.as_str().unwrap_or("").to_string())),
        "click" | "double_click" | "triple_click" | "right_click" | "middle_click" => {
            let (x, y) = target_point(&p, input, "index", "x", "y").await?;
            let button = match action {
                "right_click" => "right",
                "middle_click" => "middle",
                _ => input["button"].as_str().unwrap_or("left"),
            };
            let count = match action {
                "double_click" => 2,
                "triple_click" => 3,
                _ => 1,
            };
            click_at(&p, &ctx.agent, x, y, button, count).await?;
            snapshot_after(&p, &format!("{action} yapıldı")).await
        }
        "hover" => {
            let (x, y) = target_point(&p, input, "index", "x", "y").await?;
            move_to(&p, &ctx.agent, x, y, 0).await?;
            snapshot_after(&p, "üzerine gelindi").await
        }
        "drag" => {
            let from = target_point(&p, input, "index", "x", "y").await?;
            let to = target_point(&p, input, "to_index", "to_x", "to_y").await?;
            drag(&p, &ctx.agent, from, to).await?;
            snapshot_after(&p, "sürüklendi").await
        }
        "type" => {
            let text = input["text"].as_str().context("text gerekli")?;
            let mut at = None;
            if let Some(i) = idx {
                let (x, y) = element_center(&p, i).await?;
                click_at(&p, &ctx.agent, x, y, "left", 1).await?;
                if input["clear"].as_bool().unwrap_or(true) {
                    p.cdp
                        .eval(&format!(
                            "(() => {{ const e = document.querySelector('[data-ra-id=\"{i}\"]'); if (e && 'value' in e) {{ e.value = ''; e.dispatchEvent(new Event('input', {{bubbles: true}})); }} }})()"
                        ))
                        .await?;
                }
                at = to_screen(&p, x, y).await;
            } else {
                let (mx, my) = *p.mouse.lock().unwrap();
                at = at.or(to_screen(&p, mx, my).await);
            }
            if let Some((sx, sy)) = at {
                overlay::emit(&ctx.agent, sx, sy, "type", Some(text));
            }
            // Kısa metinler harf harf (görünür yazma), uzunlar tek seferde
            if text.chars().count() <= 150 {
                for c in text.chars() {
                    if c == '\n' {
                        press_key(&p, "enter").await?;
                    } else {
                        p.cdp.call("Input.insertText", json!({"text": c.to_string()})).await?;
                    }
                    tokio::time::sleep(Duration::from_millis(15)).await;
                }
            } else {
                p.cdp.call("Input.insertText", json!({"text": text})).await?;
            }
            if input["submit"].as_bool().unwrap_or(false) {
                press_key(&p, "enter").await?;
                tokio::time::sleep(Duration::from_millis(300)).await;
                return snapshot_after(&p, "yazıldı ve gönderildi").await;
            }
            Ok(ToolOutput::Text("yazıldı".into()))
        }
        "key" => {
            let combo = input["text"].as_str().context("text (tuş veya kombinasyon) gerekli")?;
            let (mx, my) = *p.mouse.lock().unwrap();
            if let Some((sx, sy)) = to_screen(&p, mx, my).await {
                overlay::emit(&ctx.agent, sx, sy, "key", Some(combo));
            }
            press_key(&p, combo).await?;
            tokio::time::sleep(Duration::from_millis(300)).await;
            Ok(ToolOutput::Text(format!("{combo} basıldı")))
        }
        "select" => {
            let i = idx.context("select için index gerekli")?;
            let want = input["text"].as_str().context("text (seçenek metni veya değeri) gerekli")?;
            let (x, y) = element_center(&p, i).await?;
            move_to(&p, &ctx.agent, x, y, 0).await?;
            if let Some((sx, sy)) = to_screen(&p, x, y).await {
                overlay::emit(&ctx.agent, sx, sy, "click", None);
            }
            let r = p
                .cdp
                .eval(&format!(
                    "(() => {{ const e = document.querySelector('[data-ra-id=\"{i}\"]'); if (!e || !e.options) return 'bu öğe bir açılır liste değil'; \
                     const w = {w}.toLowerCase(); const o = [...e.options].find(o => o.value.toLowerCase() === w || o.text.trim().toLowerCase() === w) \
                       || [...e.options].find(o => o.text.toLowerCase().includes(w)); \
                     if (!o) return 'seçenek yok. Seçenekler: ' + [...e.options].map(o => o.text.trim()).join(' | '); \
                     e.value = o.value; e.dispatchEvent(new Event('input', {{bubbles: true}})); e.dispatchEvent(new Event('change', {{bubbles: true}})); \
                     return 'seçildi: ' + o.text.trim(); }})()",
                    w = serde_json::to_string(want)?
                ))
                .await?;
            Ok(ToolOutput::Text(r.as_str().unwrap_or("").to_string()))
        }
        "scroll" => {
            let amount = input["amount"].as_f64().unwrap_or(600.0);
            let horizontal = input["horizontal"].as_bool().unwrap_or(false);
            let (mx, my) = *p.mouse.lock().unwrap();
            if let Some((sx, sy)) = to_screen(&p, mx, my).await {
                overlay::emit(&ctx.agent, sx, sy, "scroll", Some(if amount > 0.0 { "aşağı" } else { "yukarı" }));
            }
            // Birkaç adımda kaydır: sayfa akıcı hareket etsin, tembel yüklenen içerik tetiklensin
            let steps = 4;
            for _ in 0..steps {
                let (dx, dy) = if horizontal { (amount / steps as f64, 0.0) } else { (0.0, amount / steps as f64) };
                p.cdp
                    .call("Input.dispatchMouseEvent", json!({"type": "mouseWheel", "x": mx, "y": my, "deltaX": dx, "deltaY": dy}))
                    .await?;
                tokio::time::sleep(Duration::from_millis(60)).await;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
            Ok(ToolOutput::Text(p.cdp.eval(SNAPSHOT_JS).await?.as_str().unwrap_or("").to_string()))
        }
        "back" => {
            p.cdp.eval("history.back()").await?;
            tokio::time::sleep(Duration::from_millis(800)).await;
            wait_ready(&p).await;
            Ok(ToolOutput::Text(p.cdp.eval(SNAPSHOT_JS).await?.as_str().unwrap_or("").to_string()))
        }
        "screenshot" => {
            let r = p.cdp.call("Page.captureScreenshot", json!({"format": "jpeg", "quality": 70})).await?;
            Ok(ToolOutput::Blocks(vec![
                json!({"type": "text", "text": "Sayfanın görünen kısmı:"}),
                json!({"type": "image", "source": {"type": "base64", "media_type": "image/jpeg", "data": r["data"]}}),
            ]))
        }
        "evaluate" => {
            let v = p.cdp.eval(input["text"].as_str().context("text (JS ifadesi) gerekli")?).await?;
            Ok(ToolOutput::Text(if v.is_string() { v.as_str().unwrap().to_string() } else { v.to_string() }))
        }
        "close" => {
            release(&ctx.session);
            overlay::hide(&ctx.agent);
            Ok(ToolOutput::Text("pencere kapatıldı".into()))
        }
        other => bail!("bilinmeyen eylem: {other}"),
    }
}

pub struct BrowserTool;

#[async_trait]
impl Tool for BrowserTool {
    fn name(&self) -> String {
        "browser".into()
    }
    fn description(&self) -> String {
        "Web tarayıcısını kullanır. Her agent kendi penceresine sahiptir ve diğer agent'larla aynı anda çalışabilir \
         (kullanıcının mouse'unu kullanmaz). Akış: navigate → dönen öğe listesindeki [index] ile click/type → sonucu kontrol et. \
         Gezinme: navigate(url), back, snapshot, screenshot. \
         Mouse (index veya x,y): click (button: left/right/middle), double_click, triple_click, right_click, middle_click, \
         hover (üzerine gel: menüler açılır), drag (index|x,y → to_index|to_x,to_y), scroll (amount piksel, horizontal). \
         Klavye: type (text; index verilirse önce o alana tıklar; clear, submit), key (tek tuş veya kombinasyon: Enter, Tab, Escape, \
         ArrowDown, F5, ctrl+a, ctrl+c, ctrl+v, ctrl+z, shift+tab...). Form: select (index + text: açılır listeden seçenek). \
         Diğer: evaluate (text: JS), close. Görsel düzen önemliyse screenshot al. \
         Ödeme, satın alma, mesaj/e-posta gönderme, silme gibi geri alınamaz işlemlerden önce dur ve onay iste."
            .into()
    }
    fn input_schema(&self) -> Value {
        json!({"type": "object", "properties": {
            "action": {"type": "string", "enum": ["navigate", "back", "snapshot", "screenshot", "click", "double_click", "triple_click",
                "right_click", "middle_click", "hover", "drag", "scroll", "type", "key", "select", "evaluate", "close"]},
            "url": {"type": "string"},
            "index": {"type": "integer", "description": "snapshot'taki öğe numarası"},
            "x": {"type": "number"}, "y": {"type": "number"},
            "to_index": {"type": "integer", "description": "drag: bırakılacak öğe"},
            "to_x": {"type": "number"}, "to_y": {"type": "number"},
            "button": {"type": "string", "enum": ["left", "right", "middle"]},
            "text": {"type": "string", "description": "type: metin · key: tuş/kombinasyon · select: seçenek · evaluate: JS"},
            "clear": {"type": "boolean", "description": "type: önce alanı temizle (varsayılan true)"},
            "submit": {"type": "boolean", "description": "type: yazdıktan sonra Enter"},
            "amount": {"type": "number", "description": "scroll: piksel (+aşağı / -yukarı)"},
            "horizontal": {"type": "boolean", "description": "scroll: yatay"}
        }, "required": ["action"], "additionalProperties": false})
    }
    async fn call(&self, input: Value) -> ToolOutput {
        self.call_ctx(input, &ToolCtx { agent: "agent".into(), session: "default".into() }).await
    }
    async fn call_ctx(&self, input: Value, ctx: &ToolCtx) -> ToolOutput {
        match run(&input, ctx).await {
            Ok(o) => o,
            Err(e) => ToolOutput::Error(format!("{e:#}")),
        }
    }
}
