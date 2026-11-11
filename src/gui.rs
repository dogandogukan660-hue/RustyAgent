//! Masaüstü uygulaması (egui).
//!
//! Düzen: solda kenar çubuğu (yeni sohbet, sayfalar, tarihe göre gruplanmış son sohbetler),
//! ortada ortalanmış konuşma sütunu ve giriş kutusu, isteğe bağlı sağda ekip paneli
//! (animasyonlu agent karakterleri). Sayfalar: Sohbet, Hafıza (second brain), Agent'lar, Modeller,
//! Bağlantılar (MCP), Skill'ler, Plugin'ler, Otomasyonlar, Ayarlar. Emoji yok; ikonlar çizilir.

use crate::agent::{self, AgentDef, AgentEvent, EventKind, Runtime, MAIN_AGENT};
use crate::character::{self, Kind, Look, Mood};
use crate::config::{Config, PermissionMode};
use crate::markdown;
use crate::permissions::{ApprovalHandle, Decision, Risk};
use crate::theme::{self, Icon, Palette};
use crate::{automation, brain, commands, mcp, models, plugins, sessions, skills};
use base64::Engine;
use eframe::egui::{self, Color32, CornerRadius, RichText, Sense, Stroke, Vec2};
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

// ------------------------------------------------------------------ türler

#[derive(PartialEq, Clone, Copy)]
enum Page {
    Chat,
    Brain,
    Agents,
    Models,
    Mcp,
    Skills,
    Plugins,
    Automations,
    Settings,
}

const NAV: [(Page, Icon, &str); 8] = [
    (Page::Chat, Icon::Chat, "Sohbet"),
    (Page::Brain, Icon::Brain, "Hafıza"),
    (Page::Agents, Icon::People, "Agent'lar"),
    (Page::Models, Icon::Chip, "Modeller"),
    (Page::Mcp, Icon::Plug, "Bağlantılar (MCP)"),
    (Page::Skills, Icon::Book, "Skill'ler"),
    (Page::Plugins, Icon::Puzzle, "Plugin'ler"),
    (Page::Automations, Icon::Clock, "Otomasyonlar"),
];

enum Job {
    Prompt(String, Vec<Value>),
    Regenerate,
    Reset,
    SetModel(String),
    Compact,
    Resume(Option<String>),
    /// Sohbeti bu kullanıcı turunun öncesine geri sar
    Rewind(usize),
    Rename(String),
}

enum Msg {
    Agent(AgentEvent),
    TurnFinished,
    Info(String),
    Error(String),
    Model(String),
    Meta { id: Option<String>, title: String },
    McpResults(Vec<mcp::RegistryEntry>),
    McpTest(String),
    SkillResults(Vec<skills::RemoteSkill>),
    PluginResults(Vec<plugins::MarketPlugin>),
    HfResults(Vec<models::HfModel>),
    HfQuants(String, Vec<String>),
    ModelList(Vec<String>),
    OllamaModels(Vec<String>),
    Progress(String),
    UserLine(String, Vec<String>),
    /// Tarayıcıdan bağlanma durumu (boş = bitti)
    Connecting(String),
    Connected(String, String),
    /// Sohbet geri sarıldı: tur numarası, o turun metni ve ekleri (düzenlenip yeniden gönderilebilir)
    Rewound { turn: usize, text: String, blocks: Vec<Value> },
    /// Sağlayıcının gerçek sohbet modelleri (model seçici için)
    ProviderModels(String, Vec<String>),
}

/// Ekranın altında kısa süre görünen bildirim; silme işlemlerinde "Geri al" sunar
struct Toast {
    msg: String,
    err: bool,
    at: Instant,
    /// Geri getirilebilecek silinmiş sohbetin kimliği
    undo: Option<String>,
}

#[derive(Clone, Copy, PartialEq)]
enum EK {
    User,
    /// Yazılan / komutunun yankısı (sohbet turu sayılmaz)
    Command,
    Text,
    Thinking,
    Tool,
    ToolOk,
    ToolErr,
    Info,
    Error,
}

struct Entry {
    kind: EK,
    agent: String,
    text: String,
    files: Vec<String>,
}

struct Attachment {
    name: String,
    block: Value,
}

struct Actor {
    mood: Mood,
    until: Option<Instant>,
    bubble: String,
    last: Instant,
}

impl Actor {
    fn new() -> Self {
        Self { mood: Mood::Idle, until: None, bubble: String::new(), last: Instant::now() }
    }
    fn set(&mut self, mood: Mood, secs: Option<f32>) {
        self.mood = mood;
        self.until = secs.map(|s| Instant::now() + Duration::from_secs_f32(s));
        self.last = Instant::now();
    }
    fn current(&mut self) -> Mood {
        if let Some(u) = self.until {
            if Instant::now() > u {
                self.mood = Mood::Idle;
                self.until = None;
            }
        }
        if self.mood == Mood::Idle && self.last.elapsed() > Duration::from_secs(120) {
            return Mood::Sleeping;
        }
        self.mood
    }
}

#[derive(PartialEq, Clone, Copy)]
enum BrainTab {
    Notes,
    History,
}

// ------------------------------------------------------------------ uygulama

struct App {
    rt: Arc<Runtime>,
    handle: tokio::runtime::Handle,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    job_tx: tokio::sync::mpsc::UnboundedSender<Job>,
    cancel: Arc<tokio::sync::Notify>,
    pal: Palette,
    page: Page,
    start: Instant,
    toast: Option<Toast>,
    /// Başlık düzenleniyor mu (geçici metin)
    renaming: Option<String>,
    /// Bağlı sağlayıcıların gerçek sohbet modelleri
    provider_models: HashMap<String, Vec<String>>,
    sidebar_open: bool,
    team_open: bool,

    // sohbet
    entries: Vec<Entry>,
    input: String,
    attachments: Vec<Attachment>,
    busy: bool,
    model: String,
    title: String,
    current_id: Option<String>,
    tokens: (u64, u64),
    approvals: VecDeque<ApprovalHandle>,
    actors: HashMap<String, Actor>,
    agents: Vec<AgentDef>,
    recents: Vec<(String, String, String)>,
    recents_filter: String,
    recents_at: Instant,
    ollama_models: Vec<String>,
    /// Aktif model kullanılabilir mi (anahtar var / yerel)
    model_ok: bool,
    connecting: Option<String>,

    // hafıza
    brain_tab: BrainTab,
    brain_query: String,
    brain_kind: String,
    note_sel: Option<String>,
    note_edit: Option<brain::Note>,
    note_tags: String,
    history_query: String,
    history_results: Vec<(String, String, String, String)>,

    // agent düzenleyici
    edit: Option<AgentDef>,
    edit_main: bool,
    preview_mood: Mood,

    // modeller
    model_input: String,
    key_inputs: HashMap<String, String>,
    list_provider: String,
    list_filter: String,
    model_list: Vec<String>,
    hf_query: String,
    hf_results: Vec<models::HfModel>,
    hf_quants: HashMap<String, (Vec<String>, String)>,
    progress: String,
    /// Agent çalışırken gösterilen geçici durum (ör. "model yoğun, tekrar deneniyor")
    status: String,

    // mcp / skill / plugin
    mcp_query: String,
    mcp_results: Vec<mcp::RegistryEntry>,
    mcp_test: String,
    mcp_form: (String, String, String),
    mcp_http_form: (String, String, String),
    mcp_loading: bool,
    mcp_typed: Option<Instant>,
    skill_loading: bool,
    plugin_loading: bool,
    skill_query: String,
    skill_results: Vec<skills::RemoteSkill>,
    plugin_query: String,
    plugin_results: Vec<plugins::MarketPlugin>,

    // otomasyon / ayarlar
    auto_form: automation::Automation,
    settings: Config,
    chat_ids_text: String,
}

fn main_look(cfg: &Config) -> Look {
    let mut l = character::default_look(&cfg.main_name);
    l.kind = Kind::parse(&cfg.main_character).unwrap_or(Kind::Robot);
    if let Some(c) = character::parse_color(&cfg.main_color) {
        l.color = c;
    }
    l
}

fn palette_for(theme_name: &str) -> Palette {
    if theme_name == "light" { theme::light() } else { theme::dark() }
}

/// Dosyadan ek oluşturur: görsel → image bloğu, PDF → document bloğu, metin → text bloğu
fn make_attachment(path: &std::path::Path) -> anyhow::Result<Attachment> {
    let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
    let bytes = std::fs::read(path)?;
    let b64 = || base64::engine::general_purpose::STANDARD.encode(&bytes);
    let media = match ext.as_str() {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        _ => None,
    };
    let block = if let Some(m) = media {
        anyhow::ensure!(bytes.len() < 5 * 1024 * 1024, "görsel 5 MB'tan büyük");
        json!({"type": "image", "source": {"type": "base64", "media_type": m, "data": b64()}})
    } else if ext == "pdf" {
        anyhow::ensure!(bytes.len() < 30 * 1024 * 1024, "PDF 30 MB'tan büyük");
        json!({"type": "document", "source": {"type": "base64", "media_type": "application/pdf", "data": b64()}, "title": name})
    } else {
        let text = String::from_utf8(bytes).map_err(|_| anyhow::anyhow!("{name}: metin dosyası değil (desteklenen: metin, görsel, PDF)"))?;
        let text: String = text.chars().take(200_000).collect();
        json!({"type": "text", "text": format!("Ek dosya: {name} ({})\n```\n{text}\n```", path.display())})
    };
    Ok(Attachment { name, block })
}

fn greeting() -> &'static str {
    use chrono::Timelike;
    match chrono::Local::now().hour() {
        5..=11 => "Günaydın",
        12..=17 => "İyi günler",
        18..=22 => "İyi akşamlar",
        _ => "İyi geceler",
    }
}

/// Ortalanmış, en fazla `max_w` genişliğinde sütun
fn centered<R>(ui: &mut egui::Ui, max_w: f32, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let w = ui.available_width().min(max_w);
    let margin = ((ui.available_width() - w) / 2.0).max(0.0);
    ui.horizontal_top(|ui| {
        ui.add_space(margin);
        ui.vertical(|ui| {
            ui.set_width(w);
            add(ui)
        })
        .inner
    })
    .inner
}

fn character_widget(ui: &mut egui::Ui, look: &Look, mood: Mood, size: f32, t: f32) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(size), Sense::click());
    character::draw(&ui.painter_at(rect.expand(size * 0.08)), rect, look, mood, t);
    resp
}

fn heading(ui: &mut egui::Ui, text: &str, p: &Palette) {
    ui.label(RichText::new(text).font(theme::serif(26.0)).color(p.text));
}

fn section(ui: &mut egui::Ui, title: &str, p: &Palette) {
    ui.add_space(18.0);
    ui.label(RichText::new(title).font(theme::bold(15.5)).color(p.text));
    ui.add_space(2.0);
    let r = ui.available_rect_before_wrap();
    ui.painter().line_segment([r.left_top(), r.right_top()], Stroke::new(1.0, p.border));
    ui.add_space(8.0);
}

fn muted(text: impl Into<String>, p: &Palette) -> RichText {
    RichText::new(text.into()).color(p.muted).size(13.0)
}

fn text_input(ui: &mut egui::Ui, s: &mut String, hint: &str, width: f32) -> egui::Response {
    ui.add(egui::TextEdit::singleline(s).hint_text(hint).desired_width(width).margin(Vec2::new(8.0, 6.0)))
}

fn date_group(updated: &str) -> &'static str {
    let Ok(t) = chrono::DateTime::parse_from_rfc3339(updated) else { return "Daha eski" };
    let days = (chrono::Local::now().date_naive() - t.with_timezone(&chrono::Local).date_naive()).num_days();
    match days {
        0 => "Bugün",
        1 => "Dün",
        2..=7 => "Son 7 gün",
        8..=30 => "Son 30 gün",
        _ => "Daha eski",
    }
}

impl App {
    fn spawn<F>(&self, fut: F)
    where
        F: Future<Output = anyhow::Result<Msg>> + Send + 'static,
    {
        let tx = self.tx.clone();
        self.handle.spawn(async move {
            let m = fut.await.unwrap_or_else(|e| Msg::Error(format!("{e:#}")));
            let _ = tx.send(m);
        });
    }

    fn toast(&mut self, s: impl Into<String>, err: bool) {
        self.toast = Some(Toast { msg: s.into(), err, at: Instant::now(), undo: None });
    }

    fn push(&mut self, kind: EK, agent: &str, text: String) {
        self.entries.push(Entry { kind, agent: agent.into(), text, files: vec![] });
    }

    fn append(&mut self, kind: EK, agent: &str, text: &str) {
        if let Some(last) = self.entries.last_mut() {
            if last.kind == kind && last.agent == agent {
                last.text.push_str(text);
                return;
            }
        }
        self.push(kind, agent, text.to_string());
    }

    fn actor(&mut self, name: &str) -> &mut Actor {
        self.actors.entry(name.to_string()).or_insert_with(Actor::new)
    }

    fn look_of(&self, name: &str) -> Look {
        if name.is_empty() || name == MAIN_AGENT {
            return main_look(&self.rt.cfg);
        }
        self.agents.iter().find(|a| a.name == name).map(|a| a.look()).unwrap_or_else(|| character::default_look(name))
    }

    fn display_name(&self, name: &str) -> String {
        if name.is_empty() || name == MAIN_AGENT { self.rt.cfg.main_name.clone() } else { name.to_string() }
    }

    fn refresh_recents(&mut self) {
        self.recents = sessions::list()
            .unwrap_or_default()
            .into_iter()
            .map(|s| (s.id, s.title, s.updated))
            .collect();
        self.recents_at = Instant::now();
        self.model_ok = models::model_ready(&self.model);
    }

    /// Bağlı (anahtarı olan) sağlayıcıların gerçek sohbet modellerini arka planda çeker
    fn refresh_provider_models(&self) {
        let Ok(cfg) = Config::load() else { return };
        for (name, pr) in cfg.providers {
            if pr.base_url.contains("localhost") || pr.key().is_none() {
                continue;
            }
            let tx = self.tx.clone();
            self.handle.spawn(async move {
                if let Ok(list) = models::chat_models(&name).await {
                    let _ = tx.send(Msg::ProviderModels(name.clone(), models::rank_models(&name, list)));
                }
            });
        }
    }

    /// Sohbeti çöp kutusuna taşır ve "Geri al" bildirimi gösterir
    fn delete_session(&mut self, id: &str, title: &str) {
        match sessions::delete(id) {
            Ok(_) => {
                if self.current_id.as_deref() == Some(id) {
                    self.new_chat();
                }
                let short: String = title.chars().take(40).collect();
                self.toast = Some(Toast { msg: format!("\"{short}\" silindi"), err: false, at: Instant::now(), undo: Some(id.to_string()) });
            }
            Err(e) => self.toast(format!("{e:#}"), true),
        }
        self.refresh_recents();
    }

    /// Sohbeti Markdown dosyası olarak dışa aktarır
    fn export_chat(&mut self) {
        let mut md = format!("# {}\n\n", self.title);
        for e in &self.entries {
            match e.kind {
                EK::User => md.push_str(&format!("**Sen:** {}\n\n", e.text)),
                EK::Text => md.push_str(&format!("**{}:**\n\n{}\n\n", self.display_name(&e.agent), e.text)),
                EK::Tool => md.push_str(&format!("> araç: `{}`\n\n", e.text.chars().take(200).collect::<String>())),
                _ => {}
            }
        }
        let name: String = self.title.chars().filter(|c| c.is_alphanumeric() || *c == ' ' || *c == '-').take(50).collect();
        if let Some(path) = rfd::FileDialog::new().set_title("Sohbeti dışa aktar").set_file_name(format!("{}.md", name.trim())).add_filter("Markdown", &["md"]).save_file() {
            match std::fs::write(&path, md) {
                Ok(_) => self.toast(format!("Kaydedildi: {}", path.display()), false),
                Err(e) => self.toast(format!("{e}"), true),
            }
        }
    }

