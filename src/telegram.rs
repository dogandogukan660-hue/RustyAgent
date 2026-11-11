//! Telegram botu (Bot API, long polling). Her sohbet için ayrı ana agent oturumu.
//! Güvenlik: sadece config'teki allowed_chat_ids kabul edilir. Chat id'ni
//! öğrenmek için bota /start yaz; bot sana id'ni söyler.

use crate::agent::{EventKind, Runtime};
use anyhow::{bail, Result};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::mpsc;

pub async fn send_message(http: &reqwest::Client, token: &str, chat_id: i64, text: &str) -> Result<()> {
    // Telegram sınırı 4096 karakter
    let chars: Vec<char> = text.chars().collect();
    for chunk in chars.chunks(4000) {
        let body: String = chunk.iter().collect();
        let r = http
            .post(format!("https://api.telegram.org/bot{token}/sendMessage"))
            .json(&json!({"chat_id": chat_id, "text": body}))
            .send()
            .await?;
        if !r.status().is_success() {
            bail!("Telegram hatası: {}", r.text().await.unwrap_or_default());
        }
    }
    Ok(())
}

async fn send_typing(http: &reqwest::Client, token: &str, chat_id: i64) {
    let _ = http
        .post(format!("https://api.telegram.org/bot{token}/sendChatAction"))
        .json(&json!({"chat_id": chat_id, "action": "typing"}))
        .send()
        .await;
}

pub async fn run(rt: Arc<Runtime>) -> Result<()> {
    let token = rt.cfg.telegram.bot_token.clone();
    if token.is_empty() {
        bail!("config.toml içinde [telegram] bot_token ayarlı değil (BotFather'dan al)");
    }
    let http = reqwest::Client::new();
    let mut offset: i64 = 0;
    let mut chats: HashMap<i64, mpsc::UnboundedSender<(String, bool)>> = HashMap::new();
    println!("Telegram botu dinliyor…");

    loop {
        let resp = http
            .get(format!("https://api.telegram.org/bot{token}/getUpdates"))
            .query(&[("timeout", "50"), ("offset", &offset.to_string())])
            .timeout(std::time::Duration::from_secs(60))
            .send()
            .await;
        let v: Value = match resp {
            Ok(r) => r.json().await.unwrap_or_default(),
            Err(_) => {
                tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                continue;
            }
        };
        for upd in v["result"].as_array().cloned().unwrap_or_default() {
            offset = upd["update_id"].as_i64().unwrap_or(0) + 1;
            // Onay butonları: callback_data = "ap|<id>|y|s|n"
            if let Some(cq) = upd.get("callback_query") {
                let from_chat = cq["message"]["chat"]["id"].as_i64().unwrap_or(0);
                let data = cq["data"].as_str().unwrap_or("");
                let parts: Vec<&str> = data.split('|').collect();
                let mut answer = "Yetkisiz".to_string();
                if rt.cfg.telegram.allowed_chat_ids.contains(&from_chat) && parts.len() == 3 && parts[0] == "ap" {
                    let d = match parts[2] {
                        "y" => crate::permissions::Decision::Allow,
                        "s" => crate::permissions::Decision::AllowSession,
                        _ => crate::permissions::Decision::Deny,
                    };
                    answer = if crate::permissions::resolve(parts[1], d) {
                        match d {
                            crate::permissions::Decision::Deny => "Reddedildi".into(),
                            crate::permissions::Decision::AllowSession => "Oturum boyunca izin verildi".into(),
                            _ => "Onaylandı".into(),
                        }
                    } else {
                        "Bu istek artık geçerli değil".into()
                    };
                    // Butonları kaldır, kararı mesaja yaz
                    let old = cq["message"]["text"].as_str().unwrap_or("");
                    let _ = http
                        .post(format!("https://api.telegram.org/bot{token}/editMessageText"))
                        .json(&json!({"chat_id": from_chat, "message_id": cq["message"]["message_id"], "text": format!("{old}\n\n→ {answer}")}))
                        .send()
                        .await;
                }
                let _ = http
                    .post(format!("https://api.telegram.org/bot{token}/answerCallbackQuery"))
                    .json(&json!({"callback_query_id": cq["id"], "text": answer}))
                    .send()
                    .await;
                continue;
            }
            let msg = &upd["message"];
            let Some(chat_id) = msg["chat"]["id"].as_i64() else { continue };
            let text = msg["text"].as_str().unwrap_or("").to_string();

            if !rt.cfg.telegram.allowed_chat_ids.contains(&chat_id) {
                let _ = send_message(&http, &token, chat_id,
                    &format!("Yetkisiz. Chat id'n: {chat_id}\nBunu ~/.rustyagent/config.toml içinde telegram.allowed_chat_ids listesine ekle.")).await;
                continue;
            }
            let tx = chats.entry(chat_id).or_insert_with(|| spawn_chat(rt.clone(), http.clone(), token.clone(), chat_id));
            if let Some(file_id) = msg["voice"]["file_id"].as_str().or(msg["audio"]["file_id"].as_str()) {
                // Sesli mesaj: indir → yazıya çevir → agent'a ver (yanıt da sesli gelir)
                let (rt2, http2, token2, tx2, file_id) = (rt.clone(), http.clone(), token.clone(), tx.clone(), file_id.to_string());
                tokio::spawn(async move {
                    match transcribe_voice(&rt2, &http2, &token2, &file_id).await {
                        Ok(t) if !t.trim().is_empty() => {
                            let _ = send_message(&http2, &token2, chat_id, &format!("Sesli mesaj: {t}")).await;
                            let _ = tx2.send((t, true));
                        }
                        Ok(_) => { let _ = send_message(&http2, &token2, chat_id, "Sesli mesajda konuşma algılanmadı.").await; }
                        Err(e) => { let _ = send_message(&http2, &token2, chat_id, &format!("Ses çözülemedi: {e:#}")).await; }
                    }
                });
                continue;
            }
            if text.is_empty() {
                continue;
            }
            let _ = tx.send((text, false));
        }
    }
}

