//! Agent tanımları, agent döngüsü ve ana agent (orkestratör).
//!
//! Agent tanımı = Markdown dosyası (Claude Code subagent biçimi):
//! ---
//! name: arastirmaci
//! description: Web araştırması yapar ve kaynaklı özet çıkarır
//! model: claude-opus-5-5        # opsiyonel
//! effort: medium                # opsiyonel
//! tools: [web_search, web_fetch, read_file]   # opsiyonel, boş = hepsi; "mcp__github__*" desteklenir
//! skills: [pdf]                 # opsiyonel
//! ---
//! (system prompt)

use crate::config::{self, Config};
use crate::llm::{self, Client, StreamEvent};
use crate::notify::Notifier;
use crate::skills::{self, UseSkill};
use crate::tools::{Tool, ToolCtx, ToolOutput, ToolSet};
use anyhow::Result;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::sync::mpsc::{unbounded_channel, UnboundedSender};

pub const MAIN_AGENT: &str = "main";

#[derive(Debug, Clone)]
pub struct AgentDef {
    pub name: String,
    pub description: String,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub tools: Vec<String>,
    pub skills: Vec<String>,
    pub prompt: String,
    pub source: String,
    /// Karakter türü: robot | owl | fox | cat | slime | ghost (boş = addan otomatik)
    pub character: String,
    /// "#rrggbb" (boş = addan otomatik)
    pub color: String,
    /// Kişilik / konuşma tarzı (system prompt'a eklenir)
    pub persona: String,
}

impl AgentDef {
    pub fn look(&self) -> crate::character::Look {
        let mut l = crate::character::default_look(&self.name);
        if let Some(k) = crate::character::Kind::parse(&self.character) {
            l.kind = k;
        }
        if let Some(c) = crate::character::parse_color(&self.color) {
            l.color = c;
        }
        l
    }

    /// Agent dosyasını (frontmatter + prompt) yazar
    pub fn save(&self) -> Result<std::path::PathBuf> {
        let path = if self.source.is_empty() || !std::path::Path::new(&self.source).exists() {
            let dir = config::home_dir().join("agents");
            std::fs::create_dir_all(&dir)?;
            dir.join(format!("{}.md", self.name))
        } else {
            std::path::PathBuf::from(&self.source)
        };
        let mut fm = serde_yaml::Mapping::new();
        let mut put = |k: &str, v: serde_yaml::Value| {
            fm.insert(serde_yaml::Value::String(k.into()), v);
        };
        put("name", self.name.clone().into());
        put("description", self.description.clone().into());
        let opt = |s: &Option<String>| s.clone().filter(|x| !x.is_empty());
        if let Some(m) = opt(&self.model) { put("model", m.into()); }
        if let Some(e) = opt(&self.effort) { put("effort", e.into()); }
        if !self.tools.is_empty() { put("tools", serde_yaml::to_value(&self.tools)?); }
        if !self.skills.is_empty() { put("skills", serde_yaml::to_value(&self.skills)?); }
        if !self.character.is_empty() { put("character", self.character.clone().into()); }
        if !self.color.is_empty() { put("color", self.color.clone().into()); }
        if !self.persona.is_empty() { put("persona", self.persona.clone().into()); }
        let yaml = serde_yaml::to_string(&fm)?;
        std::fs::write(&path, format!("---\n{yaml}---\n\n{}\n", self.prompt.trim()))?;
        Ok(path)
    }
}

fn str_list(v: &serde_yaml::Value) -> Vec<String> {
    match v {
        serde_yaml::Value::Sequence(s) => s.iter().filter_map(|x| x.as_str().map(String::from)).collect(),
        serde_yaml::Value::String(s) => s.split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect(),
        _ => vec![],
    }
}