    /// Sohbeti verilen kullanıcı turuna geri sarar (o mesaj düzenlenmek üzere mesaj kutusuna gelir)
    fn rewind(&mut self, turn: usize) {
        if self.busy {
            self.toast("Önce çalışan işi durdur", true);
            return;
        }
        let _ = self.job_tx.send(Job::Rewind(turn));
    }

    fn new_chat(&mut self) {
        if self.busy {
            self.cancel.notify_waiters();
        }
        let _ = self.job_tx.send(Job::Reset);
        self.entries.clear();
        self.attachments.clear();
        self.title = "Yeni sohbet".into();
        self.current_id = None;
        self.page = Page::Chat;
    }

    fn open_session(&mut self, id: &str) {
        if self.busy {
            self.toast("Önce çalışan işi durdur", true);
            return;
        }
        self.entries.clear();
        let _ = self.job_tx.send(Job::Resume(Some(id.to_string())));
        self.page = Page::Chat;
    }

    fn add_attachment_path(&mut self, path: &std::path::Path) {
        match make_attachment(path) {
            Ok(a) => self.attachments.push(a),
            Err(e) => self.toast(format!("{e:#}"), true),
        }
    }

    /// Sağlayıcıyı tarayıcıdan bağlar (OpenRouter: gerçek giriş; diğerleri: anahtar yakalama)
    fn connect(&mut self, provider: &str) {
        let prov = provider.to_string();
        let tx = self.tx.clone();
        self.connecting = Some(match models::key_site(provider) {
            Some(k) => format!("{} açılıyor… {}", k.label, k.how),
            None => "OpenRouter açılıyor… giriş yapıp izin ver".into(),
        });
        self.spawn(async move {
            let p = tx.clone();
            let progress = move |s: String| {
                let _ = p.send(Msg::Connecting(s));
            };
            let msg = if prov == "openrouter" { models::login_openrouter().await } else { models::connect_with_browser(&prov, progress).await };
            match msg {
                Ok(m) => Ok(Msg::Connected(prov, m)),
                Err(e) => {
                    let _ = tx.send(Msg::Connecting(String::new()));
                    Err(e)
                }
            }
        });
    }

    fn handle_msg(&mut self, m: Msg) {
        match m {
            Msg::Rewound { turn, text, blocks } => {
                // Ekranda o kullanıcı mesajını ve sonrasını kaldır; metni düzenlenmek üzere kutuya koy
                if let Some(i) = self.entries.iter().enumerate().filter(|(_, e)| e.kind == EK::User).nth(turn).map(|(i, _)| i) {
                    self.entries.truncate(i);
                }
                self.attachments = blocks
                    .into_iter()
                    .map(|b| Attachment {
                        name: match b["type"].as_str() {
                            Some("image") => "görsel".into(),
                            Some("document") => b["title"].as_str().unwrap_or("PDF").to_string(),
                            _ => "dosya".into(),
                        },
                        block: b,
                    })
                    .collect();
                self.input = text;
                self.page = Page::Chat;
                self.toast("Sohbet geri sarıldı; mesajı düzenleyip yeniden gönderebilirsin", false);
            }
            Msg::ProviderModels(prov, list) => {
                self.provider_models.insert(prov, list);
            }
            Msg::Connecting(s) => self.connecting = if s.is_empty() { None } else { Some(s) },
            Msg::Connected(prov, msg) => {
                self.connecting = None;
                self.toast(msg.clone(), false);
                self.refresh_provider_models();
                // Mevcut model kullanılamıyorsa bu sağlayıcının önerilen modeline geç
                if !models::model_ready(&self.model) {
                    let m = models::recommended(&prov).to_string();
                    if models::set_default(&m).is_ok() {
                        let _ = self.job_tx.send(Job::SetModel(m.clone()));
                        self.model = m;
                    }
                }
                self.model_ok = models::model_ready(&self.model);
            }
            Msg::Agent(e) => self.on_event(e),
            Msg::TurnFinished => {
                self.busy = false;
                self.status.clear();
                let names: Vec<String> = self.actors.keys().cloned().collect();
                for n in names {
                    let a = self.actor(&n);
                    if n == MAIN_AGENT {
                        a.set(Mood::Happy, Some(2.5));
                    } else if a.mood != Mood::Idle {
                        a.set(Mood::Idle, None);
                    }
                }
                self.refresh_recents();
            }
            Msg::Info(s) => {
                self.toast(s.lines().next().unwrap_or("").to_string(), false);
                if self.page == Page::Chat {
                    self.push(EK::Info, "", s);
                }
            }
            Msg::Error(s) => {
                self.toast(s.lines().next().unwrap_or("").to_string(), true);
                self.mcp_loading = false;
                self.skill_loading = false;
                self.plugin_loading = false;
                // Diğer sayfalardaki hatalar sohbet akışına karışmasın
                if self.page == Page::Chat {
                    self.push(EK::Error, "", s);
                }
            }
            Msg::Model(m) => self.model = m,
            Msg::Meta { id, title } => {
                self.current_id = id;
                self.title = title;
                self.refresh_recents();
            }
            Msg::McpResults(r) => {
                self.mcp_results = r;
                self.mcp_loading = false;
            }
            Msg::McpTest(s) => self.mcp_test = s,
            Msg::SkillResults(r) => {
                self.skill_results = r;
                self.skill_loading = false;
            }
            Msg::PluginResults(r) => {
                self.plugin_results = r;
                self.plugin_loading = false;
            }
            Msg::HfResults(r) => self.hf_results = r,
            Msg::HfQuants(repo, q) => {
                let first = q.iter().find(|x| x.as_str() == "Q4_K_M").or(q.first()).cloned().unwrap_or_default();
                self.hf_quants.insert(repo, (q, first));
            }
            Msg::ModelList(l) => self.model_list = l,
            Msg::OllamaModels(l) => self.ollama_models = l,
            Msg::Progress(s) => self.progress = s,
            Msg::UserLine(s, files) => self.entries.push(Entry { kind: EK::User, agent: String::new(), text: s, files }),
        }
    }

    fn on_event(&mut self, e: AgentEvent) {
        let name = e.agent.clone();
        match e.kind {
            EventKind::Text(t) => {
                self.append(EK::Text, &name, &t);
                let a = self.actor(&name);
                a.set(Mood::Talking, Some(1.2));
                a.bubble.push_str(&t);
                let n = a.bubble.chars().count();
                if n > 140 {
                    a.bubble = a.bubble.chars().skip(n - 140).collect();
                }
            }
            EventKind::Thinking(t) => {
                self.append(EK::Thinking, &name, &t);
                self.actor(&name).set(Mood::Thinking, Some(4.0));
            }
            EventKind::ToolCall { name: tool, input } => {
                self.push(EK::Tool, &name, format!("{tool}  {input}"));
                let a = self.actor(&name);
                a.set(Mood::Working, Some(30.0));
                a.bubble = format!("{tool} kullanıyor");
                if tool == "delegate" {
                    if let Ok(v) = serde_json::from_str::<Value>(input.trim_end_matches('…')) {
                        if let Some(target) = v["agent"].as_str() {
                            let t = self.actor(target);
                            t.set(Mood::Thinking, Some(20.0));
                            t.bubble = "görev aldım".into();
                        }
                    }
                }
            }
            EventKind::ToolResult { name: tool, summary, is_error } => {
                self.push(if is_error { EK::ToolErr } else { EK::ToolOk }, &name, format!("{tool}: {summary}"));
                if is_error {
                    self.actor(&name).set(Mood::Error, Some(2.5));
                }
            }
            EventKind::TurnDone { input_tokens, output_tokens } => {
                self.tokens.0 += input_tokens;
                self.tokens.1 += output_tokens;
            }
            EventKind::Error(s) => {
                self.push(EK::Error, &name, s);
                self.actor(&name).set(Mood::Error, Some(4.0));
            }
            EventKind::Info(s) => {
                self.toast(s.clone(), false);
                self.push(EK::Info, &name, s);
            }
            EventKind::Approval(h) => {
                let a = self.actor(&name);
                a.set(Mood::Waiting, None);
                a.bubble = "onayın gerekiyor".into();
                self.approvals.push_back(h);
            }
            EventKind::Status(s) => self.status = s,
            EventKind::ModelSwitched(m) => {
                self.status.clear();
                if name == MAIN_AGENT {
                    self.model = m;
                    self.model_ok = true;
                }
            }
        }
    }

    fn send_input(&mut self) {
        let text = self.input.trim().to_string();
        if text.is_empty() && self.attachments.is_empty() {
            return;
        }
        self.input.clear();
        if let Some(cmd) = text.strip_prefix('/') {
            let args = commands::split_args(cmd);
            match args.first().map(|s| s.as_str()) {
                Some("new") | Some("reset") | Some("clear") => self.new_chat(),
                Some("compact") if !self.busy => {
                    self.busy = true;
                    let _ = self.job_tx.send(Job::Compact);
                }
                Some("continue") if !self.busy => {
                    self.entries.clear();
                    let _ = self.job_tx.send(Job::Resume(None));
                }
                Some("resume") if args.len() == 2 => {
                    let id = args[1].clone();
                    self.open_session(&id);
                }
                Some("mode") if args.len() == 2 => {
                    if let Some(m) = crate::permissions::parse_mode(&args[1]) {
                        self.rt.perms.write().unwrap().mode = m;
                        let _ = crate::permissions::set_mode(m);
                        self.toast(format!("İzin modu: {}", args[1]), false);
                    }
                }
                _ => {
                    self.push(EK::Command, "", text.clone());
                    if args.len() == 2 && args[0] == "model" {
                        let _ = self.job_tx.send(Job::SetModel(args[1].clone()));
                        self.model = args[1].clone();
                    }
                    let cfg = self.rt.cfg.clone();
                    let tx = self.tx.clone();
                    self.spawn(async move {
                        let p = tx.clone();
                        let progress = move |s: String| {
                            let _ = p.send(Msg::Progress(s));
                        };
                        Ok(Msg::Info(commands::run(&cfg, &args, &progress).await?))
                    });
                }
            }
            return;
        }
        if self.busy {
            self.toast("Agent hâlâ çalışıyor; bitince gönder veya durdur.", true);
            self.input = text;
            return;
        }
        let files: Vec<String> = self.attachments.iter().map(|a| a.name.clone()).collect();
        let blocks: Vec<Value> = self.attachments.drain(..).map(|a| a.block).collect();
        self.entries.push(Entry { kind: EK::User, agent: String::new(), text: text.clone(), files });
        let a = self.actor(MAIN_AGENT);
        a.set(Mood::Thinking, Some(30.0));
        a.bubble.clear();
        self.busy = true;
        self.page = Page::Chat;
        let _ = self.job_tx.send(Job::Prompt(if text.is_empty() { "Ekteki dosyalara bak.".into() } else { text }, blocks));
    }

    fn regenerate(&mut self) {
        if self.busy {
            return;
        }
        // Son kullanıcı mesajından sonrasını ekrandan sil
        if let Some(i) = self.entries.iter().rposition(|e| e.kind == EK::User) {
            self.entries.truncate(i + 1);
        }
        self.busy = true;
        self.actor(MAIN_AGENT).set(Mood::Thinking, Some(30.0));
        let _ = self.job_tx.send(Job::Regenerate);
    }
}

// ------------------------------------------------------------------ kenar çubuğu

impl App {
    fn sidebar(&mut self, ui: &mut egui::Ui, t: f32) {
        let p = self.pal;
        let collapsed = !self.sidebar_open;
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            if !collapsed {
                let ml = main_look(&self.rt.cfg);
                let mood = self.actor(MAIN_AGENT).current();
                character_widget(ui, &ml, mood, 28.0, t);
                ui.label(RichText::new("RustyAgent").font(theme::serif(18.0)).color(p.text));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if theme::icon_button(ui, Icon::Sidebar, "Kenar çubuğunu daralt (Ctrl+B)", &p).clicked() {
                        self.sidebar_open = false;
                    }
                });
            } else if theme::icon_button(ui, Icon::Sidebar, "Kenar çubuğunu aç (Ctrl+B)", &p).clicked() {
                self.sidebar_open = true;
            }
        });
        ui.add_space(8.0);

        // Yeni sohbet
        let (rect, resp) = ui.allocate_exact_size(Vec2::new(if collapsed { 40.0 } else { ui.available_width() }, 36.0), Sense::click());
        if resp.hovered() {
            ui.painter().rect_filled(rect, CornerRadius::same(8), p.hover);
        }
        let ic = egui::Rect::from_center_size(egui::pos2(rect.left() + 20.0, rect.center().y), Vec2::splat(22.0));
        ui.painter().circle_filled(ic.center(), 11.0, p.accent);
        theme::draw_icon(ui.painter(), ic.shrink(5.0), Icon::Plus, p.on_accent);
        if !collapsed {
            ui.painter().text(egui::pos2(rect.left() + 40.0, rect.center().y), egui::Align2::LEFT_CENTER, "Yeni sohbet", theme::bold(14.5), p.accent);
        }
        if resp.on_hover_text("Yeni sohbet (Ctrl+N)").clicked() {
            self.new_chat();
        }
        ui.add_space(6.0);

        for (page, icon, label) in NAV {
            let mut label = label.to_string();
            if page == Page::Chat && !self.approvals.is_empty() {
                label = format!("Sohbet  ({} onay)", self.approvals.len());
            }
            if theme::nav_item(ui, icon, &label, self.page == page, collapsed, &p).clicked() {
                self.goto(page);
            }
        }

        if !collapsed {
            ui.add_space(14.0);
            ui.add(
                egui::TextEdit::singleline(&mut self.recents_filter)
                    .hint_text("Sohbetlerde ara")
                    .desired_width(ui.available_width())
                    .margin(Vec2::new(8.0, 5.0)),
            );
            ui.add_space(4.0);
            if self.recents_at.elapsed() > Duration::from_secs(5) {
                self.refresh_recents();
            }
            let filter = brain::fold(&self.recents_filter);
            let recents = self.recents.clone();
            let bottom_reserve = 50.0;
            egui::ScrollArea::vertical()
                .id_salt("recents")
                .max_height((ui.available_height() - bottom_reserve).max(60.0))
                .show(ui, |ui| {
                    let mut last_group = "";
                    for (id, title, updated) in recents.iter().filter(|r| filter.is_empty() || brain::fold(&r.1).contains(&filter)) {
                        let g = date_group(updated);
                        if g != last_group {
                            ui.add_space(8.0);
                            ui.label(RichText::new(g).size(12.0).color(p.faint));
                            last_group = g;
                        }
                        let sel = self.current_id.as_deref() == Some(id.as_str());
                        let (rect, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 28.0), Sense::click());
                        if sel {
                            ui.painter().rect_filled(rect, CornerRadius::same(6), p.surface2);
                        } else if resp.hovered() {
                            ui.painter().rect_filled(rect, CornerRadius::same(6), p.hover);
                        }
                        let max_chars = ((rect.width() - 36.0) / 7.2) as usize;
                        let shown: String = if title.chars().count() > max_chars { format!("{}…", title.chars().take(max_chars).collect::<String>()) } else { title.clone() };
                        ui.painter().text(egui::pos2(rect.left() + 8.0, rect.center().y), egui::Align2::LEFT_CENTER, shown, egui::FontId::proportional(13.5), if sel { p.text } else { p.muted.lerp_to_gamma(p.text, 0.3) });
                        // Silme ikonu (üzerine gelince)
                        let del = egui::Rect::from_center_size(egui::pos2(rect.right() - 14.0, rect.center().y), Vec2::splat(20.0));
                        let mut deleted = false;
                        if resp.hovered() || ui.rect_contains_pointer(del) {
                            let dr = ui.interact(del, ui.scope_id().with(("del", id)), Sense::click());
                            theme::draw_icon(ui.painter(), del.shrink(4.0), Icon::Trash, if dr.hovered() { p.danger } else { p.faint });
                            if dr.on_hover_text("Sil (geri alınabilir)").clicked() {
                                self.delete_session(id, title);
                                deleted = true;
                            }
                        }
                        if !deleted && resp.on_hover_text(title).clicked() {
                            let id = id.clone();
                            self.open_session(&id);
                        }
                    }
                    if self.recents.is_empty() {
                        ui.label(muted("Henüz sohbet yok", &p));
                    }
                });
        }

        ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
            ui.add_space(8.0);
            if theme::nav_item(ui, Icon::Gear, "Ayarlar", self.page == Page::Settings, collapsed, &p).clicked() {
                self.goto(Page::Settings);
            }
        });
    }

    fn load_skills(&mut self) {
        self.skill_loading = true;
        let src = self.rt.cfg.skill_sources.clone();
        self.spawn(async move { Ok(Msg::SkillResults(skills::browse(&src, "").await?)) });
    }

    fn load_plugins(&mut self) {
        self.plugin_loading = true;
        let m = self.rt.cfg.plugin_marketplaces.clone();
        self.spawn(async move { Ok(Msg::PluginResults(plugins::browse(&m, "").await?)) });
    }

    fn search_mcp(&mut self) {
        self.mcp_typed = None;
        self.mcp_loading = true;
        let q = self.mcp_query.trim().to_string();
        self.spawn(async move { Ok(Msg::McpResults(mcp::browse(&q).await?)) });
    }

    fn goto(&mut self, page: Page) {
        match page {
            Page::Settings => {
                self.settings = Config::load().unwrap_or_else(|_| self.rt.cfg.clone());
                self.chat_ids_text = self.settings.telegram.allowed_chat_ids.iter().map(|i| i.to_string()).collect::<Vec<_>>().join(", ");
            }
            Page::Agents | Page::Chat | Page::Automations => self.agents = agent::load_agents(),
            // Listeleri sayfa açılınca kendiliğinden yükle (Ara'ya basmaya gerek kalmasın)
            Page::Skills if self.skill_results.is_empty() && !self.skill_loading => self.load_skills(),
            Page::Plugins if self.plugin_results.is_empty() && !self.plugin_loading => self.load_plugins(),
            Page::Mcp if self.mcp_results.is_empty() && !self.mcp_loading => self.search_mcp(),
            _ => {}
        }
        self.page = page;
    }
}

