//! OpenAI uyumlu /chat/completions adaptörü.
//! OpenAI (GPT), Google Gemini (OpenAI uyumlu uç nokta), OpenRouter (yüzlerce model),
//! Ollama ve LM Studio (yerel açık kaynak modeller) bu yolla çalışır.
//!
//! Geçmiş Anthropic blok biçiminde tutulur; burada OpenAI mesajlarına çevrilir
//! ve yanıt yine Anthropic bloklarına (text / tool_use) dönüştürülür.

use crate::llm::{Request, Response, SseParser, StreamEvent};
use anyhow::{bail, Context, Result};
use futures::StreamExt;
use serde_json::{json, Value};
use tokio::sync::mpsc::UnboundedSender;

fn is_local(base: &str) -> bool {
    base.contains("localhost") || base.contains("127.0.0.1")
}

/// Gemini'nin imzası olmayan (başka modelden gelen) araç çağrıları için tanımladığı doğrulamayı atlama değeri
const GEMINI_SKIP_SIGNATURE: &str = "skip_thought_signature_validator";

/// Anthropic biçimli geçmişi OpenAI mesajlarına çevirir.
/// `gemini`: Gemini 3, araç çağrılarındaki düşünce imzasının (thought_signature) geri gönderilmesini ister.
fn convert_messages(system: &str, messages: &[Value], gemini: bool) -> Vec<Value> {
    let mut out = vec![json!({"role": "system", "content": system})];
    for m in messages {
        let role = m["role"].as_str().unwrap_or("user");
        // Basit metin
        if let Some(s) = m["content"].as_str() {
            out.push(json!({"role": role, "content": s}));
            continue;
        }
        let blocks = m["content"].as_array().cloned().unwrap_or_default();
        if role == "assistant" {
            let text: String = blocks.iter().filter(|b| b["type"] == "text").filter_map(|b| b["text"].as_str()).collect();
            let calls: Vec<Value> = blocks
                .iter()
                .filter(|b| b["type"] == "tool_use")
                .map(|b| {
                    let mut call = json!({
                        "id": b["id"], "type": "function",
                        "function": {"name": b["name"], "arguments": b["input"].to_string()}
                    });
                    // Sağlayıcıya özgü ek alan (Gemini düşünce imzası) olduğu gibi geri gönderilir
                    if b.get("extra_content").is_some() {
                        call["extra_content"] = b["extra_content"].clone();
                    } else if gemini {
                        call["extra_content"] = json!({"google": {"thought_signature": GEMINI_SKIP_SIGNATURE}});
                    }
                    call
                })
                .collect();
            let mut msg = json!({"role": "assistant", "content": if text.is_empty() { Value::Null } else { json!(text) }});
            if !calls.is_empty() {
                msg["tool_calls"] = json!(calls);
            }
            out.push(msg);
            continue;
        }
        // user: tool_result blokları -> role:tool; görseller ayrı user mesajına
        let mut images: Vec<Value> = Vec::new();
        let mut texts: Vec<String> = Vec::new();
        for b in &blocks {
            match b["type"].as_str() {
                Some("tool_result") => {
                    let mut content = String::new();
                    match &b["content"] {
                        Value::String(s) => content.push_str(s),
                        Value::Array(parts) => {
                            for p in parts {
                                match p["type"].as_str() {
                                    Some("text") => content.push_str(p["text"].as_str().unwrap_or("")),
                                    Some("image") => {
                                        images.push(image_part(p));
                                        content.push_str("[görsel aşağıda ekli]");
                                    }
                                    _ => {}
                                }
                            }
                        }
                        _ => {}
                    }
                    if b["is_error"].as_bool().unwrap_or(false) {
                        content = format!("HATA: {content}");
                    }
                    out.push(json!({"role": "tool", "tool_call_id": b["tool_use_id"], "content": content}));
                }
                Some("text") => texts.push(b["text"].as_str().unwrap_or("").into()),
                Some("image") => images.push(image_part(b)),
                Some("document") => texts.push(format!(
                    "[Ek PDF \"{}\" bu modele doğrudan gönderilemiyor; içeriği gerekiyorsa read_file veya pdf skill'i ile okunmalı]",
                    b["title"].as_str().unwrap_or("belge")
                )),
                _ => {}
            }
        }
        if !texts.is_empty() || !images.is_empty() {
            let mut parts: Vec<Value> = texts.into_iter().map(|t| json!({"type": "text", "text": t})).collect();
            if parts.is_empty() {
                parts.push(json!({"type": "text", "text": "Araç sonucundaki görsel:"}));
            }
            parts.extend(images);
            out.push(json!({"role": "user", "content": parts}));
        }
    }
    out
}

