//! Second brain — kalıcı hafıza.
//!
//! Notlar ~/.rustyagent/brain/<kimlik>.md dosyalarında, Obsidian uyumlu Markdown olarak durur:
//!   ---
//!   title: Kullanıcının kahve tercihi
//!   kind: preference          # fact | preference | project | summary | note
//!   tags: [profil, yemek]
//!   created: …  updated: …  source: <oturum id>
//!   ---
//!   Sütsüz filtre kahve sever. İlgili: [[Sabah rutini]]
//!
//! - Agent'lar `memory_save` / `memory_search` / `memory_read` / `history_search` araçlarını kullanır.
//! - Yeni sohbetin ilk mesajına profil notları ve mesajla ilgili notlar otomatik eklenir.
//! - Biten sohbetler ve sıkıştırma özetleri otomatik olarak "summary" notu olur.
//! - Arama Türkçe karakter ve büyük/küçük harf duyarsızdır (BM25).

use crate::config;
use crate::tools::{Tool, ToolOutput, ToolSet};
use anyhow::{Context, Result};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

/// Kullanıcı mesajının başına eklenen bağlamın bittiği yer (arayüz bu işaretten sonrasını gösterir)
pub const USER_MARK: &str = "[Kullanıcının mesajı]\n";

#[derive(Debug, Clone, Default)]
pub struct Note {
    pub id: String,
    pub title: String,
    pub kind: String,
    pub tags: Vec<String>,
    pub created: String,
    pub updated: String,
    pub source: String,
    pub body: String,
}

pub fn dir() -> PathBuf {
    config::home_dir().join("brain")
}

/// Türkçe karakterleri ASCII'ye katlar ve küçültür (arama için)
pub fn fold(s: &str) -> String {
    s.chars()
        .flat_map(|c| c.to_lowercase())
        .map(|c| match c {
            'ı' | 'i' | 'î' => 'i',
            'ğ' => 'g',
            'ü' | 'û' => 'u',
            'ş' => 's',
            'ö' => 'o',
            'ç' => 'c',
            'â' => 'a',
            '\u{307}' => '\0', // İ küçültülünce gelen birleşik nokta
            c => c,
        })
        .filter(|c| *c != '\0')
        .collect()
}

fn tokens(s: &str) -> Vec<String> {
    fold(s)
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.chars().count() >= 2)
        // Basit kök: Türkçe çekim eklerine karşı ilk 6 harf
        .map(|w| w.chars().take(6).collect())
        .collect()
}

pub fn slug(title: &str) -> String {
    let s: String = fold(title)
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let s = s.split('-').filter(|x| !x.is_empty()).collect::<Vec<_>>().join("-");
    let s: String = s.chars().take(60).collect();
    if s.is_empty() { format!("not-{}", &uuid::Uuid::new_v4().simple().to_string()[..6]) } else { s }
}

fn parse(id: &str, text: &str) -> Note {
    let (m, body) = crate::skills::split_frontmatter(text);
    let tags = match &m["tags"] {
        serde_yaml::Value::Sequence(s) => s.iter().filter_map(|x| x.as_str().map(String::from)).collect(),
        serde_yaml::Value::String(s) => s.split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect(),
        _ => vec![],
    };
    Note {
        id: id.to_string(),
        title: m["title"].as_str().unwrap_or(id).to_string(),
        kind: m["kind"].as_str().unwrap_or("note").to_string(),
        tags,
        created: m["created"].as_str().unwrap_or("").to_string(),
        updated: m["updated"].as_str().unwrap_or("").to_string(),
        source: m["source"].as_str().unwrap_or("").to_string(),
        body: body.trim().to_string(),
    }
}

pub fn list() -> Vec<Note> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir()) else { return out };
    for e in rd.flatten() {
        let p = e.path();
        if p.extension().and_then(|x| x.to_str()) != Some("md") {
            continue;
        }
        if let Ok(t) = std::fs::read_to_string(&p) {
            out.push(parse(&p.file_stem().unwrap().to_string_lossy(), &t));
        }
    }
    out.sort_by(|a, b| b.updated.cmp(&a.updated));
    out
}

