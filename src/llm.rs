//! LLM yönlendiricisi. Model adı "<sağlayıcı>/<model>" biçimindedir:
//!   anthropic/...  -> Claude Messages API (ham HTTP + SSE; Rust için resmi SDK yok)
//!   openai/, gemini/, openrouter/, ollama/, lmstudio/ ... -> OpenAI uyumlu API (src/openai.rs)
//!
//! Konuşma geçmişi her sağlayıcı için Anthropic içerik bloğu biçiminde tutulur;
//! OpenAI uyumlu sağlayıcılar için istek anında çevrilir. Asistan mesajının
//! `content` blokları (thinking dahil) olduğu gibi geçmişe eklenir (append-only).

use crate::config::{Config, ProviderKind};
use anyhow::{bail, Context, Result};
use futures::StreamExt;
use serde_json::{json, Value};
use tokio::sync::mpsc::UnboundedSender;

const API_VERSION: &str = "2026-10-21";
/// Sunucu tarafı yedek modeli destekleyen Claude modelleri
const FALLBACK_MODELS: &[&str] = &["claude-opus-5-5", "claude-opus-5", "claude-fable-5-2", "claude-fable-5-1", "claude-sonnet-5-5"];
/// Güvenlik sınıflandırıcısı reddederse sunucu tarafında otomatik yedek modele geçiş
const FALLBACK_BETA: &str = "server-side-fallback-2026-07-01";

/// Model kullanılamıyor (kaldırılmış / yeni kullanıcılara kapalı / kota bitmiş).
/// Oturum bunu yakalayıp başka bir modele geçerek tekrar dener.
#[derive(Debug, Clone)]
pub struct ModelProblem {
    pub model: String,
    /// "unavailable" (kaldırılmış) | "quota" (kota bitti) | "overloaded" (yanıt vermiyor / 503)
    pub kind: &'static str,
    /// Sağlayıcının önerdiği model (varsa), "sağlayıcı/model" biçiminde değil, yalnız model adı
    pub suggested: Option<String>,
    pub detail: String,
}

impl std::fmt::Display for ModelProblem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.kind {
            "quota" => write!(f, "{} için kota doldu (ücretsiz katman sınırı). Ayrıntı: {}", self.model, self.detail),
            "overloaded" => write!(f, "{} şu an yanıt vermiyor (sağlayıcı yoğun). Ayrıntı: {}", self.model, self.detail),
            _ => write!(f, "{} artık kullanılamıyor. Ayrıntı: {}", self.model, self.detail),
        }
    }
}

impl std::error::Error for ModelProblem {}