// ------------------------------------------------------------------ sohbet

impl App {
    fn page_chat(&mut self, ui: &mut egui::Ui, t: f32) {
        let p = self.pal;
        // Üst çubuk
        egui::Panel::top("chat_top").frame(egui::Frame::new().fill(p.bg).inner_margin(egui::Margin::symmetric(16, 8))).show(ui, |ui| {
            ui.horizontal(|ui| {
                // Başlık: tıklayınca yeniden adlandır
                let has_chat = self.current_id.is_some();
                if let Some(mut draft) = self.renaming.take() {
                    let r = ui.add(egui::TextEdit::singleline(&mut draft).desired_width(360.0).font(egui::FontId::proportional(15.0)));
                    r.request_focus();
                    let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
                    let esc = ui.input(|i| i.key_pressed(egui::Key::Escape));
                    if enter && !draft.trim().is_empty() {
                        self.title = draft.trim().to_string();
                        let _ = self.job_tx.send(Job::Rename(self.title.clone()));
                    } else if !esc && !r.lost_focus() {
                        self.renaming = Some(draft);
                    }
                } else {
                    let r = ui.add(egui::Label::new(RichText::new(&self.title).size(15.0).color(p.text)).sense(Sense::click()));
                    if has_chat && r.on_hover_text("Yeniden adlandırmak için tıkla").clicked() {
                        self.renaming = Some(self.title.clone());
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if theme::icon_button(ui, Icon::Team, if self.team_open { "Ekip panelini gizle" } else { "Ekip panelini göster" }, &p).clicked() {
                        self.team_open = !self.team_open;
                    }
                    if !self.entries.is_empty() {
                        if has_chat && theme::icon_button(ui, Icon::Trash, "Sohbeti sil (geri alınabilir)", &p).clicked() {
                            let (id, title) = (self.current_id.clone().unwrap_or_default(), self.title.clone());
                            self.delete_session(&id, &title);
                        }
                        if theme::icon_button(ui, Icon::Download, "Markdown olarak dışa aktar", &p).clicked() {
                            self.export_chat();
                        }
                        let turns = self.entries.iter().filter(|e| e.kind == EK::User).count();
                        if turns > 0 && !self.busy && theme::icon_button(ui, Icon::Undo, "Son mesajı geri al", &p).clicked() {
                            self.rewind(turns - 1);
                        }
                    }
                    if self.busy {
                        ui.spinner();
                        ui.label(muted("çalışıyor", &p));
                    }
                });
            });
        });

        if self.team_open {
            egui::Panel::right("team")
                .exact_size(250.0)
                .frame(egui::Frame::new().fill(p.sidebar).inner_margin(12).stroke(Stroke::new(1.0, p.border)))
                .show(ui, |ui| self.team_panel(ui, t));
        }

        let empty = self.entries.is_empty();
        if !empty {
            egui::Panel::bottom("composer").frame(egui::Frame::new().fill(p.bg).inner_margin(egui::Margin { left: 16, right: 16, top: 4, bottom: 14 })).show(ui, |ui| {
                centered(ui, 780.0, |ui| self.composer(ui));
                ui.vertical_centered(|ui| {
                    let mode = format!("{:?}", self.rt.perms.read().unwrap().mode).to_lowercase();
                    ui.label(RichText::new(format!("izin modu: {mode}   ·   {} / {} token", self.tokens.0, self.tokens.1)).size(11.5).color(p.faint));
                });
            });
        }

        egui::CentralPanel::default().frame(egui::Frame::new().fill(p.bg)).show(ui, |ui| {
            if empty {
                self.empty_state(ui, t);
                return;
            }
            egui::ScrollArea::vertical().id_salt("chat").stick_to_bottom(true).auto_shrink([false; 2]).show(ui, |ui| {
                ui.add_space(16.0);
                centered(ui, 760.0, |ui| self.transcript(ui, t));
                ui.add_space(24.0);
            });
        });
    }