pub fn get(id_or_title: &str) -> Option<Note> {
    let p = dir().join(format!("{id_or_title}.md"));
    if let Ok(t) = std::fs::read_to_string(&p) {
        return Some(parse(id_or_title, &t));
    }
    let f = fold(id_or_title);
    list().into_iter().find(|n| fold(&n.title) == f || n.id == slug(id_or_title))
}

/// Notu kaydeder (aynı başlıklı not varsa günceller). Kimliği döndürür.
pub fn save(mut n: Note) -> Result<String> {
    std::fs::create_dir_all(dir())?;
    let now = chrono::Local::now().to_rfc3339();
    if n.id.is_empty() {
        n.id = get(&n.title).map(|e| e.id).unwrap_or_else(|| slug(&n.title));
    }
    if let Some(old) = get(&n.id) {
        if n.created.is_empty() {
            n.created = old.created;
        }
    }
    if n.created.is_empty() {
        n.created = now.clone();
    }
    n.updated = now;
    if n.kind.is_empty() {
        n.kind = "note".into();
    }
    let mut fm = serde_yaml::Mapping::new();
    let mut put = |k: &str, v: serde_yaml::Value| {
        fm.insert(k.into(), v);
    };
    put("title", n.title.clone().into());
    put("kind", n.kind.clone().into());
    put("tags", serde_yaml::to_value(&n.tags)?);
    put("created", n.created.clone().into());
    put("updated", n.updated.clone().into());
    if !n.source.is_empty() {
        put("source", n.source.clone().into());
    }
    std::fs::write(dir().join(format!("{}.md", n.id)), format!("---\n{}---\n\n{}\n", serde_yaml::to_string(&fm)?, n.body.trim()))?;
    Ok(n.id)
}

pub fn delete(id: &str) -> Result<()> {
    std::fs::remove_file(dir().join(format!("{id}.md"))).context("not silinemedi")
}

/// [[Başlık]] bağlantıları
pub fn links(body: &str) -> Vec<String> {
    let re = regex::Regex::new(r"\[\[([^\]|#]+)").unwrap();
    re.captures_iter(body).map(|c| c[1].trim().to_string()).collect()
}

/// Bu nota bağlantı veren notlar
pub fn backlinks(title: &str) -> Vec<Note> {
    let f = fold(title);
    list().into_iter().filter(|n| links(&n.body).iter().any(|l| fold(l) == f)).collect()
}

/// BM25 arama: başlık ve etiketler daha ağırlıklı
pub fn search(query: &str, k: usize) -> Vec<(f32, Note)> {
    let q = tokens(query);
    if q.is_empty() {
        return vec![];
    }
    let notes = list();
    let docs: Vec<Vec<String>> = notes
        .iter()
        .map(|n| {
            let mut t = tokens(&n.title);
            t.extend(tokens(&n.title));
            t.extend(tokens(&n.title));
            for tag in &n.tags {
                t.extend(tokens(tag));
                t.extend(tokens(tag));
            }
            t.extend(tokens(&n.body));
            t
        })
        .collect();
    let n_docs = docs.len().max(1) as f32;
    let avg = docs.iter().map(|d| d.len()).sum::<usize>() as f32 / n_docs;
    let mut df: HashMap<&str, usize> = HashMap::new();
    for d in &docs {
        let mut seen = std::collections::HashSet::new();
        for w in d {
            if seen.insert(w.as_str()) {
                *df.entry(w.as_str()).or_default() += 1;
            }
        }
    }
    let (k1, b) = (1.4f32, 0.75f32);
    let mut scored: Vec<(f32, Note)> = docs
        .iter()
        .zip(notes)
        .filter_map(|(d, n)| {
            let len = d.len() as f32;
            let mut s = 0.0;
            for term in &q {
                let tf = d.iter().filter(|w| *w == term).count() as f32;
                if tf == 0.0 {
                    continue;
                }
                let dfi = *df.get(term.as_str()).unwrap_or(&0) as f32;
                let idf = ((n_docs - dfi + 0.5) / (dfi + 0.5) + 1.0).ln();
                s += idf * tf * (k1 + 1.0) / (tf + k1 * (1.0 - b + b * len / avg.max(1.0)));
            }
            (s > 0.0).then_some((s, n))
        })
        .collect();
    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap());
    scored.truncate(k);
    scored
}

