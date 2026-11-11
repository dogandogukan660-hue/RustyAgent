//! Global yapılandırma: ~/.rustyagent/config.toml
//!
//! Dizin yapısı (Claude Code'a benzer):
//!   ~/.rustyagent/config.toml        -> ana ayarlar
//!   ~/.rustyagent/agents/*.md        -> agent tanımları (frontmatter + system prompt)
//!   ~/.rustyagent/skills/<ad>/SKILL.md
//!   ~/.rustyagent/plugins/<ad>/      -> plugin (agents/, skills/, .mcp.json)
//!   ~/.rustyagent/mcp.json           -> MCP sunucuları
//!   ~/.rustyagent/automations.toml   -> zamanlanmış görevler
//! Proje bazlı: ./.rustyagent/ altında aynı yapı (öncelik projede).

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Varsayılan model: "<sağlayıcı>/<model>" (ör. anthropic/claude-opus-5-5,
    /// openrouter/openai/gpt-5, gemini/gemini-2.5-pro, ollama/llama3.2).
    /// Önek yoksa anthropic varsayılır.
    pub model: String,
    /// low | medium | high | xhigh | max (Claude için)
    pub effort: String,
    pub max_tokens: u32,
    pub providers: BTreeMap<String, Provider>,
    pub telegram: TelegramConfig,
    pub voice: VoiceConfig,
    /// Plugin pazarları: "github_kullanici/repo" (içinde .claude-plugin/marketplace.json)
    pub plugin_marketplaces: Vec<String>,
    /// Skill kaynakları: "github_kullanici/repo/yol"
    pub skill_sources: Vec<String>,
    /// Bilgisayar kontrolü (mouse/klavye/ekran) açık mı
    pub computer_use: bool,
    /// Tarayıcı aracı (her agent kendi Chrome/Edge penceresinde, paralel)
    pub browser: bool,
    /// Chrome/Edge yolu (boş = otomatik bul)
    pub browser_path: String,
    /// Agent'ların renkli sanal imleçlerini ekranda göster
    pub cursor_overlay: bool,
    /// Araç izinleri / onay sistemi
    pub permissions: Permissions,
    /// Konuşma bu kadar token'a ulaşınca tur arasında özetlenir (uzak modeller)
    pub compact_at_tokens: u64,
    /// Yerel modeller (Ollama/LM Studio) için sıkıştırma eşiği — bağlam pencereleri küçüktür
    pub compact_at_tokens_local: u64,
    /// Ana agent'ın adı, karakteri, rengi ve kişiliği
    pub main_name: String,
    pub main_character: String,
    pub main_color: String,
    pub main_persona: String,
    /// Biten sohbetleri otomatik özetleyip hafızaya (second brain) yaz
    pub brain_auto_summary: bool,
    /// Arayüz teması: "dark" | "light" | "system"
    pub theme: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum PermissionMode {
    /// Değişiklik yapan ve tehlikeli işlemler için onay sor (varsayılan)
    #[default]
    Ask,
    /// Değişiklikleri serbest bırak, yalnızca tehlikeli işlemler için sor
    Auto,
    /// Hiç sorma (yalnızca deny listesi geçerli) — dikkat!
    Yolo,
    /// Yalnızca okuma; değişiklik yapan her şey reddedilir
    Readonly,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Permissions {
    pub mode: PermissionMode,
    /// Her zaman izinli: "read_file", "shell(git status*)", "browser", "mcp__github__*"
    pub allow: Vec<String>,
    /// Her zaman yasak (mod ne olursa olsun): "shell(*rm -rf*)", "computer"
    pub deny: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProviderKind {
    /// Anthropic Messages API
    Anthropic,
    /// OpenAI uyumlu /chat/completions (OpenAI, Gemini, OpenRouter, Ollama, LM Studio, Groq...)
    Openai,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Provider {
    pub kind: ProviderKind,
    #[serde(default)]
    pub base_url: String,
    /// Doğrudan anahtar (ör. tarayıcı girişiyle alınan)
    #[serde(default)]
    pub api_key: String,
    /// Anahtarın okunacağı ortam değişkeni
    #[serde(default)]
    pub api_key_env: String,
}

impl Provider {
    pub fn key(&self) -> Option<String> {
        if !self.api_key.is_empty() {
            return Some(self.api_key.clone());
        }
        if !self.api_key_env.is_empty() {
            if let Ok(k) = std::env::var(&self.api_key_env) {
                if !k.is_empty() {
                    return Some(k);
                }
            }
        }
        None
    }
}

fn default_providers() -> BTreeMap<String, Provider> {
    let p = |kind, base: &str, env: &str| Provider {
        kind,
        base_url: base.into(),
        api_key: String::new(),
        api_key_env: env.into(),
    };
    use ProviderKind::*;
    BTreeMap::from([
        ("anthropic".into(), p(Anthropic, "https://api.anthropic.com", "ANTHROPIC_API_KEY")),
        ("openai".into(), p(Openai, "https://api.openai.com/v1", "OPENAI_API_KEY")),
        ("gemini".into(), p(Openai, "https://generativelanguage.googleapis.com/v1beta/openai", "GEMINI_API_KEY")),
        ("openrouter".into(), p(Openai, "https://openrouter.ai/api/v1", "OPENROUTER_API_KEY")),
        ("groq".into(), p(Openai, "https://api.groq.com/openai/v1", "GROQ_API_KEY")),
        ("xai".into(), p(Openai, "https://api.x.ai/v1", "XAI_API_KEY")),
        ("deepseek".into(), p(Openai, "https://api.deepseek.com/v1", "DEEPSEEK_API_KEY")),
        ("mistral".into(), p(Openai, "https://api.mistral.ai/v1", "MISTRAL_API_KEY")),
        ("ollama".into(), p(Openai, "http://localhost:11434/v1", "")),
        ("lmstudio".into(), p(Openai, "http://localhost:1234/v1", "")),
    ])
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct TelegramConfig {
    pub bot_token: String,
    /// Sadece bu chat id'lerden gelen mesajlar kabul edilir (güvenlik!)
    pub allowed_chat_ids: Vec<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct VoiceConfig {
    pub twilio_account_sid: String,
    pub twilio_auth_token: String,
    pub twilio_phone_number: String,
    /// Seni arayacağı / seni tanıyacağı numara (+90...)
    pub owner_phone_number: String,
    /// Twilio'nun ulaşacağı public https adresi (ör. cloudflared/ngrok tüneli)
    pub public_base_url: String,
    /// Yerel ses sunucusu portu
    pub listen_port: u16,
    /// Twilio akışını doğrulamak için gizli anahtar (otomatik üretilir)
    pub stream_secret: String,

    pub deepgram_api_key: String,
    pub deepgram_model: String,
    pub language: String,

    pub cartesia_api_key: String,
    pub cartesia_model: String,
    /// Cartesia ses kimliği (play.cartesia.ai'den Türkçe bir ses seç)
    pub cartesia_voice_id: String,

    /// Sesli konuşmada kullanılacak model (boş = ana model). Hızlı model önerilir.
    pub model: String,
    /// Sesli konuşmada effort (düşük = daha hızlı ilk yanıt)
    pub effort: String,
}

impl Default for VoiceConfig {
    fn default() -> Self {
        Self {
            twilio_account_sid: String::new(),
            twilio_auth_token: String::new(),
            twilio_phone_number: String::new(),
            owner_phone_number: String::new(),
            public_base_url: String::new(),
            listen_port: 8787,
            stream_secret: String::new(),
            deepgram_api_key: String::new(),
            deepgram_model: "nova-3".into(),
            language: "tr".into(),
            cartesia_api_key: String::new(),
            cartesia_model: "sonic-2".into(),
            cartesia_voice_id: String::new(),
            model: String::new(),
            effort: "low".into(),
        }
    }
}

impl VoiceConfig {
    /// Çift yönlü sesli konuşma için gereken her şey ayarlı mı
    pub fn realtime_ready(&self) -> bool {
        !self.public_base_url.is_empty()
            && !self.deepgram_api_key.is_empty()
            && !self.cartesia_api_key.is_empty()
            && !self.cartesia_voice_id.is_empty()
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            model: "anthropic/claude-opus-5-5".into(),
            effort: "high".into(),
            max_tokens: 64000,
            providers: default_providers(),
            telegram: Default::default(),
            voice: Default::default(),
            plugin_marketplaces: vec!["anthropics/claude-plugins-official".into()],
            skill_sources: vec!["anthropics/skills/skills".into()],
            computer_use: false,
            browser: true,
            browser_path: String::new(),
            cursor_overlay: true,
            permissions: Permissions::default(),
            compact_at_tokens: 150_000,
            compact_at_tokens_local: 12_000,
            main_name: "Rusty".into(),
            main_character: "robot".into(),
            main_color: "#f97316".into(),
            main_persona: "Neşeli, enerjik ve pratik bir robot asistansın; kısa ve samimi konuşursun, \
                ekibini (diğer agent'ları) koordine etmekten keyif alırsın."
                .into(),
            brain_auto_summary: true,
            theme: "dark".into(),
        }
    }
}

pub fn home_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".rustyagent")
}

pub fn project_dir() -> PathBuf {
    PathBuf::from(".rustyagent")
}

/// Hem proje hem global dizinler (önce proje)
pub fn search_dirs(sub: &str) -> Vec<PathBuf> {
    vec![project_dir().join(sub), home_dir().join(sub)]
}

impl Config {
    pub fn path() -> PathBuf {
        home_dir().join("config.toml")
    }

    pub fn load() -> Result<Self> {
        let path = Self::path();
        let mut cfg: Config = if path.exists() {
            let text = std::fs::read_to_string(&path)?;
            toml::from_str(&text).with_context(|| format!("{} okunamadı", path.display()))?
        } else {
            Config::default()
        };
        let mut changed = !path.exists();
        // Yeni sürümle gelen varsayılan sağlayıcıları ekle (kullanıcınınkileri ezme)
        for (k, v) in default_providers() {
            if !cfg.providers.contains_key(&k) {
                cfg.providers.insert(k, v);
                changed = true;
            }
        }
        if cfg.voice.stream_secret.is_empty() {
            cfg.voice.stream_secret = uuid::Uuid::new_v4().simple().to_string();
            changed = true;
        }
        if changed {
            cfg.save()?;
        }
        Ok(cfg)
    }

    /// Yerel (anahtarsız, localhost) bir modelse true
    pub fn is_local_model(&self, model: &str) -> bool {
        self.resolve_model(model)
            .map(|(_, p, _)| p.base_url.contains("localhost") || p.base_url.contains("127.0.0.1"))
            .unwrap_or(false)
    }

    pub fn save(&self) -> Result<()> {
        let path = Self::path();
        std::fs::create_dir_all(path.parent().unwrap())?;
        std::fs::write(&path, toml::to_string_pretty(self)?)?;
        Ok(())
    }

    /// "sağlayıcı/model" -> (sağlayıcı adı, Provider, model adı)
    pub fn resolve_model(&self, model: &str) -> Result<(String, Provider, String)> {
        if let Some((prefix, rest)) = model.split_once('/') {
            if let Some(p) = self.providers.get(prefix) {
                return Ok((prefix.to_string(), p.clone(), rest.to_string()));
            }
        }
        // Önek yoksa (eski biçim "claude-opus-5-5") Anthropic
        let p = self.providers.get("anthropic").context("anthropic sağlayıcısı tanımlı değil")?;
        Ok(("anthropic".into(), p.clone(), model.to_string()))
    }
}