fn image_part(b: &Value) -> Value {
    let src = &b["source"];
    json!({"type": "image_url", "image_url": {
        "url": format!("data:{};base64,{}", src["media_type"].as_str().unwrap_or("image/png"), src["data"].as_str().unwrap_or(""))
    }})
}

/// Anthropic araç tanımları -> OpenAI function tanımları (sunucu araçları atlanır)
fn convert_tools(tools: &[Value]) -> Vec<Value> {
    tools
        .iter()
        .filter(|t| t.get("input_schema").is_some())
        .map(|t| json!({"type": "function", "function": {
            "name": t["name"], "description": t["description"], "parameters": t["input_schema"]
        }}))
        .collect()
}

pub async fn stream(
    http: &reqwest::Client,
    base_url: &str,
    key: Option<&str>,
    model: &str,
    req: &Request<'_>,
    events: Option<&UnboundedSender<StreamEvent>>,
) -> Result<Response> {
    let mut body = json!({
        "model": model,
        "stream": true,
        "stream_options": {"include_usage": true},
        "messages": convert_messages(req.system, req.messages, base_url.contains("generativelanguage.googleapis.com")),
    });
    let tools = convert_tools(req.tools);
    if !tools.is_empty() {
        body["tools"] = json!(tools);
        if let Some(tc) = req.tool_choice {
            body["tool_choice"] = json!(tc);
        }
    }
    // Yerel modellerde bağlam sınırı modele bağlı; uzak sağlayıcılarda sınır ver
    if !is_local(base_url) {
        if base_url.contains("api.openai.com") {
            body["max_completion_tokens"] = json!(req.max_tokens.min(32000));
        } else {
            body["max_tokens"] = json!(req.max_tokens.min(32000));
        }
    }

    let url = format!("{}/chat/completions", base_url.trim_end_matches('/'));
    let mut attempt = 0;
    let resp = loop {
        attempt += 1;
        let mut rb = http.post(&url).json(&body);
        if let Some(k) = key {
            rb = rb.bearer_auth(k);
        } else if !is_local(base_url) {
            bail!("Bu sağlayıcı için API anahtarı yok. `/login openrouter` ile tarayıcıdan giriş yap ya da config.toml'a anahtar ekle.");
        }
        if base_url.contains("openrouter.ai") {
            rb = rb.header("X-Title", "RustyAgent");
        }
        let status_note = |s: String| {
            if let Some(tx) = events {
                let _ = tx.send(StreamEvent::Status(s));
            }
        };
        let overloaded = |detail: &str| {
            anyhow::Error::new(crate::llm::ModelProblem { model: model.into(), kind: "overloaded", suggested: None, detail: detail.into() })
        };
        // Yerel modeller ilk yüklemede yavaş olabilir; onlara daha uzun süre tanı
        let first_byte = if is_local(base_url) { std::time::Duration::from_secs(300) } else { crate::llm::FIRST_BYTE_TIMEOUT };
        let sent = match tokio::time::timeout(first_byte, rb.send()).await {
            Ok(r) => r,
            // Sağlayıcı bağlantıyı açık tutup yanıt vermiyor: beklemek yerine yedek modele geçilsin
            Err(_) => return Err(overloaded(&format!("{} sn içinde yanıt gelmedi", first_byte.as_secs()))),
        };
        match sent {
            Ok(r) if r.status().is_success() => break r,
            Ok(r) => {
                let status = r.status().as_u16();
                let text = r.text().await.unwrap_or_default();
                // Kaldırılmış model / bitmiş kota: tekrar denemek boşuna; oturum başka modele geçer
                if let Some(problem) = crate::llm::classify_error(model, status, &text) {
                    return Err(anyhow::Error::new(problem));
                }
                if status == 503 || status == 529 {
                    if attempt < 3 {
                        status_note(format!("{model} yoğun, tekrar deneniyor ({attempt}/2)…"));
                        tokio::time::sleep(std::time::Duration::from_secs(2 * attempt as u64)).await;
                        continue;
                    }
                    return Err(overloaded("sağlayıcı yoğunluk bildirdi (503)"));
                }
                if (status == 429 || status >= 500) && attempt < 4 {
                    status_note(format!("{model} geçici hata verdi ({status}), tekrar deneniyor…"));
                    tokio::time::sleep(std::time::Duration::from_secs(2u64.pow(attempt))).await;
                    continue;
                }
                let msg: String = text
                    .split("\"message\"")
                    .nth(1)
                    .map(|s| s.trim_start_matches([':', ' ', '"']).split('"').next().unwrap_or("").to_string())
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| text.chars().take(400).collect());
                bail!("{model} API hatası {status}: {msg}");
            }
            Err(e) if attempt < 3 && !is_local(base_url) => {
                let _ = e;
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            }
            Err(e) => {
                if is_local(base_url) {
                    return Err(e).context("Yerel model sunucusuna bağlanılamadı (Ollama/LM Studio çalışıyor mu?)");
                }
                return Err(e).context("bağlantı hatası");
            }
        }
    };

    let mut text = String::new();
    // index -> (id, name, arguments, sağlayıcıya özgü ek alan — ör. Gemini düşünce imzası)
    let mut calls: Vec<(String, String, String, Value)> = Vec::new();
    let mut finish = String::new();
    let (mut input_tokens, mut output_tokens) = (0u64, 0u64);

    let mut sse = SseParser::default();
    let mut s = resp.bytes_stream();
    while let Some(chunk) = s.next().await {
        let chunk = chunk.context("stream okunamadı")?;
        for data in sse.push(&chunk) {
            if data.trim() == "[DONE]" {
                continue;
            }
            let Ok(ev) = serde_json::from_str::<Value>(&data) else { continue };
            if let Some(err) = ev.get("error") {
                bail!("{model} stream hatası: {err}");
            }
            if let Some(u) = ev.get("usage").filter(|u| u.is_object()) {
                input_tokens = u["prompt_tokens"].as_u64().unwrap_or(input_tokens);
                output_tokens = u["completion_tokens"].as_u64().unwrap_or(output_tokens);
            }
            let Some(choice) = ev["choices"].get(0) else { continue };
            let d = &choice["delta"];
            if let Some(t) = d["content"].as_str() {
                if !t.is_empty() {
                    text.push_str(t);
                    if let Some(tx) = events {
                        let _ = tx.send(StreamEvent::Text(t.into()));
                    }
                }
            }
            // Akıl yürütme çıktısı veren modeller (DeepSeek-R1, Qwen, gpt-oss...)
            for k in ["reasoning_content", "reasoning"] {
                if let Some(t) = d[k].as_str() {
                    if let Some(tx) = events {
                        let _ = tx.send(StreamEvent::Thinking(t.into()));
                    }
                }
            }
            for tc in d["tool_calls"].as_array().cloned().unwrap_or_default() {
                let idx = tc["index"].as_u64().unwrap_or(calls.len() as u64) as usize;
                while calls.len() <= idx {
                    calls.push((String::new(), String::new(), String::new(), Value::Null));
                }
                let c = &mut calls[idx];
                if tc.get("extra_content").map(|v| !v.is_null()).unwrap_or(false) {
                    c.3 = tc["extra_content"].clone();
                }
                if let Some(id) = tc["id"].as_str() {
                    c.0 = id.into();
                }
                if let Some(n) = tc["function"]["name"].as_str() {
                    if c.1.is_empty() {
                        if let Some(tx) = events {
                            let _ = tx.send(StreamEvent::ToolInputStart);
                        }
                    }
                    c.1.push_str(n);
                }
                if let Some(a) = tc["function"]["arguments"].as_str() {
                    c.2.push_str(a);
                }
            }
            if let Some(f) = choice["finish_reason"].as_str() {
                finish = f.into();
            }
        }
    }

    let mut content = Vec::new();
    if !text.is_empty() {
        content.push(json!({"type": "text", "text": text}));
    }
    for (i, (id, name, args, extra)) in calls.iter().enumerate() {
        if name.is_empty() {
            continue;
        }
        let input = if args.trim().is_empty() {
            json!({})
        } else {
            serde_json::from_str(args).unwrap_or_else(|_| json!({"__invalid_json": args}))
        };
        let id = if id.is_empty() { format!("call_{i}_{}", uuid::Uuid::new_v4().simple()) } else { id.clone() };
        let mut block = json!({"type": "tool_use", "id": id, "name": name, "input": input});
        if !extra.is_null() {
            block["extra_content"] = extra.clone();
        }
        content.push(block);
    }
    let has_calls = content.iter().any(|b| b["type"] == "tool_use");
    let stop_reason = match finish.as_str() {
        _ if has_calls => "tool_use",
        "length" => "max_tokens",
        "content_filter" => "refusal",
        _ => "end_turn",
    }
    .to_string();
    Ok(Response { content, stop_reason, input_tokens, output_tokens })
}

/// Bir sağlayıcının model listesi (/models)
pub async fn list_models(http: &reqwest::Client, base_url: &str, key: Option<&str>) -> Result<Vec<String>> {
    let mut rb = http.get(format!("{}/models", base_url.trim_end_matches('/')));
    if let Some(k) = key {
        rb = rb.bearer_auth(k);
    }
    let v: Value = rb.send().await?.error_for_status()?.json().await?;
    let mut ids: Vec<String> = v["data"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|m| m["id"].as_str().map(|s| s.trim_start_matches("models/").to_string()))
        .collect();
    ids.sort();
    Ok(ids)
}
