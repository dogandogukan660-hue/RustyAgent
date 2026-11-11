//! Model yönetimi:
//!  - Tarayıcıyla giriş (OpenRouter OAuth PKCE): API anahtarını elle kopyalamadan
//!    GPT, Gemini, Claude, Llama, DeepSeek, Qwen... yüzlerce modele erişim.
//!  - Hugging Face'te açık kaynak model arama (GGUF) ve Ollama ile indirme.
//!  - Sağlayıcı başına model listesi.

use crate::config::Config;
use anyhow::{bail, Context, Result};
use base64::Engine;
use futures::StreamExt;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const OLLAMA: &str = "http://localhost:11434";

// ------------------------------------------------------------------ OpenRouter girişi

/// Tarayıcıyı açar, kullanıcı OpenRouter'a giriş yapıp izin verince anahtarı alır ve kaydeder.
pub async fn login_openrouter() -> Result<String> {
    let verifier = format!("{}{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple());
    let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));

    let listener = tokio::net::TcpListener::bind("127.0.0.1:3000")
        .await
        .context("3000 portu kullanımda; onu kullanan programı kapatıp tekrar dene")?;
    let callback = "http://localhost:3000/callback";
    let url = format!(
        "https://openrouter.ai/auth?callback_url={}&code_challenge={challenge}&code_challenge_method=S256",
        urlencode(callback)
    );
    if open::that(&url).is_err() {
        println!("Tarayıcı açılamadı, bu adresi aç:\n{url}");
    }

    // Tek bir geri dönüş isteğini bekle (en fazla 5 dk)
    let code = tokio::time::timeout(std::time::Duration::from_secs(300), async {
        loop {
            let (mut sock, _) = listener.accept().await?;
            let mut buf = vec![0u8; 8192];
            let n = sock.read(&mut buf).await?;
            let req = String::from_utf8_lossy(&buf[..n]).to_string();
            let path = req.split_whitespace().nth(1).unwrap_or("").to_string();
            let code = path
                .split_once('?')
                .map(|(_, q)| q)
                .unwrap_or("")
                .split('&')
                .find_map(|kv| kv.strip_prefix("code=").map(String::from));
            let body = if code.is_some() {
                "<h2>RustyAgent: giriş tamamlandı</h2><p>Bu pencereyi kapatabilirsin.</p>"
            } else {
                "<h2>Giriş başarısız</h2>"
            };
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = sock.write_all(resp.as_bytes()).await;
            if let Some(c) = code {
                return anyhow::Ok(c);
            }
        }
    })
    .await
    .context("giriş zaman aşımına uğradı")??;

    let v: Value = reqwest::Client::new()
        .post("https://openrouter.ai/api/v1/auth/keys")
        .json(&json!({"code": code, "code_verifier": verifier, "code_challenge_method": "S256"}))
        .send()
        .await?
        .error_for_status()
        .context("OpenRouter anahtar değişimi başarısız")?
        .json()
        .await?;
    let key = v["key"].as_str().context("yanıtta anahtar yok")?.to_string();

    let mut cfg = Config::load()?;
    cfg.providers.get_mut("openrouter").context("openrouter sağlayıcısı yok")?.api_key = key;
    cfg.save()?;
    Ok("OpenRouter girişi tamam. Örnek modeller: openrouter/openai/gpt-5, openrouter/google/gemini-2.5-pro, \
        openrouter/anthropic/claude-sonnet-4.5, openrouter/deepseek/deepseek-chat — /models openrouter ile tümünü gör, /model <ad> ile seç."
        .into())
}

fn urlencode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

// ------------------------------------------------------------------ tarayıcıdan bağlan (anahtar yakalama)

/// Bir sağlayıcının anahtar sayfası. Kullanıcı tarayıcıda kendi hesabıyla (Google, GitHub, e-posta…)
/// giriş yapar, anahtar oluşturup kopyalar; uygulama anahtarı panodan yakalar, doğrular ve kaydeder.
pub struct KeySite {
    pub provider: &'static str,
    pub label: &'static str,
    pub url: &'static str,
    pub how: &'static str,
}

