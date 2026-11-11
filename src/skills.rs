//! Skill sistemi — Anthropic Agent Skills biçimiyle uyumlu:
//!   <skills>/<ad>/SKILL.md  (YAML frontmatter: name, description) + ek dosyalar
//! Agent'ın system prompt'unda sadece ad+açıklama listelenir; agent
//! `use_skill` aracıyla tam talimatı gerektiğinde yükler (kademeli açılım).
//!
//! Hazır kütüphane: github.com/anthropics/skills (pdf, docx, xlsx, pptx,
//! frontend-design, mcp-builder, webapp-testing, ... ) — `skill browse` ile.

use crate::config;
use crate::tools::{Tool, ToolOutput};
use anyhow::{Context, Result};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub dir: PathBuf,
}

pub fn split_frontmatter(text: &str) -> (serde_yaml::Value, String) {
    let t = text.trim_start_matches('\u{feff}');
    if let Some(rest) = t.strip_prefix("---") {
        if let Some(end) = rest.find("\n---") {
            let yaml = &rest[..end];
            let body = rest[end + 4..].trim_start_matches(['-', '\r', '\n']).to_string();
            let meta = serde_yaml::from_str(yaml).unwrap_or(serde_yaml::Value::Null);
            return (meta, body);
        }
    }
    (serde_yaml::Value::Null, t.to_string())
}

pub fn skill_roots() -> Vec<PathBuf> {
    let mut roots = config::search_dirs("skills");
    roots.extend(crate::plugins::installed_dirs().into_iter().map(|d| d.join("skills")));
    roots
}

pub fn load_all() -> Vec<Skill> {
    let mut out: Vec<Skill> = Vec::new();
    for root in skill_roots() {
        let Ok(rd) = std::fs::read_dir(&root) else { continue };
        for e in rd.flatten() {
            let md = e.path().join("SKILL.md");
            let Ok(text) = std::fs::read_to_string(&md) else { continue };
            let (meta, _) = split_frontmatter(&text);
            let name = meta["name"].as_str().map(String::from)
                .unwrap_or_else(|| e.file_name().to_string_lossy().into());
            if out.iter().any(|s| s.name == name) {
                continue; // proje önceliklidir
            }
            out.push(Skill {
                name,
                description: meta["description"].as_str().unwrap_or("").into(),
                dir: e.path(),
            });
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// System prompt'a eklenecek skill kataloğu
pub fn catalog(filter: &[String]) -> String {
    let skills: Vec<Skill> = load_all()
        .into_iter()
        .filter(|s| filter.is_empty() || filter.iter().any(|f| f == "*" || f == &s.name))
        .collect();
    if skills.is_empty() {
        return String::new();
    }
    let mut s = String::from(
        "\n\n# Skills\nAşağıdaki skill'ler mevcut. Görev bir skill ile eşleşiyorsa işe başlamadan önce `use_skill` ile talimatını yükle:\n",
    );
    for sk in skills {
        s.push_str(&format!("- {}: {}\n", sk.name, sk.description));
    }
    s
}

pub struct UseSkill;

#[async_trait]
impl Tool for UseSkill {
    fn name(&self) -> String { "use_skill".into() }
    fn description(&self) -> String {
        "Bir skill'in tam talimatlarını (SKILL.md) yükler. Skill klasöründeki ek dosyalar read_file ile okunabilir.".into()
    }
    fn input_schema(&self) -> Value {
        json!({"type":"object","properties":{"name":{"type":"string"}},"required":["name"],"additionalProperties":false})
    }
    async fn call(&self, input: Value) -> ToolOutput {
        let name = input["name"].as_str().unwrap_or("");
        match load_all().into_iter().find(|s| s.name == name) {
            None => ToolOutput::Error(format!("'{name}' adında skill yok")),
            Some(s) => match std::fs::read_to_string(s.dir.join("SKILL.md")) {
                Ok(t) => ToolOutput::Text(format!("Skill klasörü: {}\n\n{}", s.dir.display(), t)),
                Err(e) => ToolOutput::Error(e.to_string()),
            },
        }
    }
}

// ------------------------------------------------------------------ browse / install (GitHub)

#[derive(Debug, Clone)]
pub struct RemoteSkill {
    pub name: String,
    /// "sahip/repo/yol/ad"
    pub source: String,
}

fn gh() -> reqwest::Client {
    reqwest::Client::builder().user_agent("rusty-agent").build().unwrap()
}

/// "sahip/repo/yol" -> GitHub contents API URL
fn contents_url(spec: &str) -> Result<String> {
    let mut parts = spec.splitn(3, '/');
    let owner = parts.next().context("sahip yok")?;
    let repo = parts.next().context("repo yok")?;
    let path = parts.next().unwrap_or("");
    Ok(format!("https://api.github.com/repos/{owner}/{repo}/contents/{path}"))
}

pub async fn browse(sources: &[String], query: &str) -> Result<Vec<RemoteSkill>> {
    let mut out = Vec::new();
    let q = query.to_lowercase();
    for src in sources {
        let v: Value = gh().get(contents_url(src)?).send().await?.error_for_status()?.json().await?;
        for item in v.as_array().cloned().unwrap_or_default() {
            if item["type"] != "dir" {
                continue;
            }
            let name = item["name"].as_str().unwrap_or("").to_string();
            if q.is_empty() || name.to_lowercase().contains(&q) {
                out.push(RemoteSkill { source: format!("{src}/{name}"), name });
            }
        }
    }
    Ok(out)
}

/// GitHub'dan bir skill klasörünü ~/.rustyagent/skills/<ad> içine indirir
pub async fn install(source: &str) -> Result<PathBuf> {
    let name = source.rsplit('/').next().context("geçersiz kaynak")?;
    let dest = config::home_dir().join("skills").join(name);
    download_dir(source, &dest).await?;
    Ok(dest)
}

pub fn download_dir<'a>(spec: &'a str, dest: &'a Path) -> futures::future::BoxFuture<'a, Result<()>> {
    Box::pin(async move {
        std::fs::create_dir_all(dest)?;
        let v: Value = gh().get(contents_url(spec)?).send().await?.error_for_status()?.json().await?;
        for item in v.as_array().cloned().unwrap_or_default() {
            let name = item["name"].as_str().unwrap_or("");
            match item["type"].as_str() {
                Some("file") => {
                    if let Some(url) = item["download_url"].as_str() {
                        let bytes = gh().get(url).send().await?.bytes().await?;
                        std::fs::write(dest.join(name), &bytes)?;
                    }
                }
                Some("dir") => download_dir(&format!("{spec}/{name}"), &dest.join(name)).await?,
                _ => {}
            }
        }
        Ok(())
    })
}

pub fn remove(name: &str) -> Result<()> {
    let dir = config::home_dir().join("skills").join(name);
    std::fs::remove_dir_all(&dir).with_context(|| format!("{} silinemedi", dir.display()))
}