fn snippet(body: &str, n: usize) -> String {
    let one = body.replace('\n', " ");
    if one.chars().count() > n { format!("{}…", one.chars().take(n).collect::<String>()) } else { one }
}

/// Yeni sohbetin ilk mesajına eklenecek hafıza bağlamı (yoksa boş)
pub fn context_for(user_text: &str) -> String {
    let mut lines = Vec::new();
    let mut used = std::collections::HashSet::new();
    // Profil notları her zaman
    for n in list().into_iter().filter(|n| n.kind == "preference" || n.tags.iter().any(|t| fold(t) == "profil")).take(12) {
        lines.push(format!("- [{}] {}: {}", n.kind, n.title, snippet(&n.body, 300)));
        used.insert(n.id);
    }
    for (_, n) in search(user_text, 6) {
        if used.insert(n.id.clone()) {
            lines.push(format!("- [{}] {}: {}", n.kind, n.title, snippet(&n.body, 400)));
        }
    }
    if lines.is_empty() {
        return String::new();
    }
    format!(
        "[Hafızandan ilgili notlar — yalnızca konuyla ilgiliyse kullan; gerekirse memory_read ile tamamını oku]\n{}\n\n",
        lines.join("\n")
    )
}

/// Geçmiş sohbetlerde tam metin arama: (oturum id, başlık, tarih, alıntı)
pub fn search_history(query: &str, k: usize) -> Vec<(String, String, String, String)> {
    let q: Vec<String> = tokens(query);
    if q.is_empty() {
        return vec![];
    }
    let mut out = Vec::new();
    for s in crate::sessions::list().unwrap_or_default() {
        let mut best: Option<(usize, String)> = None;
        for m in &s.messages {
            let text = match &m["content"] {
                Value::String(t) => t.clone(),
                Value::Array(a) => crate::llm::text_of(a),
                _ => continue,
            };
            let ft = fold(&text);
            let hits = q.iter().filter(|w| ft.contains(w.as_str())).count();
            if hits > 0 && best.as_ref().map(|b| hits > b.0).unwrap_or(true) {
                let shown = text.rsplit(USER_MARK).next().unwrap_or(&text).to_string();
                best = Some((hits, snippet(&shown, 220)));
            }
        }
        if let Some((hits, snip)) = best {
            out.push((hits, s.id.clone(), s.title.clone(), s.updated.clone(), snip));
        }
    }
    out.sort_by(|a, b| b.0.cmp(&a.0).then(b.3.cmp(&a.3)));
    out.into_iter().take(k).map(|(_, a, b, c, d)| (a, b, c, d)).collect()
}

// ------------------------------------------------------------------ araçlar

pub fn register(ts: &mut ToolSet) {
    ts.add(Arc::new(MemorySave));
    ts.add(Arc::new(MemorySearch));
    ts.add(Arc::new(MemoryRead));
    ts.add(Arc::new(HistorySearch));
}

struct MemorySave;