    fn empty_state(&mut self, ui: &mut egui::Ui, t: f32) {
        let p = self.pal;
        ui.add_space((ui.available_height() * 0.22).max(30.0));
        centered(ui, 720.0, |ui| {
            ui.vertical_centered(|ui| {
                let ml = main_look(&self.rt.cfg);
                let mood = self.actor(MAIN_AGENT).current();
                character_widget(ui, &ml, mood, 84.0, t);
                ui.add_space(6.0);
                ui.label(RichText::new(format!("{}, ben {}", greeting(), self.rt.cfg.main_name)).font(theme::serif(32.0)).color(p.text));
                ui.add_space(18.0);
            });
            self.composer(ui);
            ui.add_space(14.0);
            if !self.model_ok {
                egui::Frame::new().fill(p.surface).stroke(Stroke::new(1.0, p.accent.gamma_multiply(0.6))).corner_radius(14).inner_margin(16).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(RichText::new("Başlamak için bir yapay zekâ bağla").font(theme::serif(19.0)).color(p.text));
                    ui.label(muted(format!("Şu anki model ({}) kullanılamıyor. Hesabınla tarayıcıdan bağlan; anahtar otomatik yakalanır.", self.model), &p));
                    ui.add_space(8.0);
                    if let Some(c) = &self.connecting {
                        ui.horizontal_top(|ui| {
                            ui.spinner();
                            ui.add(egui::Label::new(RichText::new(c).size(13.0).color(p.text)).wrap_mode(egui::TextWrapMode::Wrap));
                        });
                        ui.add_space(6.0);
                    }
                    ui.horizontal_wrapped(|ui| {
                        let busy = self.connecting.is_some();
                        for (prov, label) in [
                            ("gemini", "Google ile Gemini (ücretsiz)"),
                            ("openrouter", "OpenRouter ile giriş"),
                            ("anthropic", "Claude"),
                            ("openai", "OpenAI"),
                            ("groq", "Groq (ücretsiz)"),
                            ("xai", "Grok"),
                        ] {
                            let b = if prov == "gemini" { theme::primary_button(ui, label, &p) } else { theme::ghost_button(ui, label, &p) };
                            if b.clicked() && !busy {
                                self.connect(prov);
                            }
                        }
                        if theme::ghost_button(ui, "Yerel model indir", &p).clicked() {
                            self.goto(Page::Models);
                        }
                    });
                });
                ui.add_space(12.0);
            }
            ui.horizontal_wrapped(|ui| {
                let suggestions = [
                    (Icon::Search, "Araştır", "Şu konuyu araştır ve kaynaklı özet çıkar: "),
                    (Icon::Edit, "Kod yaz", "Bu projede şunu yap: "),
                    (Icon::Chip, "Bilgisayarımda yap", "Bilgisayarımda şunu yap: "),
                    (Icon::Brain, "Hafızam", "Hafızanda benimle ilgili neler var? Kısaca özetle."),
                    (Icon::Clock, "Hatırlatıcı kur", "Her sabah 9'da bana şunu hatırlatan bir otomasyon kur: "),
                ];
                for (icon, label, prompt) in suggestions {
                    if theme::icon_text_button(ui, icon, label, &p).clicked() {
                        self.input = prompt.into();
                    }
                }
            });
        });
    }

    /// Mesaj kutusu: ekler, metin, ek butonu, model seçici, gönder/durdur
    fn composer(&mut self, ui: &mut egui::Ui) {
        let p = self.pal;
        let id = egui::Id::unique("composer_input");
        let enter = ui.memory(|m| m.has_focus(id)) && ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter));
        egui::Frame::new()
            .fill(p.surface)
            .stroke(Stroke::new(1.0, p.border))
            .corner_radius(18)
            .inner_margin(egui::Margin { left: 14, right: 10, top: 10, bottom: 8 })
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                if !self.attachments.is_empty() {
                    let mut remove = None;
                    ui.horizontal_wrapped(|ui| {
                        for (i, a) in self.attachments.iter().enumerate() {
                            egui::Frame::new().fill(p.surface2).stroke(Stroke::new(1.0, p.border)).corner_radius(8).inner_margin(egui::Margin::symmetric(8, 4)).show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    ui.label(RichText::new(&a.name).size(12.5).color(p.text));
                                    if theme::icon_button(ui, Icon::Close, "Kaldır", &p).clicked() {
                                        remove = Some(i);
                                    }
                                });
                            });
                        }
                    });
                    if let Some(i) = remove {
                        self.attachments.remove(i);
                    }
                    ui.add_space(4.0);
                }
                ui.add(
                    egui::TextEdit::multiline(&mut self.input)
                        .id(id)
                        .frame(egui::Frame::NONE)
                        .desired_rows(2)
                        .desired_width(f32::INFINITY)
                        .font(egui::FontId::proportional(15.0))
                        .hint_text(RichText::new("Bir şey sor veya bir iş ver…  (Enter gönderir, Shift+Enter yeni satır, / komutlar)").color(p.faint)),
                );
                ui.horizontal(|ui| {
                    if theme::icon_button(ui, Icon::Clip, "Dosya ekle (görsel, PDF, metin) — sürükleyip bırakabilirsin", &p).clicked() {
                        if let Some(files) = rfd::FileDialog::new().set_title("Dosya ekle").pick_files() {
                            for f in files {
                                self.add_attachment_path(&f);
                            }
                        }
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        // Gönder / durdur
                        let (rect, resp) = ui.allocate_exact_size(Vec2::splat(32.0), Sense::click());
                        let can_send = !self.input.trim().is_empty() || !self.attachments.is_empty();
                        let fill = if self.busy { p.text } else if can_send { p.accent } else { p.accent.gamma_multiply(0.45) };
                        ui.painter().rect_filled(rect, CornerRadius::same(10), fill);
                        theme::draw_icon(ui.painter(), rect.shrink(8.0), if self.busy { Icon::Stop } else { Icon::Send }, if self.busy { p.bg } else { p.on_accent });
                        if resp.on_hover_text(if self.busy { "Durdur (Esc)" } else { "Gönder (Enter)" }).clicked() {
                            if self.busy {
                                self.cancel.notify_waiters();
                            } else {
                                self.send_input();
                            }
                        }
                        ui.add_space(6.0);
                        self.model_picker(ui);
                    });
                });
            });
        if enter {
            self.send_input();
            ui.memory_mut(|m| m.request_focus(id));
        }
    }

    fn model_picker(&mut self, ui: &mut egui::Ui) {
        let p = self.pal;
        let short = self.model.rsplit('/').next().unwrap_or(&self.model).to_string();
        let mut choice: Option<String> = None;
        egui::ComboBox::from_id_salt("model_pick")
            .selected_text(RichText::new(short).size(13.0).color(p.muted))
            .width(190.0)
            .show_ui(ui, |ui| {
                // Bağlı sağlayıcıların GERÇEK model listesi (sabit adlar eskiyip hata veriyordu)
                let mut provs: Vec<(String, Vec<String>)> = self.provider_models.iter().filter(|(_, l)| !l.is_empty()).map(|(k, v)| (k.clone(), v.clone())).collect();
                provs.sort();
                for (prov, list) in &provs {
                    ui.label(RichText::new(prov).size(11.5).color(p.faint));
                    let quick: Vec<String> = if prov == "openrouter" {
                        // OpenRouter'da yüzlerce model var; öne çıkanlar (tam liste Modeller sayfasında)
                        ["openrouter/openai/gpt-5", "openrouter/google/gemini-3.1-pro-preview", "openrouter/anthropic/claude-sonnet-4.5", "openrouter/deepseek/deepseek-chat"]
                            .iter().map(|s| s.to_string()).filter(|m| list.contains(m)).collect()
                    } else {
                        list.iter().take(8).cloned().collect()
                    };
                    for m in quick {
                        let short = m.split_once('/').map(|x| x.1).unwrap_or(&m).to_string();
                        if ui.selectable_label(self.model == m, short).clicked() {
                            choice = Some(m.clone());
                        }
                    }
                }
                if provs.is_empty() && self.ollama_models.is_empty() {
                    ui.label(RichText::new("Bağlı model yok").size(12.5).color(p.muted));
                }
                if !self.ollama_models.is_empty() {
                    ui.label(RichText::new("Yerel (Ollama)").size(11.5).color(p.faint));
                    for m in self.ollama_models.clone() {
                        if ui.selectable_label(self.model == m, m.trim_start_matches("ollama/")).clicked() {
                            choice = Some(m.clone());
                        }
                    }
                }
                ui.separator();
                if ui.selectable_label(false, "Diğer modeller ve giriş…").clicked() {
                    self.page = Page::Models;
                }
            });
        if let Some(m) = choice {
            match models::set_default(&m) {
                Ok(_) => {
                    let _ = self.job_tx.send(Job::SetModel(m.clone()));
                    self.model_ok = models::model_ready(&m);
                    if !self.model_ok {
                        let prov = m.split('/').next().unwrap_or("").to_string();
                        self.toast(format!("{prov} bağlı değil — Modeller sayfasından \"Tarayıcıdan bağlan\" ile bağlayabilirsin"), true);
                    }
                    self.model = m;
                }
                Err(e) => self.toast(format!("{e:#}"), true),
            }
        }
    }

    fn avatar_row(&self, ui: &mut egui::Ui, agent: &str, t: f32) {
        let look = self.look_of(agent);
        ui.horizontal(|ui| {
            character_widget(ui, &look, Mood::Idle, 24.0, t);
            ui.label(RichText::new(self.display_name(agent)).font(theme::bold(13.5)).color(look.color));
        });
    }

    fn transcript(&mut self, ui: &mut egui::Ui, t: f32) {
        let p = self.pal;
        let last_text = self.entries.iter().rposition(|e| e.kind == EK::Text);
        let mut regen = false;
        let mut rewind: Option<usize> = None;
        let mut user_turn = 0usize;
        let n = self.entries.len();
        let mut prev_agent: Option<String> = None;
        for i in 0..n {
            let e = &self.entries[i];
            match e.kind {
                EK::User => {
                    prev_agent = None;
                    ui.add_space(14.0);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                        egui::Frame::new().fill(p.user_bubble).corner_radius(14).inner_margin(egui::Margin::symmetric(14, 10)).show(ui, |ui| {
                            ui.set_max_width(560.0);
                            ui.vertical(|ui| {
                                if !e.files.is_empty() {
                                    ui.horizontal_wrapped(|ui| {
                                        for f in &e.files {
                                            egui::Frame::new().fill(p.surface2).corner_radius(6).inner_margin(egui::Margin::symmetric(6, 2)).show(ui, |ui| {
                                                ui.label(RichText::new(f).size(12.0).color(p.muted));
                                            });
                                        }
                                    });
                                }
                                if !e.text.is_empty() {
                                    ui.label(RichText::new(&e.text).size(15.0).color(p.text));
                                }
                            });
                        });
                    });
                    // Mesaj eylemleri: düzenle (buradan itibaren geri sar) ve kopyala
                    let this_turn = user_turn;
                    user_turn += 1;
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                        if theme::icon_button(ui, Icon::Copy, "Kopyala", &p).clicked() {
                            ui.ctx().copy_text(e.text.clone());
                        }
                        if !self.busy && theme::icon_button(ui, Icon::Edit, "Düzenle: sohbeti bu mesaja geri sar ve yeniden yaz", &p).clicked() {
                            rewind = Some(this_turn);
                        }
                    });
                    ui.add_space(2.0);
                }
                EK::Command => {
                    ui.add_space(6.0);
                    ui.label(RichText::new(format!("› {}", e.text)).monospace().size(12.5).color(p.faint));
                }
                EK::Text => {
                    if prev_agent.as_deref() != Some(e.agent.as_str()) {
                        ui.add_space(6.0);
                        self.avatar_row(ui, &e.agent, t);
                        prev_agent = Some(e.agent.clone());
                    }
                    let sub = !e.agent.is_empty() && e.agent != MAIN_AGENT;
                    let text = e.text.clone();
                    let body = |ui: &mut egui::Ui| markdown::render(ui, &text, &p, 15.5, true);
                    if sub {
                        egui::Frame::new().fill(p.surface).stroke(Stroke::new(1.0, p.border)).corner_radius(10).inner_margin(10).show(ui, body);
                    } else {
                        egui::Frame::new().inner_margin(egui::Margin { left: 2, right: 0, top: 2, bottom: 0 }).show(ui, body);
                    }
                    ui.horizontal(|ui| {
                        if theme::icon_button(ui, Icon::Copy, "Kopyala", &p).clicked() {
                            ui.ctx().copy_text(text.clone());
                        }
                        if Some(i) == last_text && !self.busy && theme::icon_button(ui, Icon::Retry, "Yeniden oluştur", &p).clicked() {
                            regen = true;
                        }
                    });
                }
                EK::Thinking => {
                    let text = e.text.clone();
                    egui::CollapsingHeader::new(RichText::new("Düşünce süreci").size(13.0).color(p.faint))
                        .id_salt(("think", i))
                        .show(ui, |ui| {
                            ui.label(RichText::new(text).size(13.0).italics().color(p.muted));
                        });
                }
                EK::Tool => {
                    ui.horizontal(|ui| {
                        let (r, _) = ui.allocate_exact_size(Vec2::splat(14.0), Sense::hover());
                        theme::draw_icon(ui.painter(), r, Icon::Chip, p.faint);
                        let who = if e.agent.is_empty() || e.agent == MAIN_AGENT { String::new() } else { format!("{} · ", e.agent) };
                        let short: String = e.text.chars().take(140).collect();
                        ui.label(RichText::new(format!("{who}{short}")).monospace().size(12.0).color(p.muted));
                    });
                }
                EK::ToolOk | EK::ToolErr => {
                    let col = if e.kind == EK::ToolErr { p.danger } else { p.faint };
                    let short: String = e.text.chars().take(160).collect();
                    ui.label(RichText::new(format!("    {short}")).monospace().size(11.5).color(col));
                }
                EK::Info => {
                    ui.label(RichText::new(&e.text).size(13.0).color(p.muted));
                }
                EK::Error => {
                    egui::Frame::new().fill(p.danger.gamma_multiply(0.12)).corner_radius(8).inner_margin(10).show(ui, |ui| {
                        ui.label(RichText::new(&e.text).size(13.5).color(p.danger));
                    });
                }
            }
        }
        if self.busy {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(muted(if self.status.is_empty() { "düşünüyor…" } else { self.status.as_str() }, &p));
            });
        }
        if regen {
            self.regenerate();
        }
        if let Some(turn) = rewind {
            self.rewind(turn);
        }
    }

    fn team_panel(&mut self, ui: &mut egui::Ui, t: f32) {
        let p = self.pal;
        ui.horizontal(|ui| {
            ui.label(RichText::new("Ekip").font(theme::bold(14.0)).color(p.text));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if theme::icon_button(ui, Icon::Close, "Gizle", &p).clicked() {
                    self.team_open = false;
                }
            });
        });
        ui.add_space(6.0);
        let ml = main_look(&self.rt.cfg);
        let mood = self.actor(MAIN_AGENT).current();
        let bubble_text = self.actor(MAIN_AGENT).bubble.clone();
        ui.vertical_centered(|ui| {
            if matches!(mood, Mood::Talking | Mood::Working | Mood::Waiting) && !bubble_text.is_empty() {
                bubble(ui, &bubble_text, &p);
            }
            character_widget(ui, &ml, mood, 120.0, t);
            ui.label(RichText::new(&self.rt.cfg.main_name).font(theme::bold(15.0)).color(ml.color));
            ui.label(muted(mood.label(), &p));
        });
        ui.add_space(8.0);
        let agents = self.agents.clone();
        egui::ScrollArea::vertical().id_salt("team_scroll").show(ui, |ui| {
            egui::Grid::new("team_grid").num_columns(2).spacing([6.0, 10.0]).show(ui, |ui| {
                for (i, a) in agents.iter().enumerate() {
                    let look = a.look();
                    let mood = self.actor(&a.name).current();
                    let b = self.actor(&a.name).bubble.clone();
                    ui.vertical_centered(|ui| {
                        ui.set_width(105.0);
                        if matches!(mood, Mood::Talking | Mood::Working | Mood::Thinking | Mood::Waiting) && !b.is_empty() {
                            let tail: String = b.chars().rev().take(36).collect::<Vec<_>>().into_iter().rev().collect();
                            bubble(ui, &tail, &p);
                        }
                        let r = character_widget(ui, &look, mood, 72.0, t + i as f32 * 0.7);
                        if r.on_hover_text(format!("{}\nTıkla: bu agent'a iş ver", a.description)).clicked() {
                            self.input = format!("{} agent'ına şu işi ver: ", a.name);
                        }
                        ui.label(RichText::new(&a.name).size(12.5).color(look.color));
                        ui.label(RichText::new(mood.label()).size(11.0).color(p.faint));
                    });
                    if i % 2 == 1 {
                        ui.end_row();
                    }
                }
            });
            if agents.is_empty() {
                ui.label(muted("Henüz agent yok. Agent'lar sayfasından ekleyebilirsin.", &p));
            }
        });
    }

    fn approval_modal(&mut self, ctx: &egui::Context, t: f32) {
        let Some(h) = self.approvals.front().cloned() else { return };
        let p = self.pal;
        let look = self.look_of(&h.req.agent);
        let danger = h.req.risk == Risk::Danger;
        let mut decision = None;
        egui::Modal::new(egui::Id::unique("approval"))
            .frame(egui::Frame::new().fill(p.surface).stroke(Stroke::new(1.0, p.border)).corner_radius(16).inner_margin(20))
            .show(ctx, |ui| {
                ui.set_width(540.0);
                ui.horizontal(|ui| {
                    character_widget(ui, &look, Mood::Waiting, 96.0, t);
                    ui.add_space(8.0);
                    ui.vertical(|ui| {
                        ui.horizontal(|ui| {
                            let (r, _) = ui.allocate_exact_size(Vec2::splat(18.0), Sense::hover());
                            theme::draw_icon(ui.painter(), r, Icon::Warn, if danger { p.danger } else { p.warn });
                            ui.label(RichText::new(if danger { "Tehlikeli işlem" } else { "Onay gerekiyor" }).font(theme::serif(21.0)).color(p.text));
                        });
                        ui.label(RichText::new(format!("{} şunu yapmak istiyor:", self.display_name(&h.req.agent))).color(p.muted));
                        ui.add_space(6.0);
                        egui::Frame::new().fill(p.code_bg).corner_radius(8).inner_margin(10).show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.label(RichText::new(&h.req.summary).monospace().size(13.0).color(p.text));
                        });
                        if self.approvals.len() > 1 {
                            ui.label(muted(format!("{} istek daha bekliyor", self.approvals.len() - 1), &p));
                        }
                    });
                });
                ui.add_space(14.0);
                ui.horizontal(|ui| {
                    if theme::primary_button(ui, "  İzin ver  ", &p).clicked() {
                        decision = Some(Decision::Allow);
                    }
                    if !danger {
                        if theme::ghost_button(ui, "Bu oturumda hep", &p).on_hover_text(format!("Bu çalıştırma boyunca sorma: {}", h.req.grant_key)).clicked() {
                            decision = Some(Decision::AllowSession);
                        }
                        if theme::ghost_button(ui, "Her zaman", &p).on_hover_text(format!("Kalıcı izin kuralı ekle: {}", h.req.grant_key)).clicked() {
                            decision = Some(Decision::AllowAlways);
                        }
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if theme::ghost_button(ui, "Reddet", &p).clicked() {
                            decision = Some(Decision::Deny);
                        }
                    });
                });
            });
        if let Some(d) = decision {
            h.respond(d);
            self.approvals.pop_front();
            let a = self.actor(&h.req.agent);
            if d == Decision::Deny {
                a.set(Mood::Error, Some(2.0));
            } else {
                a.set(Mood::Working, Some(10.0));
            }
            let word = if d == Decision::Deny { "Reddedildi" } else { "İzin verildi" };
            self.push(EK::Info, &h.req.agent, format!("{word}: {}", h.req.summary));
        }
    }
}

fn bubble(ui: &mut egui::Ui, text: &str, p: &Palette) {
    egui::Frame::new()
        .fill(p.surface)
        .stroke(Stroke::new(1.0, p.border))
        .corner_radius(10)
        .inner_margin(egui::Margin::symmetric(8, 5))
        .show(ui, |ui| {
            ui.label(RichText::new(text.trim()).size(12.0).color(p.text));
        });
}

// ------------------------------------------------------------------ hafıza (second brain)