pub const KEY_SITES: &[KeySite] = &[
    KeySite { provider: "gemini", label: "Google Gemini (ücretsiz)", url: "https://aistudio.google.com/apikey", how: "Google hesabınla giriş yap → \"Create API key\" → anahtarı kopyala" },
    KeySite { provider: "anthropic", label: "Claude (Anthropic)", url: "https://console.anthropic.com/settings/keys", how: "Giriş yap → \"Create Key\" → anahtarı kopyala" },
    KeySite { provider: "openai", label: "OpenAI (GPT)", url: "https://platform.openai.com/api-keys", how: "Giriş yap → \"Create new secret key\" → kopyala" },
    KeySite { provider: "groq", label: "Groq (ücretsiz, çok hızlı)", url: "https://console.groq.com/keys", how: "Google/GitHub ile giriş yap → \"Create API Key\" → kopyala" },
    KeySite { provider: "xai", label: "xAI (Grok)", url: "https://console.x.ai/", how: "Giriş yap → API Keys → \"Create API key\" → kopyala" },
    KeySite { provider: "deepseek", label: "DeepSeek", url: "https://platform.deepseek.com/api_keys", how: "Giriş yap → \"Create new API key\" → kopyala" },
    KeySite { provider: "mistral", label: "Mistral", url: "https://console.mistral.ai/api-keys", how: "Giriş yap → \"Create new key\" → kopyala" },
];

pub fn key_site(provider: &str) -> Option<&'static KeySite> {
    KEY_SITES.iter().find(|k| k.provider == provider)
}

/// Sağlayıcı bağlanınca önerilen varsayılan model
pub fn recommended(provider: &str) -> &'static str {
    match provider {
        "gemini" => "gemini/gemini-3.8-flash",
        "anthropic" => "anthropic/claude-opus-5-5",
        "openai" => "openai/gpt-5",
        "groq" => "groq/llama-3.3-70b-versatile",
        "xai" => "xai/grok-4",
        "deepseek" => "deepseek/deepseek-chat",
        "mistral" => "mistral/mistral-large-latest",
        _ => "openrouter/openai/gpt-5",
    }
}

/// Sohbet/agent için uygun olmayan özel amaçlı modelleri eler (ses, görsel, TTS, gömme…)
pub fn is_chat_model(id: &str) -> bool {
    let l = id.to_lowercase();
    const SKIP: &[&str] = &[
        "tts", "image", "audio", "live", "transcribe", "embed", "aqa", "computer-use", "deep-research", "antigravity",
        "imagen", "veo", "whisper", "dall-e", "moderation", "realtime", "customtools", "translate", "robotics", "guard",
        "learnlm", "gemma-3n", "search", "babbage", "davinci", "rerank", "ocr", "vision-preview",
    ];
    !SKIP.iter().any(|s| l.contains(s))
}

/// Sağlayıcının kullanılabilir sohbet modelleri ("sağlayıcı/model" biçiminde)
pub async fn chat_models(provider: &str) -> Result<Vec<String>> {
    Ok(list(provider, "").await?.into_iter().filter(|m| is_chat_model(m.split_once('/').map(|x| x.1).unwrap_or(m))).collect())
}

/// Model adındaki sürüm numarası (gemini-3.8-flash → 3.8)
fn version_of(id: &str) -> f32 {
    regex::Regex::new(r"(\d+(?:\.\d+)?)")
        .ok()
        .and_then(|re| re.captures(id).and_then(|c| c[1].parse().ok()))
        .unwrap_or(0.0)
}

/// Sağlayıcının en uygun sohbet modelini seçer (listede olmayan / kaçınılacak modeller hariç).
/// Kararlı, en yeni ve hızlı ("flash" vb.) modeller öne alınır; önizleme ve "lite" sürümler geri planda.
pub async fn best_model(provider: &str, avoid: &[String]) -> Option<String> {
    let models = chat_models(provider).await.ok()?;
    let rec = recommended(provider);
    if models.iter().any(|m| m == rec) && !avoid.iter().any(|a| a == rec) {
        return Some(rec.to_string());
    }
    let cand: Vec<&String> = models.iter().filter(|m| !avoid.contains(m)).collect();
    rank_models(provider, cand.into_iter().cloned().collect()).into_iter().next()
}

/// Modelleri tercih sırasına dizer. Önce hızlı ve ücretsiz katmanda kotası olan aile ("flash" vb.),
/// aile içinde en yeni sürüm; önizleme ve "lite" sürümler geride. Gemini'de Gemma gibi yan modeller en sonda.
pub fn rank_models(provider: &str, mut models: Vec<String>) -> Vec<String> {
    let score = |full: &String| {
        let l = full.split_once('/').map(|x| x.1).unwrap_or(full).to_lowercase();
        let mut s = version_of(&l) * 10.0;
        // Not: "gemini" kelimesi "mini" içerdiği için "-mini" olarak aranır
        if l.contains("flash") || l.contains("-mini") || l.contains("haiku") || l.contains("instant") {
            s += 1000.0;
        }
        if l.contains("preview") || l.contains("exp") {
            s -= 4.0;
        }
        if l.contains("lite") || l.contains("nano") {
            s -= 500.0;
        }
        if provider == "gemini" && !l.starts_with("gemini") {
            s -= 5000.0;
        }
        s
    };
    models.sort_by(|a, b| score(b).partial_cmp(&score(a)).unwrap_or(std::cmp::Ordering::Equal));
    models
}