#[async_trait]
impl Tool for MemorySave {
    fn name(&self) -> String {
        "memory_save".into()
    }
    fn description(&self) -> String {
        "Kalıcı hafızaya (second brain) not kaydeder veya aynı başlıklı notu günceller. Kullanıcı hakkında kalıcı bilgileri \
         (tercihler, kişisel bilgiler, projeler, kararlar, önemli sonuçlar) öğrendiğinde kendiliğinden kaydet. \
         Geçici şeyleri kaydetme. İlgili notlara [[Başlık]] ile bağlantı ver. kind: fact | preference | project | note."
            .into()
    }
    fn input_schema(&self) -> Value {
        json!({"type": "object", "properties": {
            "title": {"type": "string", "description": "Kısa, aranabilir başlık"},
            "content": {"type": "string", "description": "Markdown içerik"},
            "kind": {"type": "string", "enum": ["fact", "preference", "project", "note"]},
            "tags": {"type": "array", "items": {"type": "string"}, "description": "Kullanıcıyla ilgili kalıcı bilgiler için 'profil' etiketi ekle"}
        }, "required": ["title", "content"], "additionalProperties": false})
    }
    async fn call(&self, input: Value) -> ToolOutput {
        let n = Note {
            title: input["title"].as_str().unwrap_or("").to_string(),
            body: input["content"].as_str().unwrap_or("").to_string(),
            kind: input["kind"].as_str().unwrap_or("note").to_string(),
            tags: input["tags"].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default(),
            ..Default::default()
        };
        if n.title.trim().is_empty() {
            return ToolOutput::Error("title gerekli".into());
        }
        match save(n) {
            Ok(id) => ToolOutput::Text(format!("Hafızaya kaydedildi: {id}")),
            Err(e) => ToolOutput::Error(e.to_string()),
        }
    }
}

struct MemorySearch;

#[async_trait]
impl Tool for MemorySearch {
    fn name(&self) -> String {
        "memory_search".into()
    }
    fn description(&self) -> String {
        "Kalıcı hafızada (notlar) arama yapar. Kullanıcı geçmişte konuşulan bir şeye, tercihlerine veya projelerine atıf yaptığında önce burada ara.".into()
    }
    fn input_schema(&self) -> Value {
        json!({"type": "object", "properties": {"query": {"type": "string"}}, "required": ["query"], "additionalProperties": false})
    }
    async fn call(&self, input: Value) -> ToolOutput {
        let r = search(input["query"].as_str().unwrap_or(""), 8);
        if r.is_empty() {
            return ToolOutput::Text("Eşleşen not yok. Geçmiş sohbetler için history_search dene.".into());
        }
        ToolOutput::Text(
            r.iter()
                .map(|(s, n)| format!("- {} (id: {}, tür: {}, skor {:.1}): {}", n.title, n.id, n.kind, s, snippet(&n.body, 300)))
                .collect::<Vec<_>>()
                .join("\n"),
        )
    }
}

struct MemoryRead;

#[async_trait]
impl Tool for MemoryRead {
    fn name(&self) -> String {
        "memory_read".into()
    }
    fn description(&self) -> String {
        "Hafızadaki bir notun tamamını okur (id veya başlık).".into()
    }
    fn input_schema(&self) -> Value {
        json!({"type": "object", "properties": {"id": {"type": "string"}}, "required": ["id"], "additionalProperties": false})
    }
    async fn call(&self, input: Value) -> ToolOutput {
        match get(input["id"].as_str().unwrap_or("")) {
            Some(n) => {
                let bl: Vec<String> = backlinks(&n.title).into_iter().map(|b| b.title).collect();
                ToolOutput::Text(format!(
                    "# {}\ntür: {} · etiketler: {} · güncellendi: {}\n\n{}\n\nBu nota bağlantı verenler: {}",
                    n.title,
                    n.kind,
                    n.tags.join(", "),
                    n.updated,
                    n.body,
                    if bl.is_empty() { "-".into() } else { bl.join(", ") }
                ))
            }
            None => ToolOutput::Error("not bulunamadı".into()),
        }
    }
}

struct HistorySearch;

