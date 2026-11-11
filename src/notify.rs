//! Kullanıcıya bildirim: Telegram mesajı ve (opsiyonel) Twilio telefon araması.
//! Ses ayarları (Deepgram + Cartesia + public_base_url) tamamsa arama çift yönlüdür:
//! agent seni arar, mesajı söyler ve konuşmaya devam eder (`serve` açık olmalı).
//! Değilse Twilio mesajı sesli okuyup kapatır.

use crate::config::Config;
use anyhow::{bail, Result};

#[derive(Clone)]
pub struct Notifier {
    cfg: Config,
    http: reqwest::Client,
}

impl Notifier {
    pub fn new(cfg: Config) -> Self {
        Self { cfg, http: reqwest::Client::new() }
    }

    pub async fn notify(&self, message: &str, call: bool) -> Result<String> {
        let mut done = Vec::new();
        let t = &self.cfg.telegram;
        if !t.bot_token.is_empty() && !t.allowed_chat_ids.is_empty() {
            for chat in &t.allowed_chat_ids {
                crate::telegram::send_message(&self.http, &t.bot_token, *chat, message).await?;
            }
            done.push("Telegram mesajı gönderildi");
        }
        if call {
            self.phone_call(message).await?;
            done.push("telefon araması başlatıldı");
        }
        if done.is_empty() {
            bail!("Bildirim kanalı yapılandırılmamış (config.toml: telegram / voice)");
        }
        Ok(done.join(", "))
    }

    pub async fn phone_call(&self, message: &str) -> Result<()> {
        let v = &self.cfg.voice;
        if v.twilio_account_sid.is_empty() || v.owner_phone_number.is_empty() || v.twilio_phone_number.is_empty() {
            bail!("Twilio ayarları eksik (config.toml [voice])");
        }
        // Ses sunucusu ayarlıysa: çift yönlü gerçek zamanlı konuşma; değilse mesajı okuyup kapat
        let twiml = if v.realtime_ready() {
            crate::voice::stream_twiml(v, Some(message))
        } else {
            format!(
                "<Response><Say language=\"tr-TR\">{}</Say><Pause length=\"1\"/><Say language=\"tr-TR\">{}</Say></Response>",
                xml_escape(message),
                xml_escape(message)
            )
        };
        let url = format!("https://api.twilio.com/2010-04-01/Accounts/{}/Calls.json", v.twilio_account_sid);
        let r = self
            .http
            .post(url)
            .basic_auth(&v.twilio_account_sid, Some(&v.twilio_auth_token))
            .form(&[
                ("To", v.owner_phone_number.as_str()),
                ("From", v.twilio_phone_number.as_str()),
                ("Twiml", twiml.as_str()),
            ])
            .send()
            .await?;
        if !r.status().is_success() {
            bail!("Twilio hatası: {}", r.text().await.unwrap_or_default());
        }
        Ok(())
    }
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}
