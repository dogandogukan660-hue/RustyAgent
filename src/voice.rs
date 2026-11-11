//! Gerçek zamanlı sesli konuşma (telefon).
//!
//!   Telefon ⇄ Twilio Media Streams (WebSocket, 8 kHz μ-law)
//!     → Deepgram streaming STT (ara + kesin sonuçlar, uç nokta algılama)
//!     → Ana agent (yanıt akış halinde)
//!     → cümle cümle Cartesia TTS (doğrudan μ-law 8 kHz, dönüştürme yok)
//!     → Twilio → telefon
//!
//! Gecikmeyi düşüren noktalar: her aşama akış halinde; yanıtın ilk cümlesi
//! biter bitmez seslendirilir; TTS çıktısı Twilio'nun biçiminde istenir.
//! Söz kesme (barge-in): agent konuşurken kullanıcı konuşmaya başlarsa
//! ses anında durdurulur (Twilio "clear") ve yanıt üretimi iptal edilir.
//!
//! Güvenlik: gelen aramalar yalnızca owner_phone_number'dan kabul edilir;
//! ses akışı gizli bir anahtarla (stream_secret) doğrulanır.

use crate::agent::{AgentEvent, EventKind, Runtime, MAIN_AGENT};
use crate::config::{Config, VoiceConfig};
use anyhow::{bail, Context, Result};
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Form, State};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use base64::Engine;
use futures::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, Mutex};
use tokio_tungstenite::tungstenite::{client::IntoClientRequest, Message as WsMsg};

const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::STANDARD;
const FLUSH: &str = "__voice_flush__";
const RESET: &str = "__voice_reset__";

const VOICE_PROMPT: &str = "# Sesli konuşma kipi\n\
Şu an kullanıcıyla TELEFONDA konuşuyorsun. Söylediklerin sese çevrilecek:\n\
- Kısa, doğal, konuşma dilinde cümleler kur (genelde 1-3 cümle).\n\
- Markdown, liste, emoji, kod, URL kullanma; sayıları ve kısaltmaları okunacak biçimde yaz.\n\
- Uzun bir iş gerekiyorsa önce kısaca ne yapacağını söyle, sonra araçlarla yap ve sonucu kısaca özetle.\n\
- Kullanıcının sözü yazıya çevrilmiş halde geliyor; tanıma hataları olabilir, anlam çıkar ve gerekirse kısaca sor.";

// ------------------------------------------------------------------ HTTP sunucusu

pub async fn serve(rt: Arc<Runtime>) -> Result<()> {
    let v = &rt.cfg.voice;
    if !v.realtime_ready() {
        bail!("Sesli konuşma için config.toml [voice] içinde public_base_url, deepgram_api_key, cartesia_api_key ve cartesia_voice_id gerekli");
    }
    let port = v.listen_port;
    let app = Router::new()
        .route("/health", get(|| async { "ok" }))
        .route("/twilio/voice", post(incoming_call))
        .route("/twilio/stream", get(stream_ws))
        .with_state(rt.clone());
    let listener = tokio::net::TcpListener::bind(("0.0.0.0", port)).await?;
    println!("Ses sunucusu :{port} üzerinde. Twilio numarasının 'A call comes in' webhook'u: {}/twilio/voice", v.public_base_url.trim_end_matches('/'));
    axum::serve(listener, app).await?;
    Ok(())
}

/// Twilio'ya, aramayı ses akışımıza bağlamasını söyleyen TwiML
pub fn stream_twiml(v: &VoiceConfig, greeting: Option<&str>) -> String {
    let host = v.public_base_url.trim_end_matches('/').replacen("https://", "wss://", 1).replacen("http://", "ws://", 1);
    let greeting = greeting
        .map(|g| format!("<Parameter name=\"greeting\" value=\"{}\"/>", xml_escape(&g.chars().take(400).collect::<String>())))
        .unwrap_or_default();
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?><Response><Connect><Stream url=\"{host}/twilio/stream\">\
         <Parameter name=\"secret\" value=\"{}\"/>{greeting}</Stream></Connect></Response>",
        v.stream_secret
    )
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