/// Sağlayıcı hata gövdesinden model sorununu tanır
pub fn classify_error(model: &str, status: u16, body: &str) -> Option<ModelProblem> {
    let lower = body.to_lowercase();
    let suggested = regex::Regex::new(r"use models/([A-Za-z0-9._\-]+)")
        .ok()
        .and_then(|re| re.captures(body).map(|c| c[1].to_string()));
    let short_detail: String = body
        .split("\"message\"")
        .nth(1)
        .map(|s| s.trim_start_matches([':', ' ', '"']).split('"').next().unwrap_or("").to_string())
        .unwrap_or_else(|| body.chars().take(200).collect());
    if status == 404 && (lower.contains("no longer available") || lower.contains("not found") || lower.contains("does not exist")) {
        return Some(ModelProblem { model: model.into(), kind: "unavailable", suggested, detail: short_detail });
    }
    // Uzun bekleme süreli 429 = günlük/ücretsiz kota bitti (tekrar denemek boşuna)
    if status == 429 {
        let long_wait = regex::Regex::new(r#""retryDelay":\s*"(\d+)s""#)
            .ok()
            .and_then(|re| re.captures(body).and_then(|c| c[1].parse::<u64>().ok()))
            .map(|s| s > 90)
            .unwrap_or(false);
        if long_wait || lower.contains("quota") && lower.contains("limit: 0") {
            return Some(ModelProblem { model: model.into(), kind: "quota", suggested: None, detail: short_detail });
        }
    }
    None
}

#[derive(Debug, Clone)]
pub enum StreamEvent {
    Text(String),
    Thinking(String),
    ToolInputStart,
    /// Geçici durum bilgisi (ör. "model yoğun, tekrar deneniyor")
    Status(String),
}

/// Modelin ilk yanıtı (HTTP başlıkları) için en fazla bekleme süresi
pub const FIRST_BYTE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

#[derive(Debug, Clone)]
pub struct Response {
    pub content: Vec<Value>,
    pub stop_reason: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
}

#[derive(Clone)]
pub struct Client {
    pub(crate) http: reqwest::Client,
}

/// SSE akışını ayrıştırır: bayt düzeyinde tamponlar (çok baytlı UTF-8
/// karakterler parça sınırında bölünse bile bozulmaz) ve `data:` yüklerini döndürür.
#[derive(Default)]
pub(crate) struct SseParser {
    buf: Vec<u8>,
}

impl SseParser {
    pub fn push(&mut self, chunk: &[u8]) -> Vec<String> {
        self.buf.extend_from_slice(chunk);
        let mut out = Vec::new();
        loop {
            let pos = self.buf.windows(2).position(|w| w == b"\n\n");
            let pos_crlf = self.buf.windows(4).position(|w| w == b"\r\n\r\n");
            let (end, sep) = match (pos, pos_crlf) {
                (Some(a), Some(b)) if b < a => (b, 4),
                (Some(a), _) => (a, 2),
                (None, Some(b)) => (b, 4),
                (None, None) => break,
            };
            let raw: Vec<u8> = self.buf.drain(..end + sep).collect();
            let raw = String::from_utf8_lossy(&raw);
            let data: String = raw
                .lines()
                .filter_map(|l| l.strip_prefix("data:"))
                .map(|l| l.trim_start())
                .collect::<Vec<_>>()
                .join("\n");
            if !data.is_empty() {
                out.push(data);
            }
        }
        out
    }
}

pub struct Request<'a> {
    pub model: &'a str,
    pub effort: &'a str,
    pub max_tokens: u32,
    pub system: &'a str,
    pub tools: &'a [Value],
    pub messages: &'a [Value],
    /// None = otomatik; Some("none") = araç kullanma (ör. özetleme isteği)
    pub tool_choice: Option<&'a str>,
}

impl Client {
    pub fn new() -> Self {
        Self {
            http: reqwest::Client::builder()
                .connect_timeout(std::time::Duration::from_secs(20))
                // Akış ortasında sağlayıcı susarsa sonsuza dek beklemeyelim (düşünen modeller için cömert süre)
                .read_timeout(std::time::Duration::from_secs(180))
                .build()
                .expect("http client"),
        }
    }

    /// Streaming istek; metin parçaları `events` kanalına anlık gönderilir.
    /// Ayarlar her istekte okunur: tarayıcı girişiyle eklenen anahtarlar hemen geçerli olur.
    pub async fn stream(
        &self,
        req: Request<'_>,
        events: Option<&UnboundedSender<StreamEvent>>,
    ) -> Result<Response> {
        let cfg = Config::load()?;
        let (pname, provider, model) = cfg.resolve_model(req.model)?;
        let key = provider.key();
        match provider.kind {
            ProviderKind::Openai => {
                crate::openai::stream(&self.http, &provider.base_url, key.as_deref(), &model, &req, events).await
            }
            ProviderKind::Anthropic => {
                let key = key.with_context(|| format!(
                    "'{pname}' için API anahtarı yok: {} ortam değişkenini ayarla veya config.toml'a api_key ekle",
                    provider.api_key_env
                ))?;
                let base = if provider.base_url.is_empty() { "https://api.anthropic.com".to_string() } else { provider.base_url.clone() };
                self.anthropic(&format!("{}/v1/messages", base.trim_end_matches('/')), &key, &model, &req, events).await
            }
        }
    }