pub fn load_agents() -> Vec<AgentDef> {
    let mut dirs = config::search_dirs("agents");
    dirs.extend(crate::plugins::installed_dirs().into_iter().map(|d| d.join("agents")));
    let mut out: Vec<AgentDef> = Vec::new();
    for dir in dirs {
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().and_then(|x| x.to_str()) != Some("md") {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&p) else { continue };
            let (m, body) = skills::split_frontmatter(&text);
            let name = m["name"].as_str().map(String::from)
                .unwrap_or_else(|| p.file_stem().unwrap().to_string_lossy().into());
            if name == MAIN_AGENT || out.iter().any(|a| a.name == name) {
                continue;
            }
            out.push(AgentDef {
                name,
                description: m["description"].as_str().unwrap_or("").into(),
                model: m["model"].as_str().map(String::from),
                effort: m["effort"].as_str().map(String::from),
                tools: str_list(&m["tools"]),
                skills: str_list(&m["skills"]),
                prompt: body,
                source: p.display().to_string(),
                character: m["character"].as_str().unwrap_or("").into(),
                color: m["color"].as_str().unwrap_or("").into(),
                persona: m["persona"].as_str().unwrap_or("").into(),
            });
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// Yeni agent dosyası oluşturur (~/.rustyagent/agents/<ad>.md)
pub fn create_agent_file(name: &str, description: &str, tools: &[String], prompt: &str) -> Result<std::path::PathBuf> {
    let dir = config::home_dir().join("agents");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{name}.md"));
    let tools_line = if tools.is_empty() { String::new() } else { format!("tools: [{}]\n", tools.join(", ")) };
    std::fs::write(&path, format!("---\nname: {name}\ndescription: {description}\n{tools_line}---\n\n{prompt}\n"))?;
    Ok(path)
}

// ------------------------------------------------------------------ events

#[derive(Debug, Clone)]
pub enum EventKind {
    Text(String),
    Thinking(String),
    ToolCall { name: String, input: String },
    ToolResult { name: String, summary: String, is_error: bool },
    TurnDone { input_tokens: u64, output_tokens: u64 },
    Error(String),
    /// Bilgi notu (ör. bağlam sıkıştırıldı)
    Info(String),
    /// Kullanıcı onayı gerekiyor; arayüz `respond` ile yanıtlamalı
    Approval(crate::permissions::ApprovalHandle),
    /// Model değişti (ör. eski model kapatıldığı için otomatik geçiş)
    ModelSwitched(String),
    /// Geçici durum bilgisi (ör. "model yoğun, tekrar deneniyor")
    Status(String),
}

#[derive(Debug, Clone)]
pub struct AgentEvent {
    pub agent: String,
    pub kind: EventKind,
}

pub type EventTx = UnboundedSender<AgentEvent>;

// ------------------------------------------------------------------ runtime

/// Paylaşılan çalışma zamanı: LLM istemcisi, ayarlar, temel araçlar
pub struct Runtime {
    pub llm: Client,
    pub cfg: Config,
    pub base_tools: ToolSet,
    pub notifier: Notifier,
    pub mcp_errors: Vec<String>,
    /// İzin ayarları (çalışırken /mode ile değişebilir)
    pub perms: std::sync::RwLock<crate::config::Permissions>,
    /// "Bu oturum boyunca izin ver" kararları
    pub grants: std::sync::Mutex<std::collections::HashSet<String>>,
    /// Yakın zamanda yanıt vermeyen modeller (10 dk boyunca tekrar denenmez)
    cooldown: std::sync::Mutex<std::collections::HashMap<String, std::time::Instant>>,
}

const COOLDOWN: std::time::Duration = std::time::Duration::from_secs(600);

impl Runtime {
    fn cool(&self, model: &str) {
        self.cooldown.lock().unwrap().insert(model.to_string(), std::time::Instant::now());
    }
    fn cooling(&self) -> Vec<String> {
        let mut c = self.cooldown.lock().unwrap();
        c.retain(|_, t| t.elapsed() < COOLDOWN);
        c.keys().cloned().collect()
    }
}

impl Runtime {
    pub async fn new(cfg: Config) -> Result<Arc<Self>> {
        let llm = Client::new();
        let mut tools = crate::tools::default_toolset(cfg.computer_use, cfg.browser);
        tools.add(Arc::new(UseSkill));
        crate::brain::register(&mut tools);
        let hub = crate::mcp::McpHub::start_all().await;
        hub.register_tools(&mut tools).await;
        let mcp_errors = hub.errors.clone();
        // Hub'ı uygulama ömrü boyunca canlı tut (alt süreçler kapanmasın)
        Box::leak(Box::new(hub));
        let notifier = Notifier::new(cfg.clone());
        let perms = std::sync::RwLock::new(cfg.permissions.clone());
        Ok(Arc::new(Self { llm, cfg, base_tools: tools, notifier, mcp_errors, perms, grants: Default::default(), cooldown: Default::default() }))
    }

    /// Ana agent oturumu (tüm agent'ları yönetir)
    pub fn main_session(self: &Arc<Self>, events: Option<EventTx>) -> Session {
        self.main_session_with("", events)
    }

    /// Ek talimatlı ana agent oturumu (ör. sesli konuşma kipi)
    pub fn main_session_with(self: &Arc<Self>, extra: &str, events: Option<EventTx>) -> Session {
        let agents = load_agents();
        let mut tools = self.base_tools.clone();
        tools.add(Arc::new(Delegate { rt: self.clone(), events: events.clone() }));
        tools.add(Arc::new(NotifyUser { rt: self.clone() }));
        tools.add(Arc::new(CreateAgent));
        let mut roster = String::new();
        for a in &agents {
            roster.push_str(&format!("- {}: {}\n", a.name, a.description));
        }
        if roster.is_empty() {
            roster = "(henüz tanımlı agent yok — gerekirse create_agent ile oluştur)\n".into();
        }
        let persona = if self.cfg.main_persona.trim().is_empty() {
            String::new()
        } else {
            format!("\n\n# Kişiliğin\nAdın {}. {}", self.cfg.main_name, self.cfg.main_persona)
        };
        let system = format!(
            "{MAIN_PROMPT}{persona}\n\n# Ekibin (alt agent'lar)\n{roster}\n# Ortam\nİşletim sistemi: {os}\nÇalışma klasörü: {cwd}\nTarih: {date}{skills}",
            os = std::env::consts::OS,
            cwd = std::env::current_dir().map(|p| p.display().to_string()).unwrap_or_default(),
            date = chrono::Local::now().format("%Y-%m-%d"),
            skills = skills::catalog(&[]),
        );
        let system = if extra.is_empty() { system } else { format!("{system}\n\n{extra}") };
        let mut s = Session::new(self.clone(), MAIN_AGENT.into(), None, None, system, tools, events);
        s.enable_persistence();
        s
    }

    pub fn agent_session(self: &Arc<Self>, def: &AgentDef, events: Option<EventTx>) -> Session {
        let mut tools = self.base_tools.filtered(&def.tools);
        if !def.tools.is_empty() && !def.skills.is_empty() {
            tools.add(Arc::new(UseSkill));
        }
        let persona = if def.persona.trim().is_empty() {
            String::new()
        } else {
            format!("\n\n# Kişiliğin\nAdın {}. {} (Kişiliğin konuşma tarzını belirler; işin doğruluğundan ödün verme.)", def.name, def.persona)
        };
        let system = format!(
            "{}{persona}\n\n# Ortam\nİşletim sistemi: {}\nÇalışma klasörü: {}\nBir üst agent (yönetici) sana görev veriyor. İşi bitirince yaptığını ve sonucu kısa ve net raporla. Emoji kullanma.{}",
            def.prompt,
            std::env::consts::OS,
            std::env::current_dir().map(|p| p.display().to_string()).unwrap_or_default(),
            skills::catalog(&def.skills),
        );
        Session::new(self.clone(), def.name.clone(), def.model.clone(), def.effort.clone(), system, tools, events)
    }
}

const MAIN_PROMPT: &str = "Sen RustyAgent'ın ana agent'ısın: kullanıcının kişisel asistanı ve bir agent ekibinin yöneticisisin. \
Kullanıcı seninle terminalden, Telegram'dan veya telefonla konuşabilir.\n\
- Basit işleri kendin araçlarınla yap.\n\
- Uzmanlık gerektiren veya paralel yürütülebilecek işleri `delegate` ile ekibindeki agent'lara ver; bağımsız işler için aynı yanıtta birden çok delegate çağrısı yaparak paralel çalıştır.\n\
- Uzun süren bir iş bittiğinde, bir hata/karar noktası olduğunda veya kullanıcının bilmesi gereken önemli bir şey olduğunda `notify_user` ile haber ver (acilse call=true ile telefonla ara).\n\
- Web işleri için `browser` aracını kullan. Her agent kendi tarayıcı penceresinde çalışır ve kullanıcının mouse'unu kullanmaz; \
bu yüzden birden çok web işini (ör. 3 sitede fiyat karşılaştırma) ayrı agent'lara delegate ederek gerçekten aynı anda yaptırabilirsin.\n\
- Kalıcı bir hafızan (second brain) var. Kullanıcı hakkında ileride işe yarayacak kalıcı bilgileri (tercihler, kişisel bilgiler, \
projeler, kararlar) öğrendiğinde `memory_save` ile kaydet (kullanıcıyla ilgili olanlara 'profil' etiketi ver). Geçmişe atıf yapan sorularda \
önce `memory_search`, gerekirse `history_search` kullan. Hafızadan gelen notları yalnızca konuyla ilgiliyse kullan.\n\
- Emoji kullanma. Biçimlendirme için sade Markdown kullan (başlık, liste, kalın, kod blokları).\n\
- Yanıtlarını kısa ve net tut; kullanıcı Türkçe konuşuyorsa Türkçe yanıtla.";

// ------------------------------------------------------------------ session

pub struct Session {
    rt: Arc<Runtime>,
    pub agent: String,
    pub model: String,
    pub effort: String,
    system: String,
    tools: ToolSet,
    pub messages: Vec<Value>,
    events: Option<EventTx>,
    /// Oturum kimliği: tarayıcı penceresi ve araç bağlamı bu kimliğe bağlıdır
    id: String,
    /// Diske kaydedilen oturumun kimliği (yalnızca ana agent oturumları)
    pub saved_id: Option<String>,
    created: String,
    /// Son isteğin girdi token sayısı (sıkıştırma kararı için)
    pub last_input_tokens: u64,
    /// Sıkıştırmadan kalan özet: bir sonraki kullanıcı mesajının başına eklenir
    pending_summary: Option<String>,
    /// Sohbet başlığı (ilk kullanıcı mesajından)
    title: Option<String>,
}

const COMPACT_PROMPT: &str = "[Sistem: bağlam sıkıştırma] Bu konuşmaya daha sonra yalnızca senin yazacağın özetle devam edilecek; \
önceki mesajlar ve araç çıktıları silinecek. Devam etmek için gereken her şeyi içeren ayrıntılı bir özet yaz: \
kullanıcının amaçları, istekleri ve tercihleri; yapılan işler ve sonuçları (dosya yolları, çalıştırılan komutlar, bulunan bilgiler, URL'ler, \
önemli sayılar); alınan kararlar ve gerekçeleri; karşılaşılan hatalar; yarım kalan işler ve sıradaki adımlar. \
Araç kullanma, soru sorma; yalnızca özeti yaz.";

impl Drop for Session {
    fn drop(&mut self) {
        // Alt agent işini bitirince tarayıcı penceresini kapat, imlecini gizle
        crate::tools::browser::release(&self.id);
        crate::overlay::hide(&self.agent);
    }
}

impl Session {
    fn new(rt: Arc<Runtime>, agent: String, model: Option<String>, effort: Option<String>,
           system: String, tools: ToolSet, events: Option<EventTx>) -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let model = model.unwrap_or_else(|| rt.cfg.model.clone());
        let effort = effort.unwrap_or_else(|| rt.cfg.effort.clone());
        let id = format!("{agent}#{}", NEXT.fetch_add(1, std::sync::atomic::Ordering::SeqCst));
        Self {
            rt, agent, model, effort, system, tools, messages: vec![], events, id,
            saved_id: None,
            created: chrono::Local::now().to_rfc3339(),
            last_input_tokens: 0,
            pending_summary: None,
            title: None,
        }
    }

    fn emit(&self, kind: EventKind) {
        if let Some(tx) = &self.events {
            let _ = tx.send(AgentEvent { agent: self.agent.clone(), kind });
        }
    }

    /// Yeni konuşma başlat (eski oturum diskte kalır; ana agent'ta özeti arka planda hafızaya yazılır)
    pub fn reset(&mut self) {
        if self.agent == MAIN_AGENT && self.rt.cfg.brain_auto_summary && !self.messages.is_empty() {
            let (llm, model, system, tools, msgs) =
                (self.rt.llm.clone(), self.model.clone(), self.system.clone(), self.tools.definitions(), self.messages.clone());
            let sid = self.saved_id.clone().unwrap_or_default();
            let events = self.events.clone();
            tokio::spawn(async move {
                if let Ok(Some(id)) = crate::brain::summarize_conversation(llm, model, system, tools, msgs, sid).await {
                    if let Some(tx) = events {
                        let _ = tx.send(AgentEvent { agent: MAIN_AGENT.into(), kind: EventKind::Info(format!("Önceki sohbet hafızaya kaydedildi: {id}")) });
                    }
                }
            });
        }
        self.messages.clear();
        self.pending_summary = None;
        self.last_input_tokens = 0;
        self.title = None;
        if self.saved_id.is_some() {
            self.saved_id = Some(crate::sessions::new_id());
            self.created = chrono::Local::now().to_rfc3339();
        }
    }

    /// Son kullanıcı turunu geri alır (yeniden oluşturma için); o turun kullanıcı içeriğini döndürür.
    /// Yalnızca geçmişin sonundan siler (sondaki turu kaldırmak düşünce doğrulamasını bozmaz).
    pub fn pop_last_turn(&mut self) -> Option<(String, Vec<Value>)> {
        let idx = self.messages.iter().rposition(|m| {
            m["role"] == "user"
                && match &m["content"] {
                    Value::String(_) => true,
                    Value::Array(a) => !a.iter().any(|b| b["type"] == "tool_result"),
                    _ => false,
                }
        })?;
        let m = self.messages[idx].clone();
        self.messages.truncate(idx);
        let (text, att) = match &m["content"] {
            Value::String(s) => (s.clone(), vec![]),
            Value::Array(a) => (
                a.first().and_then(|b| b["text"].as_str()).unwrap_or("").to_string(),
                a.iter().skip(1).cloned().collect(),
            ),
            _ => (String::new(), vec![]),
        };
        // Eklenen bağlam önekini at; yeniden gönderilince tekrar eklenir
        let text = text.rsplit(crate::brain::USER_MARK).next().unwrap_or(&text).to_string();
        Some((text, att))
    }

    pub fn title(&self) -> String {
        self.title.clone().unwrap_or_else(|| "Yeni sohbet".into())
    }

    pub fn set_title(&mut self, title: &str) {
        self.title = Some(title.trim().to_string());
        self.persist();
    }

    /// Sohbeti `turn`'üncü kullanıcı mesajının hemen öncesine geri sarar (0 = ilk mesaj).
    /// O mesaj ve sonrası silinir; mesajın metni ve ekleri düzenlenip yeniden gönderilebilsin diye döner.
    /// Yalnızca geçmişin sonundan silindiği için düşünce bloğu doğrulamasını bozmaz.
    pub fn rewind_to(&mut self, turn: usize) -> Option<(String, Vec<Value>)> {
        let is_real_user = |m: &Value| {
            m["role"] == "user"
                && match &m["content"] {
                    Value::String(_) => true,
                    Value::Array(a) => !a.iter().any(|b| b["type"] == "tool_result"),
                    _ => false,
                }
        };
        let idx = self.messages.iter().enumerate().filter(|(_, m)| is_real_user(m)).nth(turn).map(|(i, _)| i)?;
        let m = self.messages[idx].clone();
        self.messages.truncate(idx);
        self.last_input_tokens = 0;
        let (text, att) = match &m["content"] {
            Value::String(s) => (s.clone(), vec![]),
            Value::Array(a) => (a.first().and_then(|b| b["text"].as_str()).unwrap_or("").to_string(), a.iter().skip(1).cloned().collect()),
            _ => (String::new(), vec![]),
        };
        let text = text.rsplit(crate::brain::USER_MARK).next().unwrap_or(&text).to_string();
        if self.messages.is_empty() {
            // Sohbet tamamen geri sarıldı: boş dosya bırakma
            if let Some(id) = &self.saved_id {
                let _ = crate::sessions::delete(id);
            }
            self.title = None;
        } else {
            self.persist();
        }
        Some((text, att))
    }

    /// Bu oturumu diske kaydetmeyi aç
    pub fn enable_persistence(&mut self) {
        if self.saved_id.is_none() {
            self.saved_id = Some(crate::sessions::new_id());
        }
    }

    fn persist(&self) {
        let Some(id) = &self.saved_id else { return };
        if self.messages.is_empty() && self.pending_summary.is_none() {
            return;
        }
        let s = crate::sessions::Saved {
            id: id.clone(),
            agent: self.agent.clone(),
            model: self.model.clone(),
            title: self.title.clone().unwrap_or_else(|| crate::sessions::title_from(&self.messages)),
            created: self.created.clone(),
            updated: chrono::Local::now().to_rfc3339(),
            system: self.system.clone(),
            messages: self.messages.clone(),
            pending_summary: self.pending_summary.clone(),
            input_tokens: self.last_input_tokens,
        };
        if let Err(e) = crate::sessions::save(&s) {
            self.emit(EventKind::Error(format!("oturum kaydedilemedi: {e:#}")));
        }
    }

    /// Kayıtlı bir oturuma devam et
    pub fn resume(&mut self, saved: crate::sessions::Saved) {
        self.title = Some(saved.title.clone());
        self.system = saved.system;
        self.messages = crate::sessions::strip_thinking(&saved.messages);
        self.model = saved.model;
        self.pending_summary = saved.pending_summary;
        self.saved_id = Some(saved.id);
        self.created = saved.created;
        self.last_input_tokens = saved.input_tokens;
    }

    fn compact_threshold(&self) -> u64 {
        if self.rt.cfg.is_local_model(&self.model) {
            self.rt.cfg.compact_at_tokens_local
        } else {
            self.rt.cfg.compact_at_tokens
        }
    }

    /// Bağlamı sıkıştır: tüm geçmişi tek bir özete çevirir ("basit sıkıştırma").
    /// Yalnızca turlar arasında çağrılır (bekleyen araç sonucu yokken). Özet bir sonraki
    /// kullanıcı mesajının başına eklenir; eski mesajlar ve düşünce blokları taşınmaz.
    pub async fn compact(&mut self) -> Result<()> {
        if self.messages.is_empty() {
            return Ok(());
        }
        if self.messages.last().map(|m| m["role"] != "assistant").unwrap_or(true) {
            anyhow::bail!("tur ortasında sıkıştırılamaz");
        }
        self.emit(EventKind::Info("Bağlam sıkıştırılıyor (konuşma özetleniyor)…".into()));
        // İstek, mevcut geçmişin sonuna eklenen tek bir kullanıcı mesajıdır (geçmiş değişmez)
        let mut msgs = self.messages.clone();
        let prev = self.pending_summary.clone().map(|s| format!("Daha önceki özet:\n{s}\n\n")).unwrap_or_default();
        msgs.push(json!({"role": "user", "content": format!("{prev}{COMPACT_PROMPT}")}));
        let defs = self.tools.definitions();
        let resp = self
            .rt
            .llm
            .stream(
                llm::Request {
                    model: &self.model,
                    effort: "low",
                    max_tokens: 16000,
                    system: &self.system,
                    tools: &defs,
                    messages: &msgs,
                    tool_choice: Some("none"),
                },
                None,
            )
            .await?;
        let summary = llm::text_of(&resp.content);
        if summary.trim().is_empty() {
            anyhow::bail!("özet boş döndü");
        }
        let before = self.last_input_tokens;
        if self.agent == MAIN_AGENT {
            let id = self.saved_id.clone().unwrap_or_default();
            crate::brain::save_compaction(&summary, &id, self.title.as_deref().unwrap_or("Sohbet"));
        }
        self.messages.clear();
        self.pending_summary = Some(summary);
        self.last_input_tokens = 0;
        self.persist();
        self.emit(EventKind::Info(format!("Bağlam sıkıştırıldı (~{before} token → özet).")));
        Ok(())
    }

    /// Kullanılamayan model yerine aynı sağlayıcıdan yedek seçer: önce sağlayıcının önerisi, sonra en uygun model
    async fn replacement_model(&self, p: &llm::ModelProblem, failed: &[String]) -> Option<String> {
        let provider = self.model.split_once('/').map(|x| x.0).unwrap_or("anthropic").to_string();
        let available = crate::models::chat_models(&provider).await.unwrap_or_default();
        // Bu turda başarısız olanlar + yakın zamanda yanıt vermeyenler denenmez
        let mut avoid: Vec<String> = failed.to_vec();
        avoid.extend(self.rt.cooling());
        if let Some(s) = &p.suggested {
            let cand = format!("{provider}/{s}");
            if !avoid.contains(&cand) && (available.is_empty() || available.contains(&cand)) {
                return Some(cand);
            }
        }
        crate::models::best_model(&provider, &avoid).await
    }

    /// İzin sistemi: None = çalıştır, Some(neden) = reddedildi
    async fn authorize(&self, name: &str, input: &Value, ctx: &ToolCtx) -> Option<String> {
        use crate::permissions::{self as perm, Decision, Verdict};
        let a = perm::assess(name, input, ctx).await;
        let verdict = {
            let perms = self.rt.perms.read().unwrap().clone();
            let grants = self.rt.grants.lock().unwrap().clone();
            perm::evaluate(&perms, &grants, name, &a)
        };
        match verdict {
            Verdict::Allow => None,
            Verdict::Deny(r) => Some(r),
            Verdict::Ask => {
                let req = perm::ApprovalRequest {
                    id: uuid::Uuid::new_v4().simple().to_string()[..10].to_string(),
                    agent: self.agent.clone(),
                    tool: name.to_string(),
                    summary: a.summary.clone(),
                    risk: a.risk,
                    grant_key: a.grant_key.clone(),
                };
                let decision = match &self.events {
                    Some(tx) => {
                        let (h, rx) = perm::ApprovalHandle::new(req.clone());
                        if tx.send(AgentEvent { agent: self.agent.clone(), kind: EventKind::Approval(h) }).is_err() {
                            Decision::Deny
                        } else {
                            match tokio::time::timeout(std::time::Duration::from_secs(30 * 60), rx).await {
                                Ok(Ok(d)) => d,
                                _ => Decision::Deny,
                            }
                        }
                    }
                    // Arayüz yok (otomasyon): Telegram'dan sor
                    None => perm::ask_telegram(&req, &[]).await,
                };
                match decision {
                    Decision::Allow => None,
                    Decision::AllowSession => {
                        self.rt.grants.lock().unwrap().insert(a.grant_key.clone());
                        None
                    }
                    Decision::AllowAlways => {
                        let _ = perm::persist_allow(&a.grant_key);
                        self.rt.perms.write().unwrap().allow.push(a.grant_key.clone());
                        None
                    }
                    Decision::Deny => Some("kullanıcı onay vermedi".into()),
                }
            }
        }
    }

    /// Kullanıcı mesajını gönderir; agent işini bitirene kadar araç döngüsünü çalıştırır.
    pub async fn send(&mut self, user_text: &str) -> Result<String> {
        self.send_with(user_text, vec![]).await
    }

    /// Ekli mesaj: `attachments` Anthropic içerik blokları (image / text) olarak mesajın sonuna eklenir.
    pub async fn send_with(&mut self, user_text: &str, attachments: Vec<Value>) -> Result<String> {
        // Bağlam büyüdüyse tur başlamadan önce sıkıştır
        if self.last_input_tokens >= self.compact_threshold() {
            if let Err(e) = self.compact().await {
                self.emit(EventKind::Error(format!("sıkıştırma başarısız: {e:#}")));
            }
        }
        if self.title.is_none() {
            let t: String = user_text.lines().next().unwrap_or("").chars().take(70).collect();
            self.title = Some(if t.trim().is_empty() { "Yeni sohbet".into() } else { t });
        }
        // Yeni konuşmanın ilk mesajına: sıkıştırma özeti + (ana agent için) hafızadan ilgili notlar
        let first = self.messages.is_empty();
        let summary_used = if first { self.pending_summary.take() } else { None };
        let mut prefix = String::new();
        if let Some(s) = &summary_used {
            prefix.push_str(&format!("[Önceki konuşmanın özeti — bağlam sıkıştırıldı]\n{s}\n\n"));
        }
        if first && self.agent == MAIN_AGENT {
            prefix.push_str(&crate::brain::context_for(user_text));
        }
        let text = if prefix.is_empty() { user_text.to_string() } else { format!("{prefix}{}{user_text}", crate::brain::USER_MARK) };
        let content = if attachments.is_empty() {
            json!(text)
        } else {
            let mut blocks = vec![json!({"type": "text", "text": text})];
            blocks.extend(attachments);
            Value::Array(blocks)
        };
        let start_len = self.messages.len();
        self.messages.push(json!({"role": "user", "content": content}));
        let defs = self.tools.definitions();
        let mut final_text = String::new();

        let mut failed_models: Vec<String> = Vec::new();
        // Model az önce yanıt vermediyse yine 30 sn beklemek yerine doğrudan yedeğe geç
        if self.rt.cooling().contains(&self.model) {
            let p = llm::ModelProblem { model: self.model.clone(), kind: "overloaded", suggested: None, detail: String::new() };
            if let Some(m) = self.replacement_model(&p, &[self.model.clone()]).await {
                self.emit(EventKind::Status(format!("{} az önce yanıt vermedi; {} kullanılıyor", self.model, m)));
                self.model = m.clone();
                self.emit(EventKind::ModelSwitched(m));
            }
        }
        for _ in 0..150 {
            // LLM stream olaylarını agent olaylarına çevir
            let (stx, mut srx) = unbounded_channel::<StreamEvent>();
            let fwd = self.events.clone();
            let agent = self.agent.clone();
            let forwarder = tokio::spawn(async move {
                while let Some(ev) = srx.recv().await {
                    if let Some(tx) = &fwd {
                        let kind = match ev {
                            StreamEvent::Text(t) => EventKind::Text(t),
                            StreamEvent::Thinking(t) => EventKind::Thinking(t),
                            StreamEvent::ToolInputStart => continue,
                            StreamEvent::Status(s) => EventKind::Status(s),
                        };
                        let _ = tx.send(AgentEvent { agent: agent.clone(), kind });
                    }
                }
            });

            let resp = self.rt.llm.stream(
                llm::Request {
                    model: &self.model,
                    effort: &self.effort,
                    max_tokens: self.rt.cfg.max_tokens,
                    system: &self.system,
                    tools: &defs,
                    messages: &self.messages,
                    tool_choice: None,
                },
                Some(&stx),
            ).await;
            drop(stx);
            let _ = forwarder.await;
            let resp = match resp {
                Ok(r) => r,
                // Model kaldırılmış / kota bitmiş: uygun yedek modele geç ve aynı isteği tekrar dene
                Err(e) if e.downcast_ref::<llm::ModelProblem>().is_some() && failed_models.len() < 6 => {
                    let p = e.downcast_ref::<llm::ModelProblem>().unwrap().clone();
                    failed_models.push(self.model.clone());
                    if p.kind != "unavailable" {
                        self.rt.cool(&self.model);
                    }
                    match self.replacement_model(&p, &failed_models).await {
                        Some(new_model) => {
                            let why = match p.kind {
                                "quota" => "kotası dolu",
                                "overloaded" => "şu an yanıt vermiyor",
                                _ => "artık kullanılamıyor",
                            };
                            self.emit(EventKind::Info(format!("{} {why}; {} modeline geçildi.", self.model, new_model)));
                            self.model = new_model.clone();
                            // Kalıcı sorunlarda (kaldırılmış model / biten kota) ana agent'ın varsayılanı da güncellensin;
                            // geçici yoğunlukta varsayılan değişmez, yalnızca bu oturum yedek modelle sürer
                            if self.agent == MAIN_AGENT && p.kind == "unavailable" {
                                let _ = crate::models::set_default(&new_model);
                            }
                            self.emit(EventKind::ModelSwitched(new_model));
                            continue;
                        }
                        None => {
                            self.messages.truncate(start_len);
                            if summary_used.is_some() {
                                self.pending_summary = summary_used.clone();
                            }
                            let msg = format!("{e:#}\nUygun yedek model bulunamadı; Modeller sayfasından başka bir model veya sağlayıcı seç.");
                            self.emit(EventKind::Error(msg.clone()));
                            return Err(anyhow::anyhow!(msg));
                        }
                    }
                }
                Err(e) => {
                    // Yarım kalan turu tamamen geri al ki geçmiş tutarlı kalsın
                    self.messages.truncate(start_len);
                    if summary_used.is_some() {
                        self.pending_summary = summary_used.clone();
                    }
                    self.emit(EventKind::Error(format!("{e:#}")));
                    return Err(e);
                }
            };
            self.last_input_tokens = resp.input_tokens;
            self.emit(EventKind::TurnDone { input_tokens: resp.input_tokens, output_tokens: resp.output_tokens });

            let text = llm::text_of(&resp.content);
            if !text.is_empty() {
                final_text = text;
            }
            self.messages.push(json!({"role": "assistant", "content": resp.content.clone()}));

            match resp.stop_reason.as_str() {
                "tool_use" => {
                    let results = self.run_tools(&resp.content).await;
                    self.messages.push(json!({"role": "user", "content": results}));
                }
                // Sunucu aracı (web_search) uzun sürdü; aynı istekle devam et
                "pause_turn" => continue,
                "max_tokens" => {
                    final_text.push_str("\n[yanıt max_tokens sınırında kesildi]");
                    break;
                }
                "refusal" => {
                    final_text = "İstek güvenlik nedeniyle reddedildi.".into();
                    break;
                }
                _ => break,
            }
        }
        self.persist();
        Ok(final_text)
    }

    /// Bir yanıttaki tüm tool_use bloklarını paralel çalıştırır;
    /// sonuçların hepsi tek bir user mesajında döner.
    async fn run_tools(&self, content: &[Value]) -> Vec<Value> {
        let calls: Vec<&Value> = content.iter().filter(|b| b["type"] == "tool_use").collect();
        let futs = calls.iter().map(|b| {
            let id = b["id"].as_str().unwrap_or("").to_string();
            let name = b["name"].as_str().unwrap_or("").to_string();
            let input = b["input"].clone();
            let tool = self.tools.get(&name);
            let ctx = ToolCtx { agent: self.agent.clone(), session: self.id.clone() };
            let mut preview = input.to_string();
            if preview.len() > 200 {
                let mut end = 200;
                while !preview.is_char_boundary(end) { end -= 1; }
                preview.truncate(end);
                preview.push('…');
            }
            self.emit(EventKind::ToolCall { name: name.clone(), input: preview });
            async move {
                let out = if input.get("__invalid_json").is_some() {
                    ToolOutput::Error("INVALID_JSON: araç girdisi geçerli JSON değildi, tekrar dene".into())
                } else if tool.is_none() {
                    ToolOutput::Error(format!("'{name}' adında araç yok"))
                } else if let Some(reason) = self.authorize(&name, &input, &ctx).await {
                    ToolOutput::Error(format!(
                        "İŞLEM REDDEDİLDİ ({reason}). Aynı işlemi tekrar deneme; gerekirse kullanıcıya nedenini açıkla veya başka bir yol öner."
                    ))
                } else {
                    tool.unwrap().call_ctx(input, &ctx).await
                };
                (id, name, out)
            }
        });
        let mut blocks = Vec::new();
        for (id, name, out) in futures::future::join_all(futs).await {
            self.emit(EventKind::ToolResult {
                name,
                summary: out.summary(),
                is_error: matches!(out, ToolOutput::Error(_)),
            });
            blocks.push(out.to_result_block(&id));
        }
        blocks
    }
}

// ------------------------------------------------------------------ orkestratör araçları

struct Delegate {
    rt: Arc<Runtime>,
    events: Option<EventTx>,
}

#[async_trait]
impl Tool for Delegate {
    fn name(&self) -> String { "delegate".into() }
    fn description(&self) -> String {
        "Bir görevi ekipteki bir agent'a verir ve sonucunu döndürür. Alt agent senin konuşmanı görmez; görevi tüm gerekli bağlamla, tek başına anlaşılır yaz. Bağımsız görevler için birden çok delegate çağrısını aynı anda yap.".into()
    }
    fn input_schema(&self) -> Value {
        json!({"type":"object","properties":{
            "agent":{"type":"string","description":"Agent adı"},
            "task":{"type":"string","description":"Eksiksiz görev tanımı"}
        },"required":["agent","task"],"additionalProperties":false})
    }
    async fn call(&self, input: Value) -> ToolOutput {
        let name = input["agent"].as_str().unwrap_or("");
        let task = input["task"].as_str().unwrap_or("");
        let Some(def) = load_agents().into_iter().find(|a| a.name == name) else {
            return ToolOutput::Error(format!("'{name}' adında agent yok"));
        };
        let mut s = self.rt.agent_session(&def, self.events.clone());
        match s.send(task).await {
            Ok(t) if t.trim().is_empty() => ToolOutput::Text("(agent metin döndürmedi)".into()),
            Ok(t) => ToolOutput::Text(t),
            Err(e) => ToolOutput::Error(format!("{name} başarısız: {e:#}")),
        }
    }
}

struct NotifyUser {
    rt: Arc<Runtime>,
}

#[async_trait]
impl Tool for NotifyUser {
    fn name(&self) -> String { "notify_user".into() }
    fn description(&self) -> String {
        "Kullanıcıya Telegram'dan mesaj gönderir. call=true ise ayrıca telefonla arayıp mesajı sesli okur (sadece önemli/acil durumlarda).".into()
    }
    fn input_schema(&self) -> Value {
        json!({"type":"object","properties":{
            "message":{"type":"string"},
            "call":{"type":"boolean"}
        },"required":["message"],"additionalProperties":false})
    }
    async fn call(&self, input: Value) -> ToolOutput {
        let msg = input["message"].as_str().unwrap_or("");
        let call = input["call"].as_bool().unwrap_or(false);
        match self.rt.notifier.notify(msg, call).await {
            Ok(r) => ToolOutput::Text(r),
            Err(e) => ToolOutput::Error(format!("{e:#}")),
        }
    }
}

struct CreateAgent;

#[async_trait]
impl Tool for CreateAgent {
    fn name(&self) -> String { "create_agent".into() }
    fn description(&self) -> String {
        "Ekibe yeni bir uzman agent ekler (kalıcı olarak kaydedilir). tools boşsa tüm araçlara erişir.".into()
    }
    fn input_schema(&self) -> Value {
        json!({"type":"object","properties":{
            "name":{"type":"string","description":"kisa-ad (küçük harf, tire)"},
            "description":{"type":"string","description":"Ne zaman kullanılacağı"},
            "tools":{"type":"array","items":{"type":"string"}},
            "system_prompt":{"type":"string"}
        },"required":["name","description","system_prompt"],"additionalProperties":false})
    }
    async fn call(&self, input: Value) -> ToolOutput {
        let tools: Vec<String> = input["tools"].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default();
        match create_agent_file(
            input["name"].as_str().unwrap_or("agent"),
            input["description"].as_str().unwrap_or(""),
            &tools,
            input["system_prompt"].as_str().unwrap_or(""),
        ) {
            Ok(p) => ToolOutput::Text(format!("Agent oluşturuldu: {}", p.display())),
            Err(e) => ToolOutput::Error(e.to_string()),
        }
    }
}