fn twiml(body: String) -> Response {
    ([("content-type", "text/xml")], body).into_response()
}

async fn incoming_call(State(rt): State<Arc<Runtime>>, Form(form): Form<HashMap<String, String>>) -> Response {
    let v = &rt.cfg.voice;
    let from = form.get("From").map(|s| s.as_str()).unwrap_or("");
    if v.owner_phone_number.is_empty() || from != v.owner_phone_number {
        eprintln!("Yetkisiz arama reddedildi: {from}");
        return twiml("<?xml version=\"1.0\" encoding=\"UTF-8\"?><Response><Reject/></Response>".into());
    }
    twiml(stream_twiml(v, None))
}

async fn stream_ws(State(rt): State<Arc<Runtime>>, ws: WebSocketUpgrade) -> Response {
    ws.on_upgrade(move |socket| async move {
        if let Err(e) = handle_call(socket, rt).await {
            eprintln!("Arama hatası: {e:#}");
        }
    })
}

// ------------------------------------------------------------------ konuşmacı (TTS → Twilio)

struct Speaker {
    epoch: AtomicU64,
    /// Söz kesmeden sonra, eski turun kuyruktaki metni işlenene (RESET) kadar sessiz kal
    muted: std::sync::atomic::AtomicBool,
    playing_until: std::sync::Mutex<Instant>,
    queue: mpsc::UnboundedSender<(u64, String)>,
    out: mpsc::UnboundedSender<String>,
    stream_sid: String,
}

impl Speaker {
    fn say(&self, text: &str) {
        let t = text.trim();
        if !t.is_empty() && !self.muted.load(Ordering::SeqCst) {
            let _ = self.queue.send((self.epoch.load(Ordering::SeqCst), t.to_string()));
        }
    }
    fn is_speaking(&self) -> bool {
        Instant::now() < *self.playing_until.lock().unwrap()
    }
    /// Söz kesme: kuyruğu geçersiz kıl ve Twilio'daki tamponu boşalt
    fn stop(&self) {
        self.muted.store(true, Ordering::SeqCst);
        self.epoch.fetch_add(1, Ordering::SeqCst);
        *self.playing_until.lock().unwrap() = Instant::now();
        let _ = self.out.send(json!({"event": "clear", "streamSid": self.stream_sid}).to_string());
    }
    fn played(&self, bytes: usize) {
        // μ-law 8 kHz: 8000 bayt = 1 sn
        let dur = Duration::from_micros(bytes as u64 * 125);
        let mut p = self.playing_until.lock().unwrap();
        let now = Instant::now();
        *p = (*p).max(now) + dur;
    }
}

async fn tts_worker(sp: Arc<Speaker>, mut rx: mpsc::UnboundedReceiver<(u64, String)>, v: VoiceConfig) {
    let http = reqwest::Client::new();
    while let Some((g, text)) = rx.recv().await {
        if g != sp.epoch.load(Ordering::SeqCst) {
            continue;
        }
        let resp = http
            .post("https://api.cartesia.ai/tts/bytes")
            .header("X-API-Key", &v.cartesia_api_key)
            .header("Cartesia-Version", "2025-04-16")
            .json(&json!({
                "model_id": v.cartesia_model,
                "transcript": text,
                "voice": {"mode": "id", "id": v.cartesia_voice_id},
                "output_format": {"container": "raw", "encoding": "pcm_mulaw", "sample_rate": 8000},
                "language": v.language,
            }))
            .send()
            .await;
        let resp = match resp {
            Ok(r) if r.status().is_success() => r,
            Ok(r) => {
                eprintln!("Cartesia hatası: {}", r.text().await.unwrap_or_default());
                continue;
            }
            Err(e) => {
                eprintln!("Cartesia bağlantı hatası: {e}");
                continue;
            }
        };
        let mut s = resp.bytes_stream();
        let mut pending: Vec<u8> = Vec::new();
        while let Some(Ok(chunk)) = s.next().await {
            if g != sp.epoch.load(Ordering::SeqCst) {
                break; // kesildi
            }
            pending.extend_from_slice(&chunk);
            // ~80 ms'lik paketler halinde gönder
            while pending.len() >= 640 {
                let part: Vec<u8> = pending.drain(..640).collect();
                send_audio(&sp, &part);
            }
        }
        if g == sp.epoch.load(Ordering::SeqCst) && !pending.is_empty() {
            send_audio(&sp, &pending);
        }
    }
}