impl App {
    fn page_brain(&mut self, ui: &mut egui::Ui) {
        let p = self.pal;
        egui::CentralPanel::default().frame(egui::Frame::new().fill(p.bg).inner_margin(egui::Margin::symmetric(28, 20))).show(ui, |ui| {
            let notes = brain::list();
            ui.horizontal(|ui| {
                heading(ui, "Hafıza", &p);
                ui.add_space(12.0);
                ui.label(muted(format!("{} not · {} sohbet", notes.len(), self.recents.len()), &p));
            });
            ui.label(muted("Agent'ların seninle ilgili öğrendikleri, kaydettikleri notlar ve geçmiş sohbet özetleri. Klasör: ~/.rustyagent/brain (Obsidian ile de açılabilir).", &p));
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                for (tab, label) in [(BrainTab::Notes, "Notlar"), (BrainTab::History, "Sohbet geçmişinde ara")] {
                    if ui.selectable_label(self.brain_tab == tab, RichText::new(label).size(14.0)).clicked() {
                        self.brain_tab = tab;
                    }
                }
            });
            ui.add_space(8.0);
            match self.brain_tab {
                BrainTab::History => self.brain_history(ui),
                BrainTab::Notes => self.brain_notes(ui, notes),
            }
        });
    }

    fn brain_history(&mut self, ui: &mut egui::Ui) {
        let p = self.pal;
        ui.horizontal(|ui| {
            let r = text_input(ui, &mut self.history_query, "Ne konuşmuştuk? (ör. kulaklık fiyatları)", 420.0);
            let go = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if theme::icon_text_button(ui, Icon::Search, "Ara", &p).clicked() || go {
                self.history_results = brain::search_history(&self.history_query, 30);
                if self.history_results.is_empty() {
                    self.toast("Eşleşme yok", false);
                }
            }
        });
        ui.add_space(10.0);
        let results = self.history_results.clone();
        egui::ScrollArea::vertical().id_salt("hist").show(ui, |ui| {
            for (id, title, date, snip) in results {
                egui::Frame::new().fill(p.surface).stroke(Stroke::new(1.0, p.border)).corner_radius(10).inner_margin(12).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(&title).font(theme::bold(14.0)).color(p.text));
                        ui.label(muted(date.get(..10).unwrap_or(&date), &p));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if theme::ghost_button(ui, "Sohbeti aç", &p).clicked() {
                                self.open_session(&id);
                            }
                        });
                    });
                    ui.label(RichText::new(snip).size(13.0).color(p.muted));
                });
                ui.add_space(6.0);
            }
            // Çöp kutusu: silinen sohbetler 30 gün saklanır
            let trashed = sessions::trashed();
            if !trashed.is_empty() {
                section(ui, "Silinen sohbetler (30 gün saklanır)", &p);
                for (id, title) in trashed {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(&title).size(13.5).color(p.text));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if theme::icon_text_button(ui, Icon::Undo, "Geri getir", &p).clicked() {
                                match sessions::restore(&id) {
                                    Ok(_) => {
                                        self.refresh_recents();
                                        self.toast("Sohbet geri getirildi", false);
                                    }
                                    Err(e) => self.toast(format!("{e:#}"), true),
                                }
                            }
                        });
                    });
                }
            }
        });
    }

    fn brain_notes(&mut self, ui: &mut egui::Ui, notes: Vec<brain::Note>) {
        let p = self.pal;
        egui::Panel::left("note_list").exact_size(300.0).frame(egui::Frame::new().fill(p.bg)).show(ui, |ui| {
            ui.horizontal(|ui| {
                text_input(ui, &mut self.brain_query, "Notlarda ara", 190.0);
                if theme::icon_text_button(ui, Icon::Plus, "Yeni", &p).clicked() {
                    self.note_edit = Some(brain::Note { title: "Yeni not".into(), kind: "note".into(), ..Default::default() });
                    self.note_tags.clear();
                    self.note_sel = None;
                }
            });
            ui.horizontal(|ui| {
                for (k, l) in [("", "Tümü"), ("preference", "Tercih"), ("fact", "Bilgi"), ("project", "Proje"), ("summary", "Özet"), ("note", "Not")] {
                    if ui.selectable_label(self.brain_kind == k, RichText::new(l).size(12.0)).clicked() {
                        self.brain_kind = k.into();
                    }
                }
            });
            ui.add_space(6.0);
            let shown: Vec<brain::Note> = if self.brain_query.trim().is_empty() {
                notes.clone()
            } else {
                brain::search(&self.brain_query, 50).into_iter().map(|(_, n)| n).collect()
            };
            egui::ScrollArea::vertical().id_salt("notes").show(ui, |ui| {
                for n in shown.iter().filter(|n| self.brain_kind.is_empty() || n.kind == self.brain_kind) {
                    let sel = self.note_sel.as_deref() == Some(n.id.as_str());
                    let (rect, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 48.0), Sense::click());
                    if sel {
                        ui.painter().rect_filled(rect, CornerRadius::same(8), p.surface2);
                    } else if resp.hovered() {
                        ui.painter().rect_filled(rect, CornerRadius::same(8), p.hover);
                    }
                    let title: String = n.title.chars().take(38).collect();
                    ui.painter().text(rect.left_top() + Vec2::new(10.0, 8.0), egui::Align2::LEFT_TOP, title, theme::bold(13.5), p.text);
                    let meta = format!("{} · {}", kind_label(&n.kind), n.updated.get(..10).unwrap_or(""));
                    ui.painter().text(rect.left_top() + Vec2::new(10.0, 28.0), egui::Align2::LEFT_TOP, meta, egui::FontId::proportional(11.5), p.faint);
                    if resp.clicked() {
                        self.note_sel = Some(n.id.clone());
                        self.note_edit = None;
                    }
                }
                if shown.is_empty() {
                    ui.label(muted("Henüz not yok. Agent'lar seninle konuştukça burayı dolduracak; istersen elle de ekleyebilirsin.", &p));
                }
            });
        });

        egui::CentralPanel::default().frame(egui::Frame::new().fill(p.bg).inner_margin(egui::Margin { left: 20, right: 0, top: 0, bottom: 0 })).show(ui, |ui| {
            egui::ScrollArea::vertical().id_salt("note_view").show(ui, |ui| {
                if let Some(mut n) = self.note_edit.take() {
                    ui.label(muted("Başlık", &p));
                    ui.add(egui::TextEdit::singleline(&mut n.title).font(theme::serif(20.0)).desired_width(f32::INFINITY));
                    ui.horizontal(|ui| {
                        ui.label(muted("Tür", &p));
                        egui::ComboBox::from_id_salt("note_kind").selected_text(kind_label(&n.kind)).show_ui(ui, |ui| {
                            for k in ["preference", "fact", "project", "note", "summary"] {
                                ui.selectable_value(&mut n.kind, k.to_string(), kind_label(k));
                            }
                        });
                        ui.label(muted("Etiketler", &p));
                        text_input(ui, &mut self.note_tags, "profil, iş, …", 260.0);
                    });
                    ui.label(muted("İçerik (Markdown; [[Başka not]] ile bağlantı verebilirsin)", &p));
                    ui.add(egui::TextEdit::multiline(&mut n.body).desired_rows(16).desired_width(f32::INFINITY).font(egui::FontId::proportional(14.5)));
                    ui.add_space(8.0);
                    let mut keep = true;
                    ui.horizontal(|ui| {
                        if theme::primary_button(ui, "  Kaydet  ", &p).clicked() {
                            n.tags = self.note_tags.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
                            match brain::save(n.clone()) {
                                Ok(id) => {
                                    self.note_sel = Some(id);
                                    keep = false;
                                    self.toast("Not kaydedildi", false);
                                }
                                Err(e) => self.toast(format!("{e:#}"), true),
                            }
                        }
                        if theme::ghost_button(ui, "Vazgeç", &p).clicked() {
                            keep = false;
                        }
                    });
                    if keep {
                        self.note_edit = Some(n);
                    }
                    return;
                }
                let Some(id) = self.note_sel.clone() else {
                    ui.add_space(40.0);
                    ui.label(muted("Soldan bir not seç.", &p));
                    return;
                };
                let Some(n) = brain::get(&id) else {
                    self.note_sel = None;
                    return;
                };
                ui.label(RichText::new(&n.title).font(theme::serif(26.0)).color(p.text));
                ui.horizontal_wrapped(|ui| {
                    ui.label(muted(format!("{} · güncellendi {}", kind_label(&n.kind), n.updated.get(..16).unwrap_or("").replace('T', " ")), &p));
                    for tag in &n.tags {
                        egui::Frame::new().fill(p.surface2).corner_radius(6).inner_margin(egui::Margin::symmetric(6, 2)).show(ui, |ui| {
                            ui.label(RichText::new(format!("#{tag}")).size(12.0).color(p.muted));
                        });
                    }
                });
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    if theme::icon_text_button(ui, Icon::Edit, "Düzenle", &p).clicked() {
                        self.note_tags = n.tags.join(", ");
                        self.note_edit = Some(n.clone());
                    }
                    if !n.source.is_empty() && sessions::load(&n.source).is_ok() && theme::icon_text_button(ui, Icon::Chat, "Kaynak sohbet", &p).clicked() {
                        let s = n.source.clone();
                        self.open_session(&s);
                    }
                    if theme::icon_text_button(ui, Icon::Trash, "Sil", &p).clicked() {
                        let _ = brain::delete(&n.id);
                        self.note_sel = None;
                        self.toast("Not silindi", false);
                    }
                });
                ui.add_space(12.0);
                markdown::render(ui, &n.body, &p, 15.0, true);
                let links = brain::links(&n.body);
                let back = brain::backlinks(&n.title);
                if !links.is_empty() || !back.is_empty() {
                    section(ui, "Bağlantılar", &p);
                    ui.horizontal_wrapped(|ui| {
                        for l in links {
                            let exists = brain::get(&l);
                            let label = if exists.is_some() { l.clone() } else { format!("{l} (yok)") };
                            if theme::ghost_button(ui, &label, &p).clicked() {
                                match exists {
                                    Some(x) => self.note_sel = Some(x.id),
                                    None => {
                                        self.note_edit = Some(brain::Note { title: l.clone(), kind: "note".into(), ..Default::default() });
                                        self.note_tags.clear();
                                    }
                                }
                            }
                        }
                    });
                    if !back.is_empty() {
                        ui.label(muted("Bu nota bağlantı verenler:", &p));
                        ui.horizontal_wrapped(|ui| {
                            for b in back {
                                if theme::ghost_button(ui, &b.title, &p).clicked() {
                                    self.note_sel = Some(b.id);
                                }
                            }
                        });
                    }
                }
            });
        });
    }
}

fn kind_label(k: &str) -> &'static str {
    match k {
        "preference" => "Tercih",
        "fact" => "Bilgi",
        "project" => "Proje",
        "summary" => "Sohbet özeti",
        _ => "Not",
    }
}

// ------------------------------------------------------------------ agent'lar

impl App {
    fn page_agents(&mut self, ui: &mut egui::Ui, t: f32) {
        let p = self.pal;
        egui::Panel::left("agent_list").exact_size(260.0).frame(egui::Frame::new().fill(p.bg).inner_margin(egui::Margin { left: 20, right: 8, top: 20, bottom: 8 })).show(ui, |ui| {
            heading(ui, "Agent'lar", &p);
            ui.add_space(6.0);
            if theme::icon_text_button(ui, Icon::Plus, "Yeni agent", &p).clicked() {
                self.edit = Some(AgentDef {
                    name: "yeni-agent".into(),
                    description: "Bu agent ne zaman kullanılmalı?".into(),
                    model: None,
                    effort: None,
                    tools: vec![],
                    skills: vec![],
                    prompt: "Sen bu alanda uzman bir agent'sın. Görevi eksiksiz yap ve sonucu kısa raporla.".into(),
                    source: String::new(),
                    character: "cat".into(),
                    color: "#10b981".into(),
                    persona: "Meraklı ve titiz.".into(),
                });
                self.edit_main = false;
            }
            ui.add_space(8.0);
            egui::ScrollArea::vertical().id_salt("agents_scroll").show(ui, |ui| {
                let ml = main_look(&self.rt.cfg);
                let main_name = format!("{} (ana agent)", self.rt.cfg.main_name);
                if self.agent_row(ui, &ml, &main_name, self.edit_main, t) {
                    self.edit_main = true;
                    self.edit = None;
                    self.settings = Config::load().unwrap_or_else(|_| self.rt.cfg.clone());
                }
                let agents = self.agents.clone();
                for (i, a) in agents.iter().enumerate() {
                    let sel = !self.edit_main && self.edit.as_ref().map(|e| e.name == a.name).unwrap_or(false);
                    if self.agent_row(ui, &a.look(), &a.name, sel, t + i as f32) {
                        self.edit = Some(a.clone());
                        self.edit_main = false;
                    }
                }
            });
        });

        egui::CentralPanel::default().frame(egui::Frame::new().fill(p.bg).inner_margin(egui::Margin::symmetric(24, 20))).show(ui, |ui| {
            egui::ScrollArea::vertical().id_salt("agent_edit").show(ui, |ui| {
                if self.edit_main {
                    self.edit_main_agent(ui, t);
                    return;
                }
                let Some(mut def) = self.edit.take() else {
                    ui.add_space(30.0);
                    ui.label(muted("Soldan bir agent seç veya yenisini oluştur.", &p));
                    return;
                };
                let mut look = def.look();
                ui.horizontal(|ui| {
                    egui::Frame::new().fill(p.surface).corner_radius(16).inner_margin(10).show(ui, |ui| {
                        character_widget(ui, &look, self.preview_mood, 150.0, t);
                    });
                    ui.add_space(12.0);
                    ui.vertical(|ui| {
                        ui.label(RichText::new(&def.name).font(theme::serif(26.0)).color(p.text));
                        ui.label(muted("Önizleme", &p));
                        ui.horizontal_wrapped(|ui| {
                            for m in [Mood::Idle, Mood::Thinking, Mood::Working, Mood::Talking, Mood::Waiting, Mood::Happy, Mood::Error, Mood::Sleeping] {
                                ui.selectable_value(&mut self.preview_mood, m, RichText::new(m.label()).size(12.5));
                            }
                        });
                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            ui.label(muted("Karakter", &p));
                            egui::ComboBox::from_id_salt("kind").selected_text(look.kind.label()).show_ui(ui, |ui| {
                                for k in Kind::ALL {
                                    if ui.selectable_value(&mut look.kind, k, k.label()).clicked() {
                                        def.character = k.id().into();
                                    }
                                }
                            });
                            ui.label(muted("Renk", &p));
                            let mut rgb = [look.color.r(), look.color.g(), look.color.b()];
                            if ui.color_edit_button_srgb(&mut rgb).changed() {
                                def.color = character::color_hex(Color32::from_rgb(rgb[0], rgb[1], rgb[2]));
                            }
                        });
                    });
                });
                section(ui, "Kimlik", &p);
                egui::Grid::new("agent_form").num_columns(2).spacing([14.0, 10.0]).show(ui, |ui| {
                    ui.label(muted("Ad", &p));
                    text_input(ui, &mut def.name, "kisa-ad", 300.0);
                    ui.end_row();
                    ui.label(muted("Ne zaman kullanılır", &p));
                    text_input(ui, &mut def.description, "", 520.0);
                    ui.end_row();
                    ui.label(muted("Kişilik", &p));
                    ui.add(egui::TextEdit::multiline(&mut def.persona).desired_rows(2).desired_width(520.0).hint_text("ör. Sakin, bilge, az ve öz konuşur."));
                    ui.end_row();
                    ui.label(muted("Model", &p));
                    let mut m = def.model.clone().unwrap_or_default();
                    if text_input(ui, &mut m, &format!("varsayılan ({})", self.model), 300.0).changed() {
                        def.model = if m.is_empty() { None } else { Some(m) };
                    }
                    ui.end_row();
                    ui.label(muted("Effort", &p));
                    let mut eff = def.effort.clone().unwrap_or_default();
                    egui::ComboBox::from_id_salt("effort").selected_text(if eff.is_empty() { "varsayılan" } else { &eff }).show_ui(ui, |ui| {
                        for e in ["", "low", "medium", "high", "xhigh", "max"] {
                            ui.selectable_value(&mut eff, e.to_string(), if e.is_empty() { "varsayılan" } else { e });
                        }
                    });
                    def.effort = if eff.is_empty() { None } else { Some(eff) };
                    ui.end_row();
                    ui.label(muted("Araçlar", &p));
                    let mut tools = def.tools.join(", ");
                    if text_input(ui, &mut tools, "boş = hepsi", 520.0).changed() {
                        def.tools = tools.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
                    }
                    ui.end_row();
                    ui.label("");
                    ui.label(RichText::new(format!("mevcut: {}", self.rt.base_tools.names().join(", "))).size(11.5).color(p.faint));
                    ui.end_row();
                    ui.label(muted("Skill'ler", &p));
                    let mut sk = def.skills.join(", ");
                    if text_input(ui, &mut sk, "ör. pdf, docx", 520.0).changed() {
                        def.skills = sk.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
                    }
                    ui.end_row();
                });
                section(ui, "Talimatlar", &p);
                ui.add(egui::TextEdit::multiline(&mut def.prompt).desired_rows(10).desired_width(f32::INFINITY).font(egui::FontId::proportional(14.0)));
                ui.add_space(10.0);
                let mut deleted = false;
                ui.horizontal(|ui| {
                    if theme::primary_button(ui, "  Kaydet  ", &p).clicked() {
                        match def.save() {
                            Ok(path) => {
                                def.source = path.display().to_string();
                                self.agents = agent::load_agents();
                                self.toast("Agent kaydedildi", false);
                            }
                            Err(e) => self.toast(format!("{e:#}"), true),
                        }
                    }
                    if !def.source.is_empty() && theme::ghost_button(ui, "Sil", &p).clicked() {
                        match std::fs::remove_file(&def.source) {
                            Ok(_) => {
                                self.agents = agent::load_agents();
                                self.toast("Agent silindi", false);
                                deleted = true;
                            }
                            Err(e) => self.toast(format!("{e}"), true),
                        }
                    }
                });
                if !deleted {
                    self.edit = Some(def);
                }
            });
        });
    }

    fn agent_row(&self, ui: &mut egui::Ui, look: &Look, name: &str, selected: bool, t: f32) -> bool {
        let p = self.pal;
        let (rect, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 46.0), Sense::click());
        if selected {
            ui.painter().rect_filled(rect, CornerRadius::same(8), p.surface2);
        } else if resp.hovered() {
            ui.painter().rect_filled(rect, CornerRadius::same(8), p.hover);
        }
        let cr = egui::Rect::from_min_size(rect.left_top() + Vec2::new(6.0, 5.0), Vec2::splat(36.0));
        character::draw(&ui.painter_at(cr.expand(4.0)), cr, look, Mood::Idle, t);
        ui.painter().text(egui::pos2(rect.left() + 50.0, rect.center().y), egui::Align2::LEFT_CENTER, name, egui::FontId::proportional(14.0), p.text);
        resp.clicked()
    }

    fn edit_main_agent(&mut self, ui: &mut egui::Ui, t: f32) {
        let p = self.pal;
        let mut look = main_look(&self.settings);
        ui.horizontal(|ui| {
            egui::Frame::new().fill(p.surface).corner_radius(16).inner_margin(10).show(ui, |ui| {
                character_widget(ui, &look, self.preview_mood, 150.0, t);
            });
            ui.add_space(12.0);
            ui.vertical(|ui| {
                ui.label(RichText::new(&self.settings.main_name).font(theme::serif(26.0)).color(p.text));
                ui.label(muted("Ana agent: seninle konuşan ve ekibi yöneten asistan.", &p));
                ui.horizontal_wrapped(|ui| {
                    for m in [Mood::Idle, Mood::Thinking, Mood::Working, Mood::Talking, Mood::Waiting, Mood::Happy] {
                        ui.selectable_value(&mut self.preview_mood, m, RichText::new(m.label()).size(12.5));
                    }
                });
                ui.horizontal(|ui| {
                    egui::ComboBox::from_id_salt("main_kind").selected_text(look.kind.label()).show_ui(ui, |ui| {
                        for k in Kind::ALL {
                            if ui.selectable_value(&mut look.kind, k, k.label()).clicked() {
                                self.settings.main_character = k.id().into();
                            }
                        }
                    });
                    let mut rgb = [look.color.r(), look.color.g(), look.color.b()];
                    if ui.color_edit_button_srgb(&mut rgb).changed() {
                        self.settings.main_color = character::color_hex(Color32::from_rgb(rgb[0], rgb[1], rgb[2]));
                    }
                });
            });
        });
        section(ui, "Kimlik", &p);
        ui.horizontal(|ui| {
            ui.label(muted("Ad", &p));
            text_input(ui, &mut self.settings.main_name, "", 240.0);
        });
        ui.label(muted("Kişilik", &p));
        ui.add(egui::TextEdit::multiline(&mut self.settings.main_persona).desired_rows(3).desired_width(f32::INFINITY));
        ui.add_space(8.0);
        if theme::primary_button(ui, "  Kaydet  ", &p).clicked() {
            match self.settings.save() {
                Ok(_) => self.toast("Kaydedildi. Yeni kişilik yeni sohbetlerde geçerli olur (tam etkisi yeniden başlatınca).", false),
                Err(e) => self.toast(format!("{e:#}"), true),
            }
        }
    }
}