/// Model kullanılabilir mi (anahtarı var ya da yerel)
pub fn model_ready(model: &str) -> bool {
    let Ok(cfg) = Config::load() else { return false };
    match cfg.resolve_model(model) {
        Ok((_, p, _)) => p.key().is_some() || p.base_url.contains("localhost"),
        Err(_) => false,
    }
}

/// Panodaki metin bu sağlayıcının anahtarına benziyor mu
pub fn key_matches(provider: &str, s: &str) -> bool {
    let s = s.trim();
    if s.len() < 20 || s.len() > 300 || s.chars().any(char::is_whitespace) {
        return false;
    }
    let generic_sk = s.starts_with("sk-") && !s.starts_with("sk-ant-") && !s.starts_with("sk-or-");
    match provider {
        "gemini" => s.starts_with("AIza") && s.len() >= 35,
        "anthropic" => s.starts_with("sk-ant-"),
        "openrouter" => s.starts_with("sk-or-"),
        "openai" | "deepseek" => generic_sk,
        "groq" => s.starts_with("gsk_"),
        "xai" => s.starts_with("xai-"),
        "mistral" => s.len() == 32 && s.chars().all(|c| c.is_ascii_alphanumeric()),
        _ => false,
    }
}

/// Anahtarı sağlayıcıya küçük bir istekle doğrular
pub async fn validate_key(provider: &str, key: &str) -> Result<()> {
    let http = reqwest::Client::builder().timeout(std::time::Duration::from_secs(20)).build()?;
    let r = if provider == "anthropic" {
        http.get("https://api.anthropic.com/v1/models")
            .header("x-api-key", key)
            .header("anthropic-version", "2026-10-21")
            .send()
            .await?
    } else {
        let cfg = Config::load()?;
        let p = cfg.providers.get(provider).with_context(|| format!("'{provider}' sağlayıcısı yok"))?;
        http.get(format!("{}/models", p.base_url.trim_end_matches('/'))).bearer_auth(key).send().await?
    };
    if !r.status().is_success() {
        let status = r.status();
        let body: String = r.text().await.unwrap_or_default().chars().take(200).collect();
        bail!("{status}: {body}");
    }
    Ok(())
}

fn clipboard_text() -> Option<String> {
    arboard::Clipboard::new().ok()?.get_text().ok()
}

fn clear_clipboard() {
    if let Ok(mut c) = arboard::Clipboard::new() {
        let _ = c.set_text(String::new());
    }
}

/// Tarayıcıda anahtar sayfasını açar ve kopyalanan anahtarı yakalayıp kaydeder (en fazla 10 dk bekler)
pub async fn connect_with_browser(provider: &str, progress: impl Fn(String)) -> Result<String> {
    let site = key_site(provider).with_context(|| format!("'{provider}' için tarayıcıdan bağlanma yok"))?;
    let mut last = tokio::task::spawn_blocking(clipboard_text).await.ok().flatten().unwrap_or_default();
    if open::that(site.url).is_err() {
        progress(format!("Tarayıcı açılamadı; şu adresi aç: {}", site.url));
    }
    progress(format!("{} açıldı. {}. Kopyaladığında otomatik yakalanacak.", site.label, site.how));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(600);
    while std::time::Instant::now() < deadline {
        tokio::time::sleep(std::time::Duration::from_millis(700)).await;
        let Some(text) = tokio::task::spawn_blocking(clipboard_text).await.ok().flatten() else { continue };
        if text == last {
            continue;
        }
        last = text.clone();
        let key = text.trim().to_string();
        if !key_matches(provider, &key) {
            continue;
        }
        progress("Anahtar yakalandı, doğrulanıyor…".into());
        match validate_key(provider, &key).await {
            Ok(()) => {
                let mut cfg = Config::load()?;
                cfg.providers.get_mut(provider).context("sağlayıcı yok")?.api_key = key;
                cfg.save()?;
                tokio::task::spawn_blocking(clear_clipboard).await.ok();
                return Ok(format!("{} bağlandı. Anahtar kaydedildi ve güvenlik için panodan silindi.", site.label));
            }
            Err(e) => progress(format!("Bu anahtar çalışmadı ({e:#}). Başka bir anahtar kopyalayabilirsin.")),
        }
    }
    bail!("Zaman aşımı: 10 dakika içinde anahtar kopyalanmadı")
}