#[async_trait]
impl Tool for HistorySearch {
    fn name(&self) -> String {
        "history_search".into()
    }
    fn description(&self) -> String {
        "Geçmiş sohbetlerin tam metninde arama yapar (\"geçen hafta ne konuşmuştuk\", \"hangi siteyi önermiştin\" gibi sorular için).".into()
    }
    fn input_schema(&self) -> Value {
        json!({"type": "object", "properties": {"query": {"type": "string"}}, "required": ["query"], "additionalProperties": false})
    }
    async fn call(&self, input: Value) -> ToolOutput {
        let r = search_history(input["query"].as_str().unwrap_or(""), 8);
        if r.is_empty() {
            return ToolOutput::Text("Geçmiş sohbetlerde eşleşme yok.".into());
        }
        ToolOutput::Text(
            r.iter()
                .map(|(id, title, date, snip)| format!("- {} — \"{}\" ({}): {}", date.get(..10).unwrap_or(date), title, id, snip))
                .collect::<Vec<_>>()
                .join("\n"),
        )
    }
}

// ------------------------------------------------------------------ otomatik özet

const SUMMARY_PROMPT: &str = "[Sistem: hafıza kaydı] Bu sohbeti kalıcı hafızaya kaydedeceğiz. Şu biçimde yaz, başka hiçbir şey yazma:\n\
BAŞLIK: <sohbetin konusunu anlatan kısa başlık>\n\
ÖZET:\n<3-8 maddelik özet: istenenler, yapılanlar, sonuçlar, kararlar, açık kalan işler; önemli dosya yolları/URL'ler dahil>\n\
KALICI BİLGİLER:\n<kullanıcı hakkında öğrenilen ve ileride işe yarayacak kalıcı bilgiler (tercih, kişisel bilgi, proje); yoksa 'yok'>\n\
Araç kullanma.";

/// Biten bir sohbeti özetleyip hafızaya yazar (arka planda çalıştırılır)
pub async fn summarize_conversation(
    llm: crate::llm::Client,
    model: String,
    system: String,
    tools: Vec<Value>,
    mut messages: Vec<Value>,
    session_id: String,
) -> Result<Option<String>> {
    let user_turns = messages.iter().filter(|m| m["role"] == "user" && m["content"].is_string()).count();
    if user_turns < 2 || messages.last().map(|m| m["role"] != "assistant").unwrap_or(true) {
        return Ok(None); // çok kısa veya tur ortası
    }
    messages.push(json!({"role": "user", "content": SUMMARY_PROMPT}));
    let r = llm
        .stream(
            crate::llm::Request {
                model: &model,
                effort: "low",
                max_tokens: 4000,
                system: &system,
                tools: &tools,
                messages: &messages,
                tool_choice: Some("none"),
            },
            None,
        )
        .await?;
    let text = crate::llm::text_of(&r.content);
    let title = text.lines().find_map(|l| l.trim().strip_prefix("BAŞLIK:")).map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    let body = text.split_once('\n').map(|(_, rest)| rest.trim().to_string()).unwrap_or_default();
    // Model istenen biçimde yanıt vermediyse boş/bozuk not kaydetme
    let (Some(title), true) = (title, body.chars().count() >= 40) else { return Ok(None) };
    let date = chrono::Local::now().format("%Y-%m-%d").to_string();
    let id = save(Note {
        title: format!("{date} — {title}"),
        kind: "summary".into(),
        tags: vec!["sohbet".into()],
        source: session_id,
        body,
        ..Default::default()
    })?;
    Ok(Some(id))
}

/// Sıkıştırma özetini hafızaya yazar
pub fn save_compaction(summary: &str, session_id: &str, title: &str) {
    let date = chrono::Local::now().format("%Y-%m-%d").to_string();
    let _ = save(Note {
        title: format!("{date} — {title} (ara özet)"),
        kind: "summary".into(),
        tags: vec!["sohbet".into()],
        source: session_id.to_string(),
        body: summary.to_string(),
        ..Default::default()
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folding_and_tokens() {
        assert_eq!(fold("İSTANBUL Öğrenci Çağrı"), "istanbul ogrenci cagri");
        assert_eq!(tokens("Öğrencilerin kahveleri"), vec!["ogrenc", "kahvel"]);
        assert_eq!(slug("Kahve tercihi: sütsüz!"), "kahve-tercihi-sutsuz");
        assert_eq!(links("bkz. [[Sabah rutini]] ve [[Proje X|x]]"), vec!["Sabah rutini", "Proje X"]);
    }
}