// ------------------------------------------------------------------ diğer sayfalar

impl App {
    fn page_frame(&mut self, ui: &mut egui::Ui, id: &str, title: &str, sub: &str, body: impl FnOnce(&mut Self, &mut egui::Ui)) {
        let p = self.pal;
        egui::CentralPanel::default().frame(egui::Frame::new().fill(p.bg)).show(ui, |ui| {
            egui::ScrollArea::vertical().id_salt(id).auto_shrink([false; 2]).show(ui, |ui| {
                ui.add_space(24.0);
                centered(ui, 900.0, |ui| {
                    heading(ui, title, &p);
                    if !sub.is_empty() {
                        ui.label(muted(sub, &p));
                    }
                    body(self, ui);
                    ui.add_space(30.0);
                });
            });
        });
    }

    fn page_models(&mut self, ui: &mut egui::Ui) {
        self.page_frame(ui, "models", "Modeller", "Claude, GPT, Gemini, OpenRouter ve yerel açık kaynak modeller.", |s, ui| {
            let p = s.pal;
            section(ui, "Aktif model", &p);
            ui.horizontal(|ui| {
                ui.label(RichText::new(&s.model).font(theme::bold(14.5)).color(p.text));
                text_input(ui, &mut s.model_input, "sağlayıcı/model", 300.0);
                if theme::primary_button(ui, "Kullan", &p).clicked() && !s.model_input.trim().is_empty() {
                    let m = s.model_input.trim().to_string();
                    match models::set_default(&m) {
                        Ok(_) => {
                            let _ = s.job_tx.send(Job::SetModel(m.clone()));
                            s.model = m;
                            s.toast("Model değişti", false);
                        }
                        Err(e) => s.toast(format!("{e:#}"), true),
                    }
                }
            });

            section(ui, "Hesap bağla", &p);
            ui.label(muted(
                "\"Tarayıcıdan bağlan\" sağlayıcının sayfasını açar. Kendi hesabınla (Google, GitHub, e-posta) giriş yapıp anahtar oluştur \
                 ve kopyala; uygulama kopyaladığın anahtarı otomatik yakalar, doğrular, kaydeder ve panodan siler.",
                &p,
            ));
            if let Some(c) = &s.connecting {
                egui::Frame::new().fill(p.accent.gamma_multiply(0.12)).corner_radius(8).inner_margin(10).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal_top(|ui| {
                        ui.spinner();
                        ui.add(egui::Label::new(RichText::new(c).size(13.5).color(p.text)).wrap_mode(egui::TextWrapMode::Wrap));
                    });
                });
            }
            ui.add_space(6.0);
            let cfg = Config::load().unwrap_or_else(|_| s.rt.cfg.clone());
            let mut rows: Vec<(String, String, String)> = vec![(
                "openrouter".into(),
                "OpenRouter (tek girişle yüzlerce model)".into(),
                "Gerçek tarayıcı girişi; anahtar kopyalamaya gerek yok".into(),
            )];
            rows.extend(models::KEY_SITES.iter().map(|k| (k.provider.to_string(), k.label.to_string(), k.how.to_string())));
            egui::Grid::new("providers").num_columns(4).spacing([14.0, 10.0]).show(ui, |ui| {
                for (prov, label, how) in rows {
                    let ready = cfg.providers.get(&prov).and_then(|x| x.key()).is_some();
                    ui.vertical(|ui| {
                        // Sabit sütun genişliği: uzun adlar harf harf kırılmasın
                        ui.set_width(270.0);
                        ui.add(egui::Label::new(RichText::new(&label).font(theme::bold(13.5)).color(p.text)).wrap_mode(egui::TextWrapMode::Wrap));
                        ui.add(egui::Label::new(RichText::new(&how).size(11.5).color(p.faint)).wrap_mode(egui::TextWrapMode::Wrap));
                    });
                    ui.label(RichText::new(if ready { "bağlı" } else { "bağlı değil" }).size(13.0).color(if ready { p.ok } else { p.muted }));
                    let busy = s.connecting.is_some();
                    let btn = if ready { theme::ghost_button(ui, "Yeniden bağla", &p) } else { theme::primary_button(ui, "Tarayıcıdan bağlan", &p) };
                    if btn.clicked() && !busy {
                        s.connect(&prov);
                    }
                    ui.horizontal(|ui| {
                        let k = s.key_inputs.entry(prov.clone()).or_default();
                        ui.add(egui::TextEdit::singleline(k).password(true).hint_text("veya anahtarı yapıştır").desired_width(200.0));
                        let key = k.trim().to_string();
                        if theme::ghost_button(ui, "Kaydet", &p).clicked() && !key.is_empty() {
                            let (prov2, tx) = (prov.clone(), s.tx.clone());
                            s.spawn(async move {
                                models::validate_key(&prov2, &key).await.map_err(|e| anyhow::anyhow!("Anahtar çalışmadı: {e:#}"))?;
                                let mut c = Config::load()?;
                                c.providers.get_mut(&prov2).ok_or_else(|| anyhow::anyhow!("sağlayıcı yok"))?.api_key = key;
                                c.save()?;
                                let _ = tx.send(Msg::Progress(String::new()));
                                Ok(Msg::Connected(prov2.clone(), format!("{prov2} anahtarı doğrulandı ve kaydedildi")))
                            });
                        }
                    });
                    ui.end_row();
                }
            });
            ui.add_space(4.0);
            ui.label(muted(
                "Not: ChatGPT/Claude.ai/Gemini uygulamalarının kendi \"hesapla giriş\" sistemleri yalnızca o şirketlerin uygulamalarına özeldir; \
                 başka uygulamada kullanmak kullanım koşullarına aykırıdır. Buradaki yöntem aynı hesabınla resmi anahtar alır.",
                &p,
            ));
            ui.horizontal(|ui| {
                ui.label(RichText::new("Yerel:").size(13.0).color(p.muted));
                for (name, pr) in &cfg.providers {
                    if pr.base_url.contains("localhost") {
                        ui.label(RichText::new(format!("{name} ({})", pr.base_url)).size(12.5).color(p.faint));
                    }
                }
            });

            section(ui, "Model listesi", &p);
            ui.horizontal(|ui| {
                egui::ComboBox::from_id_salt("prov").selected_text(&s.list_provider).show_ui(ui, |ui| {
                    for name in cfg.providers.keys() {
                        ui.selectable_value(&mut s.list_provider, name.clone(), name);
                    }
                });
                text_input(ui, &mut s.list_filter, "filtre", 180.0);
                if theme::ghost_button(ui, "Listele", &p).clicked() {
                    let (pv, f) = (s.list_provider.clone(), s.list_filter.clone());
                    s.spawn(async move { Ok(Msg::ModelList(models::list(&pv, &f).await?)) });
                }
            });
            let list = s.model_list.clone();
            for m in list.iter().take(150) {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(m).size(13.5).color(p.text));
                    if theme::ghost_button(ui, "Seç", &p).clicked() {
                        s.model_input = m.clone();
                    }
                    if m.starts_with("ollama/") && theme::ghost_button(ui, "Sil", &p).clicked() {
                        let n = m.clone();
                        s.spawn(async move {
                            models::ollama_remove(&n).await?;
                            Ok(Msg::Info(format!("{n} silindi")))
                        });
                    }
                });
            }

            section(ui, "Hugging Face — yerelde çalışan açık kaynak modeller", &p);
            ui.horizontal(|ui| {
                text_input(ui, &mut s.hf_query, "ör. qwen, llama, mistral, gemma", 280.0);
                if theme::icon_text_button(ui, Icon::Search, "Ara", &p).clicked() {
                    let q = s.hf_query.clone();
                    s.spawn(async move { Ok(Msg::HfResults(models::hf_browse(&q).await?)) });
                }
            });
            ui.label(muted("İpucu: 8 GB RAM için 1-8B arası modeller ve Q4_K_M seç. İndirme için Ollama kurulu olmalı.", &p));
            if !s.progress.is_empty() {
                ui.label(RichText::new(&s.progress).size(13.0).color(p.accent));
            }
            let results: Vec<(String, u64, u64)> = s.hf_results.iter().map(|m| (m.id.clone(), m.downloads, m.likes)).collect();
            egui::Grid::new("hf").num_columns(4).spacing([12.0, 6.0]).show(ui, |ui| {
                for (id, dl, likes) in results {
                    ui.label(RichText::new(&id).size(13.5).color(p.text));
                    ui.label(muted(format!("{dl} indirme · {likes} beğeni"), &p));
                    match s.hf_quants.get_mut(&id) {
                        None => {
                            if theme::ghost_button(ui, "Boyutlar", &p).clicked() {
                                let r = id.clone();
                                s.spawn(async move { Ok(Msg::HfQuants(r.clone(), models::hf_quants(&r).await?)) });
                            }
                        }
                        Some((qs, sel)) => {
                            egui::ComboBox::from_id_salt(format!("q{id}")).selected_text(sel.as_str()).show_ui(ui, |ui| {
                                for q in qs.iter() {
                                    ui.selectable_value(sel, q.clone(), q);
                                }
                            });
                        }
                    }
                    if theme::icon_text_button(ui, Icon::Download, "İndir", &p).clicked() {
                        let repo = id.clone();
                        let quant = s.hf_quants.get(&id).map(|(_, q)| q.clone()).filter(|q| !q.is_empty());
                        let tx = s.tx.clone();
                        s.spawn(async move {
                            let pt = tx.clone();
                            let label = repo.clone();
                            let name = models::hf_pull(&repo, quant.as_deref(), move |m| {
                                let _ = pt.send(Msg::Progress(format!("{label}: {m}")));
                            })
                            .await?;
                            let _ = tx.send(Msg::Progress(String::new()));
                            if let Ok(l) = models::list("ollama", "").await {
                                let _ = tx.send(Msg::OllamaModels(l));
                            }
                            Ok(Msg::Info(format!("İndirildi: {name}. Mesaj kutusundaki model seçiciden seçebilirsin.")))
                        });
                    }
                    ui.end_row();
                }
            });
        });
    }

    fn page_mcp(&mut self, ui: &mut egui::Ui) {
        self.page_frame(ui, "mcp", "Bağlantılar (MCP)", "Agent'lara yeni araçlar kazandıran MCP sunucuları. Yeni eklenenler yeniden başlatınca etkin olur.", |s, ui| {
            let p = s.pal;
            section(ui, "Bağlı sunucular", &p);
            let all = mcp::load_all();
            if all.is_empty() {
                ui.label(muted("Henüz yok. Aşağıdan arayabilir veya elle ekleyebilirsin.", &p));
            }
            for (name, c) in &all {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(name).font(theme::bold(13.5)).color(p.text));
                    ui.label(RichText::new(c.describe()).monospace().size(12.0).color(p.muted));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if theme::icon_text_button(ui, Icon::Trash, "Kaldır", &p).clicked() {
                            match mcp::remove_server(name) {
                                Ok(true) => s.toast(format!("{name} kaldırıldı (yeniden başlatınca tamamen etkin)"), false),
                                Ok(false) => s.toast(format!("{name} proje veya plugin dosyasından geliyor; oradan kaldırılmalı"), true),
                                Err(e) => s.toast(format!("{e:#}"), true),
                            }
                        }
                    });
                });
            }
            ui.add_space(6.0);
            if theme::icon_text_button(ui, Icon::Plug, "Bağlantıları test et", &p).clicked() {
                s.mcp_test = "Bağlanılıyor…".into();
                let cfg = s.rt.cfg.clone();
                s.spawn(async move { Ok(Msg::McpTest(commands::run(&cfg, &["mcp".into(), "tools".into()], &|_| {}).await?)) });
            }
            if !s.mcp_test.is_empty() {
                egui::Frame::new().fill(p.code_bg).corner_radius(8).inner_margin(10).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(RichText::new(&s.mcp_test).monospace().size(12.0).color(p.text));
                });
            }

            section(ui, "Kayıt defteri", &p);
            ui.horizontal(|ui| {
                let r = text_input(ui, &mut s.mcp_query, "ara: github, filesystem, postgres, slack…", 320.0);
                if r.changed() {
                    // Yazmayı bırakınca otomatik ara
                    s.mcp_typed = Some(Instant::now());
                }
                if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    s.search_mcp();
                }
                if s.mcp_loading {
                    ui.spinner();
                    ui.label(muted("yükleniyor…", &p));
                }
            });
            if !s.mcp_loading && s.mcp_results.is_empty() {
                ui.label(muted(if s.mcp_query.trim().is_empty() { "Liste alınamadı; internet bağlantını kontrol et." } else { "Sonuç yok." }, &p));
            }
            let results = s.mcp_results.clone();
            let configured = mcp::load_all();
            for r in results.iter().take(40) {
                let short = r.name.rsplit('/').next().unwrap_or(&r.name).to_string();
                let is_installed = configured.contains_key(&short);
                egui::Frame::new().fill(p.surface).stroke(Stroke::new(1.0, p.border)).corner_radius(10).inner_margin(12).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        ui.vertical(|ui| {
                            ui.label(RichText::new(&r.name).font(theme::bold(13.5)).color(p.text));
                            ui.label(muted(&r.description, &p));
                        });
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| match &r.install {
                            _ if is_installed => {
                                if theme::icon_text_button(ui, Icon::Trash, "Kaldır", &p).clicked() {
                                    match mcp::remove_server(&short) {
                                        Ok(_) => s.toast(format!("{short} kaldırıldı (yeniden başlatınca tamamen etkin)"), false),
                                        Err(e) => s.toast(format!("{e:#}"), true),
                                    }
                                }
                                ui.label(RichText::new("kurulu").size(12.5).color(p.ok));
                            }
                            Some(cfg) => {
                                if theme::primary_button(ui, "Kur", &p).on_hover_text(cfg.describe()).clicked() {
                                    match mcp::add_server(&short, cfg.clone()) {
                                        Ok(_) => s.toast(format!("{short} eklendi"), false),
                                        Err(e) => s.toast(format!("{e:#}"), true),
                                    }
                                }
                            }
                            None => {
                                ui.label(muted("elle kurulum", &p));
                            }
                        });
                    });
                });
                ui.add_space(4.0);
            }

            section(ui, "Elle ekle", &p);
            ui.horizontal(|ui| {
                ui.label(muted("Yerel", &p));
                text_input(ui, &mut s.mcp_form.0, "ad", 120.0);
                text_input(ui, &mut s.mcp_form.1, "komut (npx)", 110.0);
                text_input(ui, &mut s.mcp_form.2, "argümanlar", 300.0);
                if theme::ghost_button(ui, "Ekle", &p).clicked() && !s.mcp_form.0.is_empty() {
                    let c = mcp::ServerConfig { command: s.mcp_form.1.clone(), args: commands::split_args(&s.mcp_form.2), ..Default::default() };
                    let _ = mcp::add_server(&s.mcp_form.0, c);
                    s.toast("Eklendi", false);
                }
            });
            ui.horizontal(|ui| {
                ui.label(muted("Uzak", &p));
                text_input(ui, &mut s.mcp_http_form.0, "ad", 120.0);
                text_input(ui, &mut s.mcp_http_form.1, "https://…/mcp", 260.0);
                text_input(ui, &mut s.mcp_http_form.2, "Authorization: Bearer … (isteğe bağlı)", 220.0);
                if theme::ghost_button(ui, "Ekle", &p).clicked() && !s.mcp_http_form.0.is_empty() {
                    let mut headers = std::collections::BTreeMap::new();
                    if let Some((k, v)) = s.mcp_http_form.2.split_once(':') {
                        headers.insert(k.trim().to_string(), v.trim().to_string());
                    }
                    let c = mcp::ServerConfig { kind: Some("http".into()), url: Some(s.mcp_http_form.1.clone()), headers, ..Default::default() };
                    let _ = mcp::add_server(&s.mcp_http_form.0, c);
                    s.toast("Eklendi", false);
                }
            });
        });
    }

    fn page_skills(&mut self, ui: &mut egui::Ui) {
        self.page_frame(ui, "skills", "Skill'ler", "Agent'ların gerektiğinde yüklediği uzmanlık talimatları.", |s, ui| {
            let p = s.pal;
            section(ui, "Kurulu", &p);
            let installed = skills::load_all();
            if installed.is_empty() {
                ui.label(muted("Henüz yok.", &p));
            }
            for sk in &installed {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(&sk.name).font(theme::bold(13.5)).color(p.text));
                    ui.label(muted(sk.description.chars().take(110).collect::<String>(), &p));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if theme::icon_text_button(ui, Icon::Trash, "Kaldır", &p).clicked() {
                            match skills::remove(&sk.name) {
                                Ok(_) => s.toast(format!("{} kaldırıldı", sk.name), false),
                                Err(e) => s.toast(format!("{e:#}"), true),
                            }
                        }
                    });
                });
            }
            section(ui, "Kütüphane", &p);
            ui.horizontal(|ui| {
                text_input(ui, &mut s.skill_query, "süz: pdf, docx, design…", 280.0);
                if theme::icon_text_button(ui, Icon::Retry, "Yenile", &p).clicked() {
                    s.load_skills();
                }
                if s.skill_loading {
                    ui.spinner();
                    ui.label(muted("yükleniyor…", &p));
                }
            });
            let f = brain::fold(&s.skill_query);
            let results: Vec<skills::RemoteSkill> = s.skill_results.iter().filter(|r| f.is_empty() || brain::fold(&r.name).contains(&f)).cloned().collect();
            if !s.skill_loading && results.is_empty() {
                ui.label(muted(if s.skill_results.is_empty() { "Liste alınamadı; \"Yenile\" ile tekrar dene." } else { "Eşleşen skill yok." }, &p));
            }
            egui::Grid::new("skill_res").num_columns(3).spacing([16.0, 6.0]).show(ui, |ui| {
                for r in results {
                    ui.label(RichText::new(&r.name).size(13.5).color(p.text));
                    if installed.iter().any(|x| x.name == r.name) {
                        ui.label(RichText::new("kurulu").size(12.5).color(p.ok));
                        if theme::icon_text_button(ui, Icon::Trash, "Kaldır", &p).clicked() {
                            match skills::remove(&r.name) {
                                Ok(_) => s.toast(format!("{} kaldırıldı", r.name), false),
                                Err(e) => s.toast(format!("{e:#}"), true),
                            }
                        }
                    } else {
                        ui.label("");
                        if theme::icon_text_button(ui, Icon::Download, "Kur", &p).clicked() {
                            let src = r.source.clone();
                            s.spawn(async move { Ok(Msg::Info(format!("Skill kuruldu: {}", skills::install(&src).await?.display()))) });
                        }
                    }
                    ui.end_row();
                }
            });
        });
    }

    fn page_plugins(&mut self, ui: &mut egui::Ui) {
        self.page_frame(ui, "plugins", "Plugin'ler", "Agent, skill ve MCP paketleri. Kurulanlar yeniden başlatınca etkin olur.", |s, ui| {
            let p = s.pal;
            section(ui, "Kurulu", &p);
            let installed: Vec<String> = plugins::installed_dirs().iter().map(|x| x.file_name().unwrap().to_string_lossy().to_string()).collect();
            if installed.is_empty() {
                ui.label(muted("Henüz yok.", &p));
            }
            for n in &installed {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(n).font(theme::bold(13.5)).color(p.text));
                    if theme::icon_text_button(ui, Icon::Trash, "Kaldır", &p).clicked() {
                        match plugins::remove(n) {
                            Ok(_) => s.toast(format!("{n} kaldırıldı (yeniden başlatınca tamamen etkin)"), false),
                            Err(e) => s.toast(format!("{e:#}"), true),
                        }
                    }
                });
            }
            section(ui, "Pazar", &p);
            ui.horizontal(|ui| {
                text_input(ui, &mut s.plugin_query, "süz: github, slack, test…", 280.0);
                if theme::icon_text_button(ui, Icon::Retry, "Yenile", &p).clicked() {
                    s.load_plugins();
                }
                if s.plugin_loading {
                    ui.spinner();
                    ui.label(muted("yükleniyor…", &p));
                }
            });
            let f = brain::fold(&s.plugin_query);
            let results: Vec<plugins::MarketPlugin> = s
                .plugin_results
                .iter()
                .filter(|r| f.is_empty() || brain::fold(&r.name).contains(&f) || brain::fold(&r.description).contains(&f))
                .cloned()
                .collect();
            if !s.plugin_loading && results.is_empty() {
                ui.label(muted(if s.plugin_results.is_empty() { "Liste alınamadı; \"Yenile\" ile tekrar dene." } else { "Eşleşen plugin yok." }, &p));
            }
            for r in results {
                egui::Frame::new().fill(p.surface).stroke(Stroke::new(1.0, p.border)).corner_radius(10).inner_margin(12).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        ui.vertical(|ui| {
                            ui.label(RichText::new(&r.name).font(theme::bold(13.5)).color(p.text));
                            ui.label(muted(&r.description, &p));
                        });
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if installed.contains(&r.name) {
                                if theme::icon_text_button(ui, Icon::Trash, "Kaldır", &p).clicked() {
                                    match plugins::remove(&r.name) {
                                        Ok(_) => s.toast(format!("{} kaldırıldı (yeniden başlatınca tamamen etkin)", r.name), false),
                                        Err(e) => s.toast(format!("{e:#}"), true),
                                    }
                                }
                                ui.label(RichText::new("kurulu").size(12.5).color(p.ok));
                            } else if theme::primary_button(ui, "Kur", &p).clicked() {
                                let (m, n) = (s.rt.cfg.plugin_marketplaces.clone(), r.name.clone());
                                s.spawn(async move {
                                    let path = plugins::install(&m, &n).await?;
                                    Ok(Msg::Info(format!("Plugin kuruldu: {}", path.display())))
                                });
                            }
                        });
                    });
                });
                ui.add_space(4.0);
            }
        });
    }

    fn page_automations(&mut self, ui: &mut egui::Ui) {
        self.page_frame(ui, "autos", "Otomasyonlar", "Zamanlanmış görevler arka plan servisi (rusty-agent serve) açıkken çalışır; sonuçlar Telegram'a gönderilir.", |s, ui| {
            let p = s.pal;
            section(ui, "Görevler", &p);
            let list = automation::load();
            if list.is_empty() {
                ui.label(muted("Henüz yok.", &p));
            }
            for a in list {
                ui.horizontal(|ui| {
                    let mut en = a.enabled;
                    if ui.checkbox(&mut en, "").changed() {
                        let mut b = a.clone();
                        b.enabled = en;
                        let _ = automation::add(b);
                    }
                    ui.label(RichText::new(&a.name).font(theme::bold(13.5)).color(p.text));
                    ui.label(RichText::new(&a.schedule).monospace().size(12.0).color(p.muted));
                    ui.label(muted(format!("{}: {}", a.agent, a.prompt.chars().take(70).collect::<String>()), &p));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if theme::ghost_button(ui, "Sil", &p).clicked() {
                            let _ = automation::remove(&a.name);
                        }
                    });
                });
            }
            section(ui, "Yeni görev", &p);
            let agents = s.agents.clone();
            let f = &mut s.auto_form;
            egui::Grid::new("auto_form").num_columns(2).spacing([14.0, 8.0]).show(ui, |ui| {
                ui.label(muted("Ad", &p));
                text_input(ui, &mut f.name, "", 260.0);
                ui.end_row();
                ui.label(muted("Zaman (cron)", &p));
                ui.horizontal(|ui| {
                    text_input(ui, &mut f.schedule, "sn dk saat gün ay haftanın-günü", 200.0);
                    for (lbl, c) in [("Her sabah 9", "0 0 9 * * *"), ("Her saat", "0 0 * * * *"), ("Hafta içi 18:00", "0 0 18 * * Mon-Fri")] {
                        if theme::ghost_button(ui, lbl, &p).clicked() {
                            f.schedule = c.into();
                        }
                    }
                });
                ui.end_row();
                ui.label(muted("Agent", &p));
                egui::ComboBox::from_id_salt("auto_agent").selected_text(&f.agent).show_ui(ui, |ui| {
                    ui.selectable_value(&mut f.agent, MAIN_AGENT.to_string(), "main (ana agent)");
                    for a in &agents {
                        ui.selectable_value(&mut f.agent, a.name.clone(), &a.name);
                    }
                });
                ui.end_row();
                ui.label(muted("Görev", &p));
                ui.add(egui::TextEdit::multiline(&mut f.prompt).desired_rows(3).desired_width(520.0));
                ui.end_row();
                ui.label(muted("Bildirim", &p));
                ui.checkbox(&mut f.notify, "Sonucu Telegram'a gönder");
                ui.end_row();
            });
            ui.add_space(8.0);
            if theme::primary_button(ui, "  Ekle  ", &p).clicked() {
                match automation::add(s.auto_form.clone()) {
                    Ok(_) => s.toast("Otomasyon eklendi", false),
                    Err(e) => s.toast(format!("{e:#}"), true),
                }
            }
        });
    }

    fn page_settings(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let mut theme_changed = None;
        self.page_frame(ui, "settings", "Ayarlar", "", |s, ui| {
            let p = s.pal;
            section(ui, "Görünüm", &p);
            ui.horizontal(|ui| {
                for (k, l) in [("dark", "Koyu"), ("light", "Açık")] {
                    if ui.radio_value(&mut s.settings.theme, k.to_string(), l).clicked() {
                        theme_changed = Some(k.to_string());
                    }
                }
            });

            section(ui, "İzinler", &p);
            ui.horizontal(|ui| {
                for (m, l, h) in [
                    (PermissionMode::Ask, "Sor", "Değişiklik yapan ve tehlikeli işlemlerde onay ister (önerilen)"),
                    (PermissionMode::Auto, "Otomatik", "Yalnızca tehlikeli işlemlerde sorar"),
                    (PermissionMode::Yolo, "Hiç sorma", "Her şeye izin verir"),
                    (PermissionMode::Readonly, "Salt okunur", "Hiçbir değişikliğe izin vermez"),
                ] {
                    ui.radio_value(&mut s.settings.permissions.mode, m, l).on_hover_text(h);
                }
            });
            ui.label(muted(format!("Kalıcı izin kuralları: {}", if s.settings.permissions.allow.is_empty() { "yok".into() } else { s.settings.permissions.allow.join(", ") }), &p));
            if !s.settings.permissions.allow.is_empty() && theme::ghost_button(ui, "Kuralları temizle", &p).clicked() {
                s.settings.permissions.allow.clear();
            }

            section(ui, "Hafıza", &p);
            ui.checkbox(&mut s.settings.brain_auto_summary, "Biten sohbetleri özetleyip hafızaya kaydet (yeni sohbete geçince, arka planda)");

            section(ui, "Yetenekler", &p);
            ui.checkbox(&mut s.settings.computer_use, "Masaüstü kontrolü (gerçek mouse ve klavye)");
            ui.checkbox(&mut s.settings.browser, "Tarayıcı aracı (her agent kendi penceresinde)");
            ui.checkbox(&mut s.settings.cursor_overlay, "Agent imleçlerini ekranda göster");

            section(ui, "Model davranışı", &p);
            ui.horizontal(|ui| {
                ui.label(muted("Effort", &p));
                egui::ComboBox::from_id_salt("eff").selected_text(&s.settings.effort).show_ui(ui, |ui| {
                    for e in ["low", "medium", "high", "xhigh", "max"] {
                        ui.selectable_value(&mut s.settings.effort, e.to_string(), e);
                    }
                });
                ui.label(muted("En fazla çıktı", &p));
                ui.add(egui::DragValue::new(&mut s.settings.max_tokens).range(1000..=128000).speed(500));
            });
            ui.horizontal(|ui| {
                ui.label(muted("Sıkıştırma eşiği: uzak", &p));
                ui.add(egui::DragValue::new(&mut s.settings.compact_at_tokens).range(10000..=1000000).speed(1000));
                ui.label(muted("yerel", &p));
                ui.add(egui::DragValue::new(&mut s.settings.compact_at_tokens_local).range(2000..=200000).speed(500));
            });

            section(ui, "Telegram", &p);
            egui::Grid::new("tg").num_columns(2).spacing([14.0, 8.0]).show(ui, |ui| {
                ui.label(muted("Bot token", &p));
                ui.add(egui::TextEdit::singleline(&mut s.settings.telegram.bot_token).password(true).desired_width(420.0));
                ui.end_row();
                ui.label(muted("İzinli chat id'ler", &p));
                text_input(ui, &mut s.chat_ids_text, "123456, 789…", 420.0);
                ui.end_row();
            });

            section(ui, "Ses ve telefon", &p);
            let v = &mut s.settings.voice;
            egui::Grid::new("voice").num_columns(2).spacing([14.0, 8.0]).show(ui, |ui| {
                for (l, f, pw) in [
                    ("Twilio Account SID", &mut v.twilio_account_sid, false),
                    ("Twilio Auth Token", &mut v.twilio_auth_token, true),
                    ("Twilio numarası", &mut v.twilio_phone_number, false),
                    ("Senin numaran", &mut v.owner_phone_number, false),
                    ("Public adres (tünel)", &mut v.public_base_url, false),
                    ("Deepgram anahtarı", &mut v.deepgram_api_key, true),
                    ("Deepgram modeli", &mut v.deepgram_model, false),
                    ("Dil", &mut v.language, false),
                    ("Cartesia anahtarı", &mut v.cartesia_api_key, true),
                    ("Cartesia ses kimliği", &mut v.cartesia_voice_id, false),
                    ("Sesli konuşma modeli", &mut v.model, false),
                ] {
                    ui.label(muted(l, &p));
                    ui.add(egui::TextEdit::singleline(f).password(pw).desired_width(420.0));
                    ui.end_row();
                }
            });

            ui.add_space(16.0);
            if theme::primary_button(ui, "  Ayarları kaydet  ", &p).clicked() {
                s.settings.telegram.allowed_chat_ids = s.chat_ids_text.split(',').filter_map(|x| x.trim().parse().ok()).collect();
                let perms = s.settings.permissions.clone();
                match s.settings.save() {
                    Ok(_) => {
                        *s.rt.perms.write().unwrap() = perms;
                        s.toast("Kaydedildi. İzinler ve tema hemen, diğerleri yeniden başlatınca etkin olur.", false);
                    }
                    Err(e) => s.toast(format!("{e:#}"), true),
                }
            }
        });
        if let Some(t) = theme_changed {
            self.pal = palette_for(&t);
            theme::apply(ctx, &self.pal);
            let mut c = Config::load().unwrap_or_else(|_| self.rt.cfg.clone());
            c.theme = t;
            let _ = c.save();
        }
    }
}