pub fn logout(provider: &str) -> Result<()> {
    let mut cfg = Config::load()?;
    cfg.providers.get_mut(provider).context("böyle bir sağlayıcı yok")?.api_key.clear();
    cfg.save()
}

// ------------------------------------------------------------------ model listesi

pub async fn list(provider: &str, filter: &str) -> Result<Vec<String>> {
    let cfg = Config::load()?;
    let p = cfg.providers.get(provider).with_context(|| format!("'{provider}' sağlayıcısı yok"))?;
    let http = reqwest::Client::builder().timeout(std::time::Duration::from_secs(15)).build()?;
    let ids = if provider == "ollama" {
        let v: Value = http.get(format!("{OLLAMA}/api/tags")).send().await
            .context("Ollama çalışmıyor (Ollama uygulamasını başlat)")?.json().await?;
        v["models"].as_array().cloned().unwrap_or_default().iter()
            .filter_map(|m| m["name"].as_str().map(String::from)).collect()
    } else if p.kind == crate::config::ProviderKind::Anthropic {
        vec!["claude-opus-5-5".into(), "claude-sonnet-5-5".into(), "claude-haiku-5-5".into(), "claude-fable-5-2".into()]
    } else {
        crate::openai::list_models(&http, &p.base_url, p.key().as_deref()).await?
    };
    let f = filter.to_lowercase();
    Ok(ids.into_iter().filter(|i| f.is_empty() || i.to_lowercase().contains(&f)).map(|i| format!("{provider}/{i}")).collect())
}

/// Tüm sağlayıcıların durumu (anahtar var mı)
pub fn status() -> Result<String> {
    let cfg = Config::load()?;
    let mut s = format!("Aktif model: {}\n\nSağlayıcılar:\n", cfg.model);
    for (name, p) in &cfg.providers {
        let local = p.base_url.contains("localhost");
        let st = if local { "yerel (anahtar gerekmez)" } else if p.key().is_some() { "✓ hazır" } else { "✗ anahtar yok" };
        s.push_str(&format!("  {name:<11} {st}\n"));
    }
    s.push_str("\nTarayıcıyla giriş: /login openrouter   ·   Yerel modeller: /hf browse <arama>");
    Ok(s)
}

pub fn set_default(model: &str) -> Result<()> {
    let mut cfg = Config::load()?;
    cfg.resolve_model(model)?;
    cfg.model = model.to_string();
    cfg.save()
}

// ------------------------------------------------------------------ Hugging Face + Ollama

pub struct HfModel {
    pub id: String,
    pub downloads: u64,
    pub likes: u64,
}

/// Hugging Face'te GGUF (yerelde çalışabilen) modelleri arar
pub async fn hf_browse(query: &str) -> Result<Vec<HfModel>> {
    let v: Value = reqwest::Client::builder().user_agent("rusty-agent").build()?
        .get("https://huggingface.co/api/models")
        .query(&[("search", query), ("filter", "gguf"), ("sort", "downloads"), ("direction", "-1"), ("limit", "25")])
        .send().await?.error_for_status()?.json().await?;
    Ok(v.as_array().cloned().unwrap_or_default().iter().map(|m| HfModel {
        id: m["id"].as_str().unwrap_or("").into(),
        downloads: m["downloads"].as_u64().unwrap_or(0),
        likes: m["likes"].as_u64().unwrap_or(0),
    }).collect())
}

/// Bir HF deposundaki GGUF niceleme seçenekleri (Q4_K_M, Q8_0...)
pub async fn hf_quants(repo: &str) -> Result<Vec<String>> {
    let v: Value = reqwest::Client::builder().user_agent("rusty-agent").build()?
        .get(format!("https://huggingface.co/api/models/{repo}"))
        .send().await?.error_for_status()?.json().await?;
    let re = regex::Regex::new(r"(?i)[-.]((?:I?Q\d[A-Z0-9_]*)|F16|BF16|F32)\.gguf$").unwrap();
    let mut q: Vec<String> = v["siblings"].as_array().cloned().unwrap_or_default().iter()
        .filter_map(|s| s["rfilename"].as_str())
        .filter_map(|f| re.captures(f).map(|c| c[1].to_uppercase()))
        .collect();
    q.sort();
    q.dedup();
    Ok(q)
}