fn send_audio(sp: &Speaker, bytes: &[u8]) {
    sp.played(bytes.len());
    let _ = sp.out.send(json!({"event": "media", "streamSid": sp.stream_sid, "media": {"payload": B64.encode(bytes)}}).to_string());
}

/// Akan metni cümlelere böler (ilk cümle mümkün olan en erken seslendirilsin diye)
#[derive(Default)]
struct SentenceSplitter {
    buf: String,
}

impl SentenceSplitter {
    fn push(&mut self, t: &str) -> Vec<String> {
        self.buf.push_str(t);
        let mut out = Vec::new();
        loop {
            let cut = self.buf.char_indices().find(|(i, c)| {
                matches!(c, '.' | '!' | '?' | '\n' | ':' | ';')
                    && *i >= 12
                    // "3.5" gibi ondalık sayıları bölme
                    && !self.buf[i + c.len_utf8()..].starts_with(|n: char| n.is_ascii_digit())
            });
            let Some((i, c)) = cut else { break };
            let end = i + c.len_utf8();
            // Cümle sonundan sonra en az bir karakter gelmiş olmalı (ondalık kontrolü için)
            if end >= self.buf.len() {
                break;
            }
            let s: String = self.buf.drain(..end).collect();
            out.push(clean_for_speech(&s));
        }
        out
    }
    fn flush(&mut self) -> Option<String> {
        let s = std::mem::take(&mut self.buf);
        let s = clean_for_speech(&s);
        if s.trim().is_empty() { None } else { Some(s) }
    }
}

fn clean_for_speech(s: &str) -> String {
    s.replace(['*', '#', '`', '_', '>'], "").replace("  ", " ").trim().to_string()
}

// ------------------------------------------------------------------ Deepgram STT

enum Heard {
    Interim(String),
    Final(String),
    EndOfTurn,
}

