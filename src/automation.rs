//! Otomasyonlar: cron zamanlamasıyla agent'lara iş yaptırma.
//! ~/.rustyagent/automations.toml:
//! [[automation]]
//! name = "sabah-ozeti"
//! schedule = "0 0 9 * * *"      # sn dk saat gün ay haftanın-günü (yerel saat)
//! prompt = "Gelen kutumu özetle ve önemli olanları bildir"
//! agent = "main"                # opsiyonel
//! notify = true                 # sonucu Telegram'a gönder
//! enabled = true

use crate::agent::{self, Runtime, MAIN_AGENT};
use crate::config;
use anyhow::{Context, Result};
use chrono::{DateTime, Local};
use serde::{Deserialize, Serialize};
use std::str::FromStr;
use std::sync::Arc;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Automation {
    pub name: String,
    pub schedule: String,
    pub prompt: String,
    #[serde(default = "main_agent")]
    pub agent: String,
    #[serde(default = "yes")]
    pub notify: bool,
    #[serde(default = "yes")]
    pub enabled: bool,
}

fn main_agent() -> String { MAIN_AGENT.into() }
fn yes() -> bool { true }

#[derive(Debug, Default, Serialize, Deserialize)]
struct File {
    #[serde(default)]
    automation: Vec<Automation>,
}

fn path() -> std::path::PathBuf {
    config::home_dir().join("automations.toml")
}

pub fn load() -> Vec<Automation> {
    let mut all: Vec<Automation> = Vec::new();
    let mut files = vec![path()];
    files.extend(crate::plugins::installed_dirs().into_iter().map(|d| d.join("automations.toml")));
    for f in files {
        if let Ok(t) = std::fs::read_to_string(&f) {
            if let Ok(parsed) = toml::from_str::<File>(&t) {
                all.extend(parsed.automation);
            }
        }
    }
    all
}

pub fn add(a: Automation) -> Result<()> {
    cron::Schedule::from_str(&a.schedule).context("geçersiz cron ifadesi (6 alan: sn dk saat gün ay hg)")?;
    let mut f: File = std::fs::read_to_string(path()).ok().and_then(|t| toml::from_str(&t).ok()).unwrap_or_default();
    f.automation.retain(|x| x.name != a.name);
    f.automation.push(a);
    std::fs::create_dir_all(config::home_dir())?;
    std::fs::write(path(), toml::to_string_pretty(&f)?)?;
    Ok(())
}

pub fn remove(name: &str) -> Result<bool> {
    let mut f: File = std::fs::read_to_string(path()).ok().and_then(|t| toml::from_str(&t).ok()).unwrap_or_default();
    let before = f.automation.len();
    f.automation.retain(|x| x.name != name);
    std::fs::write(path(), toml::to_string_pretty(&f)?)?;
    Ok(f.automation.len() != before)
}

pub async fn run_once(rt: &Arc<Runtime>, a: &Automation) -> Result<String> {
    let mut session = if a.agent == MAIN_AGENT {
        rt.main_session(None)
    } else {
        let def = agent::load_agents().into_iter().find(|d| d.name == a.agent)
            .with_context(|| format!("agent '{}' yok", a.agent))?;
        rt.agent_session(&def, None)
    };
    session.send(&format!("[Otomasyon: {}]\n{}", a.name, a.prompt)).await
}

/// Zamanlayıcı döngüsü (serve modunda arka planda çalışır)
pub async fn scheduler(rt: Arc<Runtime>) {
    let mut last_check: DateTime<Local> = Local::now();
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(20)).await;
        let now = Local::now();
        for a in load().into_iter().filter(|a| a.enabled) {
            let Ok(sched) = cron::Schedule::from_str(&a.schedule) else { continue };
            let due = sched.after(&last_check).next().map(|t| t <= now).unwrap_or(false);
            if !due {
                continue;
            }
            let rt = rt.clone();
            tokio::spawn(async move {
                let result = run_once(&rt, &a).await;
                let text = match result {
                    Ok(t) => format!("Otomasyon: {}\n\n{}", a.name, t),
                    Err(e) => format!("Otomasyon {} başarısız: {e:#}", a.name),
                };
                if a.notify {
                    let _ = rt.notifier.notify(&text, false).await;
                } else {
                    println!("{text}");
                }
            });
        }
        last_check = now;
    }
}