/// Modeli Ollama ile indirir; ilerleme `progress` ile bildirilir. Dönen değer kullanılacak model adıdır.
pub async fn hf_pull(repo: &str, quant: Option<&str>, progress: impl Fn(String)) -> Result<String> {
    let name = match quant {
        Some(q) => format!("hf.co/{repo}:{q}"),
        None => format!("hf.co/{repo}"),
    };
    let resp = reqwest::Client::new()
        .post(format!("{OLLAMA}/api/pull"))
        .json(&json!({"model": name, "stream": true}))
        .send()
        .await
        .context("Ollama çalışmıyor. Kurulum: winget install Ollama.Ollama (veya ollama.com) ve uygulamayı başlat")?;
    if !resp.status().is_success() {
        bail!("Ollama hatası: {}", resp.text().await.unwrap_or_default());
    }
    let mut s = resp.bytes_stream();
    let mut buf: Vec<u8> = Vec::new();
    let mut last_pct = u64::MAX;
    let mut last_status = String::new();
    while let Some(chunk) = s.next().await {
        buf.extend_from_slice(&chunk?);
        while let Some(pos) = buf.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = buf.drain(..=pos).collect();
            let Ok(v) = serde_json::from_slice::<Value>(&line) else { continue };
            if let Some(e) = v["error"].as_str() {
                bail!("İndirme hatası: {e}");
            }
            let status = v["status"].as_str().unwrap_or("");
            if let (Some(done), Some(total)) = (v["completed"].as_u64(), v["total"].as_u64()) {
                let pct = done * 100 / total.max(1);
                if pct / 10 != last_pct / 10 {
                    last_pct = pct;
                    progress(format!("{status}: %{pct} ({:.1} / {:.1} GB)", done as f64 / 1e9, total as f64 / 1e9));
                }
            } else if !status.is_empty() && status != last_status {
                progress(status.to_string());
            }
            last_status = status.to_string();
        }
    }
    Ok(format!("ollama/{name}"))
}

#[cfg(test)]
mod tests {
    use super::key_matches;

    #[test]
    fn key_patterns_do_not_cross_match() {
        let gemini = "AIzaSyD-abcdefghijklmnopqrstuvwxyz12345";
        let anthropic = "sk-ant-api03-abcdefghijklmnopqrstuvwxyz";
        let openrouter = "sk-or-v1-0123456789abcdef0123456789abcdef";
        let openai = "sk-proj-abcdefghijklmnopqrstuvwxyz0123";
        assert!(key_matches("gemini", gemini));
        assert!(!key_matches("openai", gemini));
        assert!(key_matches("anthropic", anthropic));
        assert!(!key_matches("openai", anthropic));
        assert!(!key_matches("openai", openrouter));
        assert!(key_matches("openai", openai));
        assert!(key_matches("groq", "gsk_abcdefghijklmnopqrstuvwxyz0123"));
        assert!(key_matches("xai", "xai-abcdefghijklmnopqrstuvwxyz0123"));
        let dummy_mistral = "0123456789".repeat(3) + "ab";
        assert!(key_matches("mistral", &dummy_mistral));
        // Sıradan kopyalanan metinler yakalanmamalı
        assert!(!key_matches("gemini", "AIza bir cümle değil"));
        assert!(!key_matches("mistral", "Merhaba dünya bu bir cümledir!!"));
        assert!(!key_matches("openai", "sk-kısa"));
    }

    #[test]
    fn ranking_prefers_newest_flash_and_filters_special_models() {
        use super::{is_chat_model, rank_models};
        let list: Vec<String> = ["gemini/gemma-4-26b-it", "gemini/gemini-4-argon", "gemini/gemini-3.6-flash", "gemini/gemini-3.8-flash",
            "gemini/gemini-3.8-flash-lite", "gemini/gemini-3.1-pro-preview", "gemini/gemini-3-flash-preview"]
            .iter().map(|s| s.to_string()).collect();
        let r = rank_models("gemini", list);
        assert_eq!(r[0], "gemini/gemini-3.8-flash");
        assert_eq!(r[1], "gemini/gemini-3.6-flash");
        assert_eq!(r.last().unwrap(), "gemini/gemma-4-26b-it");
        assert!(is_chat_model("gemini-3.8-flash"));
        for m in ["gemini-3.8-flash-tts", "gemini-3.1-flash-image", "gemini-3.5-transcribe", "gemini-3.8-live", "antigravity-preview-latest"] {
            assert!(!is_chat_model(m), "{m}");
        }
    }
}

pub async fn ollama_remove(name: &str) -> Result<()> {
    let r = reqwest::Client::new()
        .delete(format!("{OLLAMA}/api/delete"))
        .json(&json!({"model": name.trim_start_matches("ollama/")}))
        .send()
        .await?;
    if !r.status().is_success() {
        bail!("silinemedi: {}", r.text().await.unwrap_or_default());
    }
    Ok(())
}