async fn connect_deepgram(v: &VoiceConfig, tx: mpsc::UnboundedSender<Heard>) -> Result<mpsc::UnboundedSender<Vec<u8>>> {
    let url = format!(
        "wss://api.deepgram.com/v1/listen?model={}&language={}&encoding=mulaw&sample_rate=8000&channels=1\
         &interim_results=true&endpointing=300&utterance_end_ms=1000&vad_events=true&smart_format=true",
        v.deepgram_model, v.language
    );
    let mut req = url.into_client_request()?;
    req.headers_mut().insert("Authorization", format!("Token {}", v.deepgram_api_key).parse()?);
    let (ws, _) = tokio_tungstenite::connect_async(req)
        .await
        .context("Deepgram'a bağlanılamadı (anahtarı ve model/dil uyumunu kontrol et; Türkçe için nova-2 veya nova-3)")?;
    let (mut sink, mut stream) = ws.split();

    let (audio_tx, mut audio_rx) = mpsc::unbounded_channel::<Vec<u8>>();
    tokio::spawn(async move {
        while let Some(a) = audio_rx.recv().await {
            if a.is_empty() {
                let _ = sink.send(WsMsg::Text(r#"{"type":"CloseStream"}"#.into())).await;
                break;
            }
            if sink.send(WsMsg::Binary(a.into())).await.is_err() {
                break;
            }
        }
    });
    tokio::spawn(async move {
        while let Some(Ok(msg)) = stream.next().await {
            let WsMsg::Text(t) = msg else { continue };
            let Ok(v) = serde_json::from_str::<Value>(&t) else { continue };
            match v["type"].as_str() {
                Some("Results") => {
                    let text = v["channel"]["alternatives"][0]["transcript"].as_str().unwrap_or("").trim().to_string();
                    let is_final = v["is_final"].as_bool().unwrap_or(false);
                    if !text.is_empty() {
                        let _ = tx.send(if is_final { Heard::Final(text) } else { Heard::Interim(text) });
                    }
                    if v["speech_final"].as_bool().unwrap_or(false) {
                        let _ = tx.send(Heard::EndOfTurn);
                    }
                }
                Some("UtteranceEnd") => {
                    let _ = tx.send(Heard::EndOfTurn);
                }
                _ => {}
            }
        }
    });
    Ok(audio_tx)
}

// ------------------------------------------------------------------ bir arama

async fn handle_call(socket: WebSocket, rt: Arc<Runtime>) -> Result<()> {
    let v = Config::load()?.voice;
    let (mut tw_sink, mut tw_stream) = socket.split();

    // Twilio'ya giden mesajlar tek bir görevden yazılır
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<String>();
    tokio::spawn(async move {
        while let Some(m) = out_rx.recv().await {
            if tw_sink.send(Message::Text(m.into())).await.is_err() {
                break;
            }
        }
    });

    // "start" olayını bekle
    let (stream_sid, params) = loop {
        let Some(Ok(Message::Text(t))) = tw_stream.next().await else { bail!("akış başlamadan kapandı") };
        let ev: Value = serde_json::from_str(&t).unwrap_or_default();
        if ev["event"] == "start" {
            break (ev["start"]["streamSid"].as_str().unwrap_or("").to_string(), ev["start"]["customParameters"].clone());
        }
    };
    if params["secret"].as_str() != Some(v.stream_secret.as_str()) {
        bail!("geçersiz akış anahtarı — bağlantı reddedildi");
    }
    let greeting = params["greeting"].as_str().map(String::from);

    // STT
    let (heard_tx, mut heard_rx) = mpsc::unbounded_channel::<Heard>();
    let audio_tx = connect_deepgram(&v, heard_tx).await?;

    // Twilio sesi → Deepgram
    let audio_fwd = audio_tx.clone();
    let reader = tokio::spawn(async move {
        while let Some(Ok(msg)) = tw_stream.next().await {
            let Message::Text(t) = msg else { continue };
            let Ok(ev) = serde_json::from_str::<Value>(&t) else { continue };
            match ev["event"].as_str() {
                Some("media") => {
                    if let Ok(bytes) = B64.decode(ev["media"]["payload"].as_str().unwrap_or("")) {
                        let _ = audio_fwd.send(bytes);
                    }
                }
                Some("stop") => break,
                _ => {}
            }
        }
        let _ = audio_fwd.send(Vec::new()); // Deepgram'ı kapat
    });

    // TTS
    let (q_tx, q_rx) = mpsc::unbounded_channel();
    let speaker = Arc::new(Speaker {
        epoch: AtomicU64::new(0),
        muted: std::sync::atomic::AtomicBool::new(false),
        playing_until: std::sync::Mutex::new(Instant::now()),
        queue: q_tx,
        out: out_tx.clone(),
        stream_sid,
    });
    tokio::spawn(tts_worker(speaker.clone(), q_rx, v.clone()));

    // Agent oturumu (sesli kip talimatıyla)
    let extra = match &greeting {
        Some(g) => format!("{VOICE_PROMPT}\n\nBu aramayı SEN başlattın ve kullanıcıya şunu ilettin: \"{g}\". Kullanıcının yanıtına göre devam et."),
        None => VOICE_PROMPT.to_string(),
    };
    let (ev_tx, mut ev_rx) = mpsc::unbounded_channel::<AgentEvent>();
    let mut session = rt.main_session_with(&extra, Some(ev_tx.clone()));
    if !v.model.is_empty() {
        session.model = v.model.clone();
    }
    session.effort = v.effort.clone();
    let session = Arc::new(Mutex::new(session));

    // Agent olayları → cümleler → ses
    {
        let sp = speaker.clone();
        tokio::spawn(async move {
            let mut splitter = SentenceSplitter::default();
            let mut spoke_this_turn = false;
            while let Some(e) = ev_rx.recv().await {
                if e.agent == RESET {
                    sp.muted.store(false, Ordering::SeqCst);
                    splitter = SentenceSplitter::default();
                    spoke_this_turn = false;
                    continue;
                }
                if e.agent == FLUSH {
                    if let Some(s) = splitter.flush() {
                        sp.say(&s);
                    }
                    spoke_this_turn = false;
                    continue;
                }
                // Onay: telefonda sesli söyle, kararı Telegram butonlarından al
                if let EventKind::Approval(h) = &e.kind {
                    if let Some(s) = splitter.flush() {
                        sp.say(&s);
                    }
                    sp.say("Bu işlem için onayın gerekiyor, Telegram'a gönderdim. Onaylayınca devam edeceğim.");
                    let h = h.clone();
                    tokio::spawn(async move {
                        let d = crate::permissions::ask_telegram(&h.req, &[]).await;
                        h.respond(d);
                    });
                    continue;
                }
                if e.agent != MAIN_AGENT {
                    continue;
                }
                match e.kind {
                    EventKind::Text(t) => {
                        for s in splitter.push(&t) {
                            sp.say(&s);
                            spoke_this_turn = true;
                        }
                    }
                    EventKind::ToolCall { .. } => {
                        if let Some(s) = splitter.flush() {
                            sp.say(&s);
                            spoke_this_turn = true;
                        }
                        if !spoke_this_turn {
                            sp.say("Bir saniye, hemen bakıyorum.");
                            spoke_this_turn = true;
                        }
                    }
                    EventKind::Error(_) => sp.say("Bir sorun oluştu, tekrar dener misin?"),
                    _ => {}
                }
            }
        });
    }

    speaker.say(greeting.as_deref().unwrap_or("Merhaba, dinliyorum."));

    // Konuşma döngüsü
    let mut pending = String::new();
    let mut queued: Vec<String> = Vec::new();
    let mut turn: Option<(tokio::task::JoinHandle<()>, usize)> = None;
    loop {
        // Bitmiş turu temizle; kuyrukta bekleyen söz varsa onu işle
        if let Some((h, _)) = &turn {
            if h.is_finished() {
                turn = None;
            }
        }
        if turn.is_none() && !queued.is_empty() {
            let text = queued.drain(..).collect::<Vec<_>>().join(" ");
            turn = Some(start_turn(session.clone(), ev_tx.clone(), text).await);
        }

        let heard = tokio::select! {
            h = heard_rx.recv() => h,
            _ = tokio::time::sleep(Duration::from_millis(200)) => continue,
        };
        let Some(heard) = heard else { break }; // arama bitti
        match heard {
            Heard::Interim(t) => {
                if speaker.is_speaking() && t.chars().count() >= 3 {
                    barge_in(&speaker, &mut turn, &session, &ev_tx).await;
                }
            }
            Heard::Final(t) => {
                if speaker.is_speaking() && t.chars().count() >= 3 {
                    barge_in(&speaker, &mut turn, &session, &ev_tx).await;
                }
                pending.push_str(&t);
                pending.push(' ');
            }
            Heard::EndOfTurn => {
                let text = pending.trim().to_string();
                pending.clear();
                if text.is_empty() {
                    continue;
                }
                if turn.is_some() {
                    // Agent hâlâ çalışıyor (araç kullanıyor): sözü sıraya al
                    queued.push(text);
                    speaker.say("Not aldım, elimdeki işi bitirince ona geçeceğim.");
                } else {
                    turn = Some(start_turn(session.clone(), ev_tx.clone(), text).await);
                }
            }
        }
    }

    if let Some((h, _)) = turn {
        h.abort();
    }
    reader.abort();
    Ok(())
}

/// Söz kesme: konuşmayı durdur, yanıt üretimini iptal et, geçmişi tur öncesine döndür
async fn barge_in(
    speaker: &Speaker,
    turn: &mut Option<(tokio::task::JoinHandle<()>, usize)>,
    session: &Arc<Mutex<crate::agent::Session>>,
    ev_tx: &mpsc::UnboundedSender<AgentEvent>,
) {
    speaker.stop();
    if let Some((h, len)) = turn.take() {
        h.abort();
        let _ = h.await;
        session.lock().await.messages.truncate(len);
    }
    // Cümle tamponunda kalan yarım metni seslendirmeden at
    let _ = ev_tx.send(AgentEvent { agent: RESET.into(), kind: EventKind::Text(String::new()) });
}

/// Agent turunu ayrı görevde başlatır; iptal için tur öncesi geçmiş uzunluğunu döndürür
async fn start_turn(
    session: Arc<Mutex<crate::agent::Session>>,
    ev_tx: mpsc::UnboundedSender<AgentEvent>,
    text: String,
) -> (tokio::task::JoinHandle<()>, usize) {
    let len = session.lock().await.messages.len();
    let h = tokio::spawn(async move {
        let mut s = session.lock().await;
        let _ = s.send(&text).await;
        let _ = ev_tx.send(AgentEvent { agent: FLUSH.into(), kind: EventKind::Text(String::new()) });
    });
    (h, len)
}

// ------------------------------------------------------------------ Telegram sesli mesaj desteği

/// Kayıtlı bir ses dosyasını yazıya çevirir (Deepgram)
pub async fn transcribe(v: &VoiceConfig, audio: Vec<u8>, mime: &str) -> Result<String> {
    if v.deepgram_api_key.is_empty() {
        bail!("Sesli mesaj için config.toml [voice] deepgram_api_key gerekli");
    }
    let r: Value = reqwest::Client::new()
        .post("https://api.deepgram.com/v1/listen")
        .query(&[("model", v.deepgram_model.as_str()), ("language", v.language.as_str()), ("smart_format", "true")])
        .header("Authorization", format!("Token {}", v.deepgram_api_key))
        .header("Content-Type", mime)
        .body(audio)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    Ok(r["results"]["channels"][0]["alternatives"][0]["transcript"].as_str().unwrap_or("").to_string())
}

/// Metni MP3 sese çevirir (Cartesia) — Telegram sesli yanıt için
pub async fn synthesize_mp3(v: &VoiceConfig, text: &str) -> Result<Vec<u8>> {
    if v.cartesia_api_key.is_empty() || v.cartesia_voice_id.is_empty() {
        bail!("Cartesia ayarlı değil");
    }
    let r = reqwest::Client::new()
        .post("https://api.cartesia.ai/tts/bytes")
        .header("X-API-Key", &v.cartesia_api_key)
        .header("Cartesia-Version", "2025-04-16")
        .json(&json!({
            "model_id": v.cartesia_model,
            "transcript": clean_for_speech(text),
            "voice": {"mode": "id", "id": v.cartesia_voice_id},
            "output_format": {"container": "mp3", "sample_rate": 44100, "bit_rate": 128000},
            "language": v.language,
        }))
        .send()
        .await?
        .error_for_status()?;
    Ok(r.bytes().await?.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splitter_cuts_sentences_but_not_decimals() {
        let mut s = SentenceSplitter::default();
        let mut out = s.push("Sıcaklık bugün 3.5 derece olacak. Yarın");
        out.extend(s.push(" daha soğuk!"));
        out.extend(s.flush());
        assert_eq!(out, vec!["Sıcaklık bugün 3.5 derece olacak.", "Yarın daha soğuk!"]);
    }

    #[test]
    fn twiml_has_secret_and_wss() {
        let v = VoiceConfig { public_base_url: "https://abc.trycloudflare.com".into(), stream_secret: "s3".into(), ..Default::default() };
        let x = stream_twiml(&v, Some("İş bitti & hazır"));
        assert!(x.contains("wss://abc.trycloudflare.com/twilio/stream"));
        assert!(x.contains("value=\"s3\""));
        assert!(x.contains("İş bitti &amp; hazır"));
    }
}