    async fn anthropic(
        &self,
        url: &str,
        api_key: &str,
        model: &str,
        req: &Request<'_>,
        events: Option<&UnboundedSender<StreamEvent>>,
    ) -> Result<Response> {
        let mut body = json!({
            "model": model,
            "max_tokens": req.max_tokens,
            "stream": true,
            "system": [{
                "type": "text",
                "text": req.system,
                "cache_control": {"type": "ephemeral"}
            }],
            "thinking": {"type": "adaptive", "display": "summarized"},
            "output_config": {"effort": req.effort},
            "messages": sanitize_for_anthropic(req.messages),
            // Konuşma geçmişini otomatik önbellekle (son bloğa yerleşir)
            "cache_control": {"type": "ephemeral"},
        });
        let use_fallback = FALLBACK_MODELS.contains(&model);
        if use_fallback {
            body["fallbacks"] = json!("default");
        }
        if !req.tools.is_empty() {
            body["tools"] = Value::Array(req.tools.to_vec());
            if let Some(tc) = req.tool_choice {
                body["tool_choice"] = json!({"type": tc});
            }
        }

        let mut attempt = 0;
        let resp = loop {
            attempt += 1;
            let mut rb = self
                .http
                .post(url)
                .header("x-api-key", api_key)
                .header("anthropic-version", API_VERSION)
                .header("content-type", "application/json");
            if use_fallback {
                rb = rb.header("anthropic-beta", FALLBACK_BETA);
            }
            let r = match tokio::time::timeout(FIRST_BYTE_TIMEOUT, rb.json(&body).send()).await {
                Ok(r) => r,
                Err(_) => {
                    return Err(anyhow::Error::new(ModelProblem {
                        model: model.into(),
                        kind: "overloaded",
                        suggested: None,
                        detail: format!("{} sn içinde yanıt gelmedi", FIRST_BYTE_TIMEOUT.as_secs()),
                    }));
                }
            };
            match r {
                Ok(r) if r.status().is_success() => break r,
                Ok(r) => {
                    let status = r.status().as_u16();
                    let text = r.text().await.unwrap_or_default();
                    if let Some(problem) = classify_error(model, status, &text) {
                        return Err(anyhow::Error::new(problem));
                    }
                    let retryable = status == 429 || status == 529 || status >= 500;
                    if retryable && attempt < 4 {
                        tokio::time::sleep(std::time::Duration::from_secs(2u64.pow(attempt))).await;
                        continue;
                    }
                    bail!("Claude API hatası {status}: {text}");
                }
                Err(e) if attempt < 4 => {
                    tokio::time::sleep(std::time::Duration::from_secs(2u64.pow(attempt))).await;
                    let _ = e;
                    continue;
                }
                Err(e) => return Err(e).context("Claude API bağlantı hatası"),
            }
        };

        let mut blocks: Vec<Value> = Vec::new();
        let mut partial_json: Vec<String> = Vec::new();
        let mut stop_reason = String::new();
        let mut input_tokens = 0;
        let mut output_tokens = 0;

        let mut sse = SseParser::default();
        let mut stream = resp.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.context("stream okunamadı")?;
            for data in sse.push(&chunk) {
                let ev: Value = match serde_json::from_str(&data) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                match ev["type"].as_str().unwrap_or("") {
                    "message_start" => {
                        let u = &ev["message"]["usage"];
                        input_tokens = u["input_tokens"].as_u64().unwrap_or(0)
                            + u["cache_read_input_tokens"].as_u64().unwrap_or(0)
                            + u["cache_creation_input_tokens"].as_u64().unwrap_or(0);
                    }
                    "content_block_start" => {
                        let idx = ev["index"].as_u64().unwrap_or(0) as usize;
                        let block = ev["content_block"].clone();
                        if block["type"] == "tool_use" {
                            if let Some(tx) = events {
                                let _ = tx.send(StreamEvent::ToolInputStart);
                            }
                        }
                        while blocks.len() <= idx {
                            blocks.push(Value::Null);
                            partial_json.push(String::new());
                        }
                        blocks[idx] = block;
                    }
                    "content_block_delta" => {
                        let idx = ev["index"].as_u64().unwrap_or(0) as usize;
                        let d = &ev["delta"];
                        let Some(block) = blocks.get_mut(idx) else { continue };
                        match d["type"].as_str().unwrap_or("") {
                            "text_delta" => {
                                let t = d["text"].as_str().unwrap_or("");
                                append_str(block, "text", t);
                                if let Some(tx) = events {
                                    let _ = tx.send(StreamEvent::Text(t.into()));
                                }
                            }
                            "thinking_delta" => {
                                let t = d["thinking"].as_str().unwrap_or("");
                                append_str(block, "thinking", t);
                                if let Some(tx) = events {
                                    let _ = tx.send(StreamEvent::Thinking(t.into()));
                                }
                            }
                            "signature_delta" => {
                                append_str(block, "signature", d["signature"].as_str().unwrap_or(""));
                            }
                            "input_json_delta" => {
                                partial_json[idx].push_str(d["partial_json"].as_str().unwrap_or(""));
                            }
                            "citations_delta" => {
                                if let Some(arr) = block["citations"].as_array_mut() {
                                    arr.push(d["citation"].clone());
                                } else {
                                    block["citations"] = json!([d["citation"].clone()]);
                                }
                            }
                            _ => {}
                        }
                    }
                    "content_block_stop" => {
                        let idx = ev["index"].as_u64().unwrap_or(0) as usize;
                        if let Some(block) = blocks.get_mut(idx) {
                            if block["type"] == "tool_use" || block["type"] == "server_tool_use" {
                                let raw = &partial_json[idx];
                                block["input"] = if raw.trim().is_empty() {
                                    json!({})
                                } else {
                                    // Bozuk JSON'u işaretle; agent döngüsü is_error ile geri bildirir
                                    serde_json::from_str(raw)
                                        .unwrap_or_else(|_| json!({"__invalid_json": raw}))
                                };
                            }
                        }
                    }
                    "message_delta" => {
                        if let Some(s) = ev["delta"]["stop_reason"].as_str() {
                            stop_reason = s.into();
                        }
                        output_tokens = ev["usage"]["output_tokens"].as_u64().unwrap_or(output_tokens);
                    }
                    "error" => bail!("stream hatası: {}", ev["error"]),
                    _ => {}
                }
            }
        }