// ------------------------------------------------------------------ ana döngü

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        while let Ok(m) = self.rx.try_recv() {
            self.handle_msg(m);
        }
        let t = self.start.elapsed().as_secs_f32();
        let p = self.pal;

        // Kısayollar
        let (new_chat, stop, toggle_sb) = ctx.input(|i| {
            (
                i.modifiers.command && i.key_pressed(egui::Key::N),
                i.key_pressed(egui::Key::Escape),
                i.modifiers.command && i.key_pressed(egui::Key::B),
            )
        });
        if new_chat {
            self.new_chat();
        }
        if stop && self.busy && self.approvals.is_empty() {
            self.cancel.notify_waiters();
        }
        if toggle_sb {
            self.sidebar_open = !self.sidebar_open;
        }
        // MCP araması: yazmayı bıraktıktan 0,5 sn sonra kendiliğinden ara
        if let Some(at) = self.mcp_typed {
            if at.elapsed() > Duration::from_millis(500) && !self.mcp_loading {
                self.search_mcp();
            }
            ctx.request_repaint_after(Duration::from_millis(100));
        }

        // Sürükle-bırak ile dosya ekleme
        let dropped: Vec<std::path::PathBuf> = ctx.input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).filter(|p| !p.as_os_str().is_empty()).collect());
        for d in dropped {
            self.add_attachment_path(&d);
            self.page = Page::Chat;
        }
        let hovering_files = ctx.input(|i| !i.raw.hovered_files.is_empty());

        egui::Panel::left("sidebar")
            .exact_size(if self.sidebar_open { 262.0 } else { 56.0 })
            .frame(egui::Frame::new().fill(p.sidebar).inner_margin(egui::Margin { left: 8, right: 8, top: 0, bottom: 0 }).stroke(Stroke::new(1.0, p.border)))
            .show(ui, |ui| self.sidebar(ui, t));

        match self.page {
            Page::Chat => self.page_chat(ui, t),
            Page::Brain => self.page_brain(ui),
            Page::Agents => self.page_agents(ui, t),
            Page::Models => self.page_models(ui),
            Page::Mcp => self.page_mcp(ui),
            Page::Skills => self.page_skills(ui),
            Page::Plugins => self.page_plugins(ui),
            Page::Automations => self.page_automations(ui),
            Page::Settings => self.page_settings(ui, &ctx),
        }

        self.approval_modal(&ctx, t);

        if hovering_files {
            let screen = ctx.content_rect();
            let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::unique("drop")));
            painter.rect_filled(screen, CornerRadius::ZERO, Color32::from_black_alpha(140));
            painter.text(screen.center(), egui::Align2::CENTER_CENTER, "Dosyayı sohbete eklemek için bırak", theme::serif(26.0), Color32::WHITE);
        }

        // Bildirim: silme işlemlerinde 8 sn boyunca "Geri al" sunar
        let mut undo_clicked: Option<String> = None;
        let mut expired = false;
        if let Some(toast) = &self.toast {
            let life = if toast.undo.is_some() { 8 } else { 4 };
            if toast.at.elapsed() < Duration::from_secs(life) {
                egui::Area::new(egui::Id::unique("toast")).anchor(egui::Align2::CENTER_BOTTOM, Vec2::new(0.0, -90.0)).show(&ctx, |ui| {
                    egui::Frame::new()
                        .fill(p.surface)
                        .stroke(Stroke::new(1.0, if toast.err { p.danger } else { p.border }))
                        .corner_radius(10)
                        .inner_margin(egui::Margin::symmetric(14, 9))
                        .show(ui, |ui| {
                            ui.set_max_width(520.0);
                            ui.horizontal(|ui| {
                                ui.label(RichText::new(&toast.msg).size(13.5).color(if toast.err { p.danger } else { p.text }));
                                if let Some(id) = &toast.undo {
                                    if theme::primary_button(ui, "Geri al", &p).clicked() {
                                        undo_clicked = Some(id.clone());
                                    }
                                }
                            });
                        });
                });
            } else {
                expired = true;
            }
        }
        if expired {
            self.toast = None;
        }
        if let Some(id) = undo_clicked {
            match sessions::restore(&id) {
                Ok(_) => {
                    self.refresh_recents();
                    self.toast("Sohbet geri getirildi", false);
                }
                Err(e) => self.toast(format!("{e:#}"), true),
            }
        }

        // Karakter animasyonları için yeniden çizim (sohbette akıcı, diğer sayfalarda seyrek)
        let fast = (self.page == Page::Chat && (self.team_open || self.entries.is_empty())) || self.page == Page::Agents || self.busy || !self.approvals.is_empty();
        ctx.request_repaint_after(Duration::from_millis(if fast { 33 } else { 250 }));
    }
}