/// Bir sohbet için sıralı mesaj işleyici
async fn transcribe_voice(rt: &Runtime, http: &reqwest::Client, token: &str, file_id: &str) -> Result<String> {
    let v: Value = http
        .get(format!("https://api.telegram.org/bot{token}/getFile"))
        .query(&[("file_id", file_id)])
        .send().await?.json().await?;
    let path = v["result"]["file_path"].as_str().ok_or_else(|| anyhow::anyhow!("dosya yolu alınamadı"))?;
    let audio = http.get(format!("https://api.telegram.org/file/bot{token}/{path}")).send().await?.bytes().await?;
    let mime = if path.ends_with(".mp3") { "audio/mpeg" } else { "audio/ogg" };
    crate::voice::transcribe(&rt.cfg.voice, audio.to_vec(), mime).await
}

async fn send_voice(http: &reqwest::Client, token: &str, chat_id: i64, mp3: Vec<u8>) -> Result<()> {
    let part = reqwest::multipart::Part::bytes(mp3).file_name("yanit.mp3").mime_str("audio/mpeg")?;
    let form = reqwest::multipart::Form::new().text("chat_id", chat_id.to_string()).part("voice", part);
    let r = http.post(format!("https://api.telegram.org/bot{token}/sendVoice")).multipart(form).send().await?;
    if !r.status().is_success() {
        bail!("sendVoice hatası: {}", r.text().await.unwrap_or_default());
    }
    Ok(())
}

fn spawn_chat(rt: Arc<Runtime>, http: reqwest::Client, token: String, chat_id: i64) -> mpsc::UnboundedSender<(String, bool)> {
    let (tx, mut rx) = mpsc::unbounded_channel::<(String, bool)>();
    tokio::spawn(async move {
        let (etx, mut erx) = mpsc::unbounded_channel();
        let mut session = rt.main_session(Some(etx));
        // Alt agent ilerlemesini kısa notlar halinde ilet
        let (h2, t2) = (http.clone(), token.clone());
        tokio::spawn(async move {
            while let Some(ev) = erx.recv().await {
                match &ev.kind {
                    EventKind::ToolCall { name, .. } if name == "delegate" => {
                        let _ = send_message(&h2, &t2, chat_id, "Görev ekibe dağıtılıyor…").await;
                    }
                    // Onay isteği: bu sohbete butonlu mesaj olarak sor
                    EventKind::Approval(h) => {
                        let h = h.clone();
                        tokio::spawn(async move {
                            let d = crate::permissions::ask_telegram(&h.req, &[chat_id]).await;
                            h.respond(d);
                        });
                    }
                    EventKind::Info(s) => {
                        let _ = send_message(&h2, &t2, chat_id, &format!("Bilgi: {s}")).await;
                    }
                    _ => {}
                }
            }
        });
        while let Some((text, as_voice)) = rx.recv().await {
            match text.trim() {
                "/start" => {
                    let _ = send_message(&http, &token, chat_id, "RustyAgent hazır. Bana bir iş ver. /reset ile sohbeti sıfırla.").await;
                    continue;
                }
                "/reset" => {
                    session.reset();
                    let _ = send_message(&http, &token, chat_id, "Sohbet sıfırlandı.").await;
                    continue;
                }
                _ => {}
            }
            send_typing(&http, &token, chat_id).await;
            let reply = match session.send(&text).await {
                Ok(t) if !t.trim().is_empty() => t,
                Ok(_) => "Tamam.".into(),
                Err(e) => format!("Hata: {e:#}"),
            };
            let _ = send_message(&http, &token, chat_id, &reply).await;
            // Sesli soruya sesli yanıt da ver (Cartesia ayarlıysa; çok uzun metinleri atla)
            if as_voice && reply.chars().count() < 1500 {
                if let Ok(mp3) = crate::voice::synthesize_mp3(&rt.cfg.voice, &reply).await {
                    let _ = send_voice(&http, &token, chat_id, mp3).await;
                }
            }
        }
    });
    tx
}