        blocks.retain(|b| !b.is_null());
        Ok(Response { content: blocks, stop_reason, input_tokens, output_tokens })
    }
}

fn append_str(block: &mut Value, key: &str, s: &str) {
    let cur = block[key].as_str().unwrap_or("").to_string();
    block[key] = Value::String(cur + s);
}

/// Başka sağlayıcılardan kalan ek alanları (ör. Gemini'nin `extra_content` imzası) Anthropic'e göndermeden önce temizler
fn sanitize_for_anthropic(messages: &[Value]) -> Value {
    let needs = messages.iter().any(|m| {
        m["content"].as_array().map(|a| a.iter().any(|b| b.get("extra_content").is_some())).unwrap_or(false)
    });
    if !needs {
        return Value::Array(messages.to_vec());
    }
    Value::Array(
        messages
            .iter()
            .map(|m| {
                let mut m = m.clone();
                if let Some(a) = m["content"].as_array_mut() {
                    for b in a.iter_mut() {
                        if let Some(o) = b.as_object_mut() {
                            o.remove("extra_content");
                        }
                    }
                }
                m
            })
            .collect(),
    )
}

pub fn text_of(content: &[Value]) -> String {
    content
        .iter()
        .filter(|b| b["type"] == "text")
        .filter_map(|b| b["text"].as_str())
        .collect::<Vec<_>>()
        .join("")
}
