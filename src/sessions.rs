//! Oturum kaydı: ana agent konuşmaları ~/.rustyagent/sessions/<id>.json içine
//! her turdan sonra yazılır; `/resume` veya `rusty-agent -c` ile devam edilir.
//!
//! Devam ederken kaydedilmiş system prompt aynen kullanılır ve geçmişteki
//! düşünce (thinking) blokları çıkarılır: bu blokları doğrulayan API kontrolü
//! araç listesi değişmişse 400 verir; tüm thinking bloklarını kaldırmak ise her zaman geçerlidir.

use crate::config;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Saved {
    pub id: String,
    pub agent: String,
    pub model: String,
    pub title: String,
    pub created: String,
    pub updated: String,
    pub system: String,
    pub messages: Vec<Value>,
    /// Sıkıştırma sonrası özet (bir sonraki mesajın başına eklenir)
    #[serde(default)]
    pub pending_summary: Option<String>,
    /// Son isteğin girdi token sayısı (devam edince sıkıştırma kararı için)
    #[serde(default)]
    pub input_tokens: u64,
}

fn dir() -> PathBuf {
    config::home_dir().join("sessions")
}

pub fn new_id() -> String {
    format!("{}-{}", chrono::Local::now().format("%Y%m%d-%H%M%S"), &uuid::Uuid::new_v4().simple().to_string()[..4])
}

pub fn save(s: &Saved) -> Result<()> {
    std::fs::create_dir_all(dir())?;
    let tmp = dir().join(format!("{}.json.tmp", s.id));
    std::fs::write(&tmp, serde_json::to_vec(s)?)?;
    std::fs::rename(&tmp, dir().join(format!("{}.json", s.id)))?;
    Ok(())
}

pub fn load(id: &str) -> Result<Saved> {
    // Tam id ya da benzersiz önek kabul edilir
    let path = dir().join(format!("{id}.json"));
    let path = if path.exists() {
        path
    } else {
        list()?.into_iter().find(|s| s.id.starts_with(id)).map(|s| dir().join(format!("{}.json", s.id)))
            .with_context(|| format!("'{id}' oturumu bulunamadı"))?
    };
    Ok(serde_json::from_slice(&std::fs::read(path)?)?)
}

/// En yeniden eskiye
pub fn list() -> Result<Vec<Saved>> {
    let mut out: Vec<Saved> = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir()) else { return Ok(out) };
    for e in rd.flatten() {
        if e.path().extension().and_then(|x| x.to_str()) != Some("json") {
            continue;
        }
        if let Ok(s) = serde_json::from_slice::<Saved>(&std::fs::read(e.path()).unwrap_or_default()) {
            out.push(s);
        }
    }
    out.sort_by(|a, b| b.updated.cmp(&a.updated));
    Ok(out)
}

fn trash_dir() -> PathBuf {
    dir().join("trash")
}

/// Sohbeti çöp kutusuna taşır (geri alınabilir). 30 günden eski çöpler temizlenir.
pub fn delete(id: &str) -> Result<()> {
    std::fs::create_dir_all(trash_dir())?;
    std::fs::rename(dir().join(format!("{id}.json")), trash_dir().join(format!("{id}.json"))).context("oturum silinemedi")?;
    if let Ok(rd) = std::fs::read_dir(trash_dir()) {
        for e in rd.flatten() {
            let old = e.metadata().and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).map(|d| d.as_secs() > 30 * 86400);
            if old == Some(true) {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
    Ok(())
}

/// Silinen sohbeti çöp kutusundan geri getirir
pub fn restore(id: &str) -> Result<()> {
    std::fs::rename(trash_dir().join(format!("{id}.json")), dir().join(format!("{id}.json"))).context("sohbet geri getirilemedi")
}

/// Çöp kutusundaki sohbetler: (id, başlık), en yeni silinen önce
pub fn trashed() -> Vec<(String, String)> {
    let mut out: Vec<(std::time::SystemTime, String, String)> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(trash_dir()) {
        for e in rd.flatten() {
            if let Ok(s) = serde_json::from_slice::<Saved>(&std::fs::read(e.path()).unwrap_or_default()) {
                let t = e.metadata().and_then(|m| m.modified()).unwrap_or(std::time::SystemTime::UNIX_EPOCH);
                out.push((t, s.id, s.title));
            }
        }
    }
    out.sort_by(|a, b| b.0.cmp(&a.0));
    out.into_iter().map(|(_, a, b)| (a, b)).collect()
}

/// Sohbet başlığını değiştirir (kayıtlı dosyada)
pub fn rename(id: &str, title: &str) -> Result<()> {
    let mut s = load(id)?;
    s.title = title.to_string();
    save(&s)
}

pub fn latest() -> Result<Saved> {
    list()?.into_iter().next().context("kayıtlı oturum yok")
}

/// Asistan mesajlarından thinking bloklarını çıkarır (devam ederken güvenli geçmiş)
pub fn strip_thinking(messages: &[Value]) -> Vec<Value> {
    messages
        .iter()
        .map(|m| {
            if m["role"] != "assistant" {
                return m.clone();
            }
            let Some(blocks) = m["content"].as_array() else { return m.clone() };
            let kept: Vec<Value> = blocks
                .iter()
                .filter(|b| !matches!(b["type"].as_str(), Some("thinking") | Some("redacted_thinking")))
                .cloned()
                .collect();
            let kept = if kept.is_empty() { vec![json!({"type": "text", "text": "(…)"})] } else { kept };
            json!({"role": "assistant", "content": kept})
        })
        .collect()
}

/// İlk kullanıcı mesajından kısa başlık
pub fn title_from(messages: &[Value]) -> String {
    let first = messages
        .iter()
        .find(|m| m["role"] == "user")
        .and_then(|m| m["content"].as_str().map(String::from))
        .unwrap_or_default();
    let first = first.rsplit(crate::brain::USER_MARK).next().unwrap_or(&first).to_string();
    let t: String = first.lines().next().unwrap_or("").chars().take(70).collect();
    if t.is_empty() { "(boş)".into() } else { t }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_only_thinking() {
        let msgs = vec![
            json!({"role": "user", "content": "selam"}),
            json!({"role": "assistant", "content": [
                {"type": "thinking", "thinking": "", "signature": "x"},
                {"type": "text", "text": "merhaba"},
                {"type": "tool_use", "id": "t1", "name": "glob", "input": {}}
            ]}),
        ];
        let out = strip_thinking(&msgs);
        let blocks = out[1]["content"].as_array().unwrap();
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0]["type"], "text");
        assert_eq!(blocks[1]["type"], "tool_use");
        assert_eq!(out[0], msgs[0]);
    }
}