/// Masaüstü uygulamasını başlatır (ana iş parçacığında çalışmalı)
pub fn run(rt: Arc<Runtime>, handle: tokio::runtime::Handle, resume: Option<Option<String>>, start_page: Option<String>) -> anyhow::Result<()> {
    let (tx, rx) = channel::<Msg>();
    let (job_tx, mut job_rx) = tokio::sync::mpsc::unbounded_channel::<Job>();
    let cancel = Arc::new(tokio::sync::Notify::new());

    let (ev_tx, mut ev_rx) = tokio::sync::mpsc::unbounded_channel::<AgentEvent>();
    {
        let tx = tx.clone();
        handle.spawn(async move {
            while let Some(e) = ev_rx.recv().await {
                let _ = tx.send(Msg::Agent(e));
            }
        });
    }

    // Ana agent oturumu
    {
        let rt = rt.clone();
        let tx = tx.clone();
        let cancel = cancel.clone();
        handle.spawn(async move {
            let mut session = rt.main_session(Some(ev_tx));
            let meta = |s: &agent::Session| Msg::Meta { id: s.saved_id.clone().filter(|_| !s.messages.is_empty()), title: s.title() };
            while let Some(job) = job_rx.recv().await {
                match job {
                    Job::Reset => {
                        session.reset();
                        let _ = tx.send(meta(&session));
                    }
                    Job::SetModel(m) => session.model = m,
                    Job::Rename(t) => {
                        session.set_title(&t);
                        let _ = tx.send(meta(&session));
                    }
                    Job::Rewind(turn) => {
                        if let Some((text, blocks)) = session.rewind_to(turn) {
                            let _ = tx.send(Msg::Rewound { turn, text, blocks });
                            let _ = tx.send(meta(&session));
                        }
                    }
                    Job::Compact => {
                        if let Err(e) = session.compact().await {
                            let _ = tx.send(Msg::Error(format!("Sıkıştırılamadı: {e:#}")));
                        }
                        let _ = tx.send(Msg::TurnFinished);
                    }
                    Job::Resume(id) => {
                        let loaded = match &id {
                            Some(i) => sessions::load(i),
                            None => sessions::latest(),
                        };
                        match loaded {
                            Ok(saved) => {
                                for m in &saved.messages {
                                    if m["role"] == "user" {
                                        let (text, files) = match &m["content"] {
                                            Value::String(s) => (s.clone(), vec![]),
                                            Value::Array(a) => {
                                                if a.iter().any(|b| b["type"] == "tool_result") {
                                                    continue;
                                                }
                                                let text = a.first().and_then(|b| b["text"].as_str()).unwrap_or("").to_string();
                                                let files = a.iter().skip(1).map(|b| match b["type"].as_str() {
                                                    Some("image") => "görsel".to_string(),
                                                    Some("document") => b["title"].as_str().unwrap_or("PDF").to_string(),
                                                    _ => "dosya".to_string(),
                                                }).collect();
                                                (text, files)
                                            }
                                            _ => continue,
                                        };
                                        let shown = text.rsplit(brain::USER_MARK).next().unwrap_or(&text).to_string();
                                        let _ = tx.send(Msg::UserLine(shown, files));
                                    } else if let Some(b) = m["content"].as_array() {
                                        let text = crate::llm::text_of(b);
                                        if !text.is_empty() {
                                            let _ = tx.send(Msg::Agent(AgentEvent { agent: MAIN_AGENT.into(), kind: EventKind::Text(text) }));
                                        }
                                    }
                                }
                                let model = saved.model.clone();
                                session.resume(saved);
                                let _ = tx.send(Msg::Model(model));
                                let _ = tx.send(meta(&session));
                            }
                            Err(e) => {
                                let _ = tx.send(Msg::Error(format!("{e:#}")));
                            }
                        }
                    }
                    Job::Prompt(_, _) | Job::Regenerate => {
                        let (text, att) = match job {
                            Job::Prompt(t, a) => (t, a),
                            _ => match session.pop_last_turn() {
                                Some(x) => x,
                                None => {
                                    let _ = tx.send(Msg::TurnFinished);
                                    continue;
                                }
                            },
                        };
                        let before = session.messages.len();
                        tokio::select! {
                            r = session.send_with(&text, att) => {
                                if let Err(e) = r {
                                    let _ = tx.send(Msg::Error(format!("{e:#}")));
                                }
                            }
                            _ = cancel.notified() => {
                                // Durduruldu: yarım kalan turu geri al (yalnızca sondan siler)
                                session.messages.truncate(before);
                                let _ = tx.send(Msg::Info("Durduruldu.".into()));
                            }
                        }
                        let _ = tx.send(Msg::TurnFinished);
                        let _ = tx.send(meta(&session));
                    }
                }
            }
        });
    }
    if let Some(id) = resume {
        let _ = job_tx.send(Job::Resume(id));
    }
    for e in &rt.mcp_errors {
        let _ = tx.send(Msg::Error(format!("MCP başlatılamadı: {e}")));
    }
    // Yerel modelleri arka planda öğren (model seçici için)
    {
        let tx = tx.clone();
        handle.spawn(async move {
            if let Ok(l) = models::list("ollama", "").await {
                let _ = tx.send(Msg::OllamaModels(l));
            }
        });
    }

    let cfg = rt.cfg.clone();
    let pal = palette_for(&cfg.theme);
    let mut app = App {
        handle,
        tx,
        rx,
        job_tx,
        cancel,
        pal,
        page: Page::Chat,
        start: Instant::now(),
        toast: None,
        renaming: None,
        provider_models: HashMap::new(),
        sidebar_open: true,
        team_open: true,
        entries: vec![],
        input: String::new(),
        attachments: vec![],
        busy: false,
        model: cfg.model.clone(),
        title: "Yeni sohbet".into(),
        current_id: None,
        tokens: (0, 0),
        approvals: VecDeque::new(),
        actors: HashMap::new(),
        agents: agent::load_agents(),
        recents: vec![],
        recents_filter: String::new(),
        recents_at: Instant::now(),
        ollama_models: vec![],
        model_ok: models::model_ready(&cfg.model),
        connecting: None,
        brain_tab: BrainTab::Notes,
        brain_query: String::new(),
        brain_kind: String::new(),
        note_sel: None,
        note_edit: None,
        note_tags: String::new(),
        history_query: String::new(),
        history_results: vec![],
        edit: None,
        edit_main: false,
        preview_mood: Mood::Idle,
        model_input: String::new(),
        key_inputs: HashMap::new(),
        list_provider: "ollama".into(),
        list_filter: String::new(),
        model_list: vec![],
        hf_query: String::new(),
        hf_results: vec![],
        hf_quants: HashMap::new(),
        progress: String::new(),
        status: String::new(),
        mcp_query: String::new(),
        mcp_loading: false,
        mcp_typed: None,
        skill_loading: false,
        plugin_loading: false,
        mcp_results: vec![],
        mcp_test: String::new(),
        mcp_form: Default::default(),
        mcp_http_form: Default::default(),
        skill_query: String::new(),
        skill_results: vec![],
        plugin_query: String::new(),
        plugin_results: vec![],
        auto_form: automation::Automation {
            name: "gunluk-ozet".into(),
            schedule: "0 0 9 * * *".into(),
            prompt: String::new(),
            agent: MAIN_AGENT.into(),
            notify: true,
            enabled: true,
        },
        chat_ids_text: String::new(),
        settings: cfg,
        rt,
    };
    app.refresh_recents();
    app.refresh_provider_models();
    if let Some(name) = start_page {
        let page = match name.as_str() {
            "brain" => Page::Brain,
            "agents" => Page::Agents,
            "models" => Page::Models,
            "mcp" => Page::Mcp,
            "skills" => Page::Skills,
            "plugins" => Page::Plugins,
            "automations" => Page::Automations,
            "settings" => Page::Settings,
            _ => Page::Chat,
        };
        app.goto(page);
    }

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("RustyAgent")
            .with_inner_size([1360.0, 860.0])
            .with_min_inner_size([980.0, 620.0]),
        ..Default::default()
    };
    eframe::run_native(
        "RustyAgent",
        options,
        Box::new(move |cc| {
            theme::install_fonts(&cc.egui_ctx);
            theme::apply(&cc.egui_ctx, &pal);
            Ok(Box::new(app))
        }),
    )
    .map_err(|e| anyhow::anyhow!("pencere açılamadı: {e}"))
}
