//! Plugin sistemi — Claude Code plugin biçimiyle uyumlu.
//! Bir plugin klasörü şunları içerebilir:
//!   agents/*.md   skills/<ad>/SKILL.md   .mcp.json   automations.toml
//! Pazar (marketplace): GitHub reposunda `.claude-plugin/marketplace.json`.

use crate::config;
use anyhow::{bail, Context, Result};
use serde_json::Value;
use std::path::PathBuf;

pub fn plugins_root() -> PathBuf {
    config::home_dir().join("plugins")
}

pub fn installed_dirs() -> Vec<PathBuf> {
    std::fs::read_dir(plugins_root())
        .map(|rd| rd.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect())
        .unwrap_or_default()
}

#[derive(Debug, Clone)]
pub struct MarketPlugin {
    pub name: String,
    pub description: String,
    pub marketplace: String,
    /// GitHub kaynak spec'i: "sahip/repo/yol"
    pub source: String,
}

fn gh() -> reqwest::Client {
    reqwest::Client::builder().user_agent("rusty-agent").build().unwrap()
}

async fn fetch_marketplace(repo: &str) -> Result<Vec<MarketPlugin>> {
    let url = format!("https://raw.githubusercontent.com/{repo}/HEAD/.claude-plugin/marketplace.json");
    let v: Value = gh().get(&url).send().await?.error_for_status()
        .with_context(|| format!("{repo} pazarı okunamadı"))?.json().await?;
    let mut out = Vec::new();
    for p in v["plugins"].as_array().cloned().unwrap_or_default() {
        let source = match &p["source"] {
            Value::String(rel) => format!("{repo}/{}", rel.trim_start_matches("./")),
            Value::Object(o) => match (o.get("source").and_then(|s| s.as_str()), o.get("repo").and_then(|s| s.as_str())) {
                (Some("github"), Some(r)) => r.to_string(),
                _ => continue, // desteklenmeyen kaynak türü (url/git vb.)
            },
            _ => continue,
        };
        out.push(MarketPlugin {
            name: p["name"].as_str().unwrap_or("?").into(),
            description: p["description"].as_str().unwrap_or("").into(),
            marketplace: repo.into(),
            source,
        });
    }
    Ok(out)
}

pub async fn browse(marketplaces: &[String], query: &str) -> Result<Vec<MarketPlugin>> {
    let q = query.to_lowercase();
    let mut all = Vec::new();
    let mut errors = Vec::new();
    for m in marketplaces {
        match fetch_marketplace(m).await {
            Ok(list) => all.extend(list),
            Err(e) => errors.push(format!("{e:#}")),
        }
    }
    if all.is_empty() && !errors.is_empty() {
        bail!(errors.join("\n"));
    }
    all.retain(|p| q.is_empty() || p.name.to_lowercase().contains(&q) || p.description.to_lowercase().contains(&q));
    Ok(all)
}

pub async fn install(marketplaces: &[String], name: &str) -> Result<PathBuf> {
    let list = browse(marketplaces, "").await?;
    let p = list.into_iter().find(|p| p.name == name).with_context(|| format!("'{name}' pazarda yok"))?;
    let dest = plugins_root().join(&p.name);
    crate::skills::download_dir(&p.source, &dest).await?;
    Ok(dest)
}

pub fn remove(name: &str) -> Result<()> {
    std::fs::remove_dir_all(plugins_root().join(name)).context("plugin silinemedi")
}
