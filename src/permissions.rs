//! İzin / onay sistemi.
//!
//! Her araç çağrısı çalışmadan önce risk seviyesine göre sınıflanır:
//!   Safe   — okuma, ekran görüntüsü, gezinme…            → sorulmaz
//!   Write  — dosya yazma, shell, mouse/klavye, form…      → moda göre sorulur
//!   Danger — silme, ödeme/satın alma, gönderme, zorla push… → (yolo ve allow kuralı dışında) her seferinde sorulur
//!
//! Modlar (config.toml [permissions] mode): ask | auto | yolo | readonly
//! Kurallar: allow / deny listeleri; desen biçimi: "araç" veya "araç(özne*)"
//!   ör. "shell(git status*)", "write_file(C:/proje/*)", "browser", "mcp__github__*"
//!
//! Onay kanalları: terminal arayüzü, Telegram (satır içi butonlar), `run` komutunda klavye.
//! Kanal yoksa (otomasyon) onay Telegram'dan istenir; o da yoksa işlem reddedilir.

use crate::config::{Config, PermissionMode, Permissions};
use crate::tools::ToolCtx;
use anyhow::Result;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use tokio::sync::oneshot;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Risk {
    Safe,
    Write,
    Danger,
}

impl Risk {
    pub fn label(&self) -> &'static str {
        match self {
            Risk::Safe => "güvenli",
            Risk::Write => "değişiklik yapar",
            Risk::Danger => "TEHLİKELİ",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Assessment {
    pub risk: Risk,
    /// İnsan için kısa açıklama
    pub summary: String,
    /// Kural eşleştirme öznesi: "shell(git push --force)"
    pub subject: String,
    /// "Bu oturum boyunca izin ver" anahtarı
    pub grant_key: String,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Decision {
    Allow,
    /// Bu çalıştırma boyunca aynı türü sorma
    AllowSession,
    /// Kalıcı kural olarak kaydet
    AllowAlways,
    Deny,
}

#[derive(Debug, Clone)]
pub struct ApprovalRequest {
    pub id: String,
    pub agent: String,
    pub tool: String,
    pub summary: String,
    pub risk: Risk,
    pub grant_key: String,
}

/// Arayüze gönderilen onay isteği; yanıt `respond` ile verilir
#[derive(Clone)]
pub struct ApprovalHandle {
    pub req: ApprovalRequest,
    tx: Arc<Mutex<Option<oneshot::Sender<Decision>>>>,
}

impl std::fmt::Debug for ApprovalHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ApprovalHandle({})", self.req.summary)
    }
}

impl ApprovalHandle {
    pub fn new(req: ApprovalRequest) -> (Self, oneshot::Receiver<Decision>) {
        let (tx, rx) = oneshot::channel();
        (Self { req, tx: Arc::new(Mutex::new(Some(tx))) }, rx)
    }
    pub fn respond(&self, d: Decision) {
        if let Some(tx) = self.tx.lock().unwrap().take() {
            let _ = tx.send(d);
        }
    }
}

// ------------------------------------------------------------------ desen eşleştirme

fn glob_match(pattern: &str, text: &str) -> bool {
    let re = format!("(?is)^{}$", regex::escape(pattern).replace("\\*", ".*"));
    regex::Regex::new(&re).map(|r| r.is_match(text)).unwrap_or(false)
}

fn rule_matches(rule: &str, tool: &str, subject: &str) -> bool {
    let rule = rule.trim();
    rule == tool || glob_match(rule, tool) || glob_match(rule, subject)
}

// ------------------------------------------------------------------ risk sınıflandırma

fn danger_shell() -> &'static regex::Regex {
    static R: OnceLock<regex::Regex> = OnceLock::new();
    R.get_or_init(|| {
        regex::Regex::new(
            r"(?ix)
            \brm\s+(-\w*\s+)*-\w*[rf] | \brmdir\b | \bdel\s | \berase\b | remove-item | \bformat(-volume)?\s | mkfs | \bdd\s+if=
            | shutdown | restart-computer | stop-computer | \breboot\b | \bkillall\b | stop-process | taskkill
            | git\s+push\b.*(--force|-f\b) | git\s+reset\s+--hard | git\s+clean\s+-\w*f | git\s+branch\s+-D
            | drop\s+(table|database|schema) | truncate\s+table | delete\s+from
            | reg\s+delete | remove-itemproperty | set-executionpolicy | \bchmod\s+-R | \bchown\s+-R
            | (curl|wget|iwr|invoke-webrequest)[^|]*\|\s*(sh|bash|iex|invoke-expression|powershell)
            | invoke-expression | \biex\b | diskpart | bcdedit | cipher\s+/w | vssadmin | net\s+user | \bsudo\b
            | npm\s+publish | cargo\s+publish | \bmv\s+.*\s+/dev/null",
        )
        .unwrap()
    })
}

fn safe_shell() -> &'static regex::Regex {
    static R: OnceLock<regex::Regex> = OnceLock::new();
    R.get_or_init(|| {
        regex::Regex::new(
            r"(?ix)^\s*(
              ls|dir|pwd|echo|cat|type|head|tail|wc|which|where|whoami|hostname|date|tree
            | get-childitem|gci|get-content|gc|get-location|get-date|get-process|get-service|get-command|select-string|findstr|test-path|resolve-path
            | git\s+(status|log|diff|show|branch|remote|rev-parse|blame)
            | cargo\s+(check|build|test|tree|metadata|--version|clippy)
            | (rustc|node|npm|python|python3|py|go|java|dotnet)\s+--?version
            )\b[^;&|>`]*$",
        )
        .unwrap()
    })
}

fn danger_label() -> &'static regex::Regex {
    static R: OnceLock<regex::Regex> = OnceLock::new();
    R.get_or_init(|| {
        regex::Regex::new(
            r"(?ix)
              satın\s*al | sepete\s*ekle | ödeme | \böde\b | siparişi?\s*(ver|tamamla|onayla) | \bbuy\b | purchase | checkout | \bpay\b
            | place\s+order | order\s+now | \bgönder\b | \bsend\b | paylaş | \bshare\b | publish | yayınla
            | \bsil\b | silme | delete | \bremove\b | kaldır | hesab\w*\s*kapat | transfer | havale | \beft\b
            | \bonayla\b | confirm | abone\s*ol | subscribe | iptal\s*et",
        )
        .unwrap()
    })
}

fn sensitive_path(p: &str) -> bool {
    let l = p.to_lowercase().replace('\\', "/");
    ["/.ssh/", "/etc/", "c:/windows/", "/system32/", "/.aws/", "/.gnupg/", "/.rustyagent/config.toml", "/.env"]
        .iter()
        .any(|s| l.contains(s))
        || l.ends_with(".env")
}

fn short(s: &str, n: usize) -> String {
    let s = s.replace('\n', " ⏎ ");
    if s.chars().count() > n { format!("{}…", s.chars().take(n).collect::<String>()) } else { s }
}

/// Bir araç çağrısının riskini değerlendirir
pub async fn assess(tool: &str, input: &Value, ctx: &ToolCtx) -> Assessment {
    let s = |k: &str| input[k].as_str().unwrap_or("").to_string();
    let mk = |risk, summary: String, subject: String, grant_key: String| Assessment { risk, summary, subject, grant_key };
    match tool {
        "read_file" | "glob" | "grep" | "web_fetch" | "use_skill" | "web_search" | "delegate" | "notify_user"
        | "memory_save" | "memory_search" | "memory_read" | "history_search" => {
            mk(Risk::Safe, format!("{tool}"), tool.into(), tool.into())
        }
        "write_file" | "edit_file" => {
            let path = s("path");
            let risk = if sensitive_path(&path) { Risk::Danger } else { Risk::Write };
            let verb = if tool == "write_file" { "dosya yaz" } else { "dosya düzenle" };
            mk(risk, format!("{verb}: {path}"), format!("{tool}({path})"), tool.into())
        }
        "create_agent" => mk(Risk::Write, format!("yeni agent oluştur: {}", s("name")), tool.into(), tool.into()),
        "shell" => {
            let cmd = s("command");
            let first = cmd.split_whitespace().next().unwrap_or("").to_string();
            let risk = if danger_shell().is_match(&cmd) {
                Risk::Danger
            } else if safe_shell().is_match(&cmd) {
                Risk::Safe
            } else {
                Risk::Write
            };
            mk(risk, format!("komut çalıştır: {}", short(&cmd, 160)), format!("shell({cmd})"), format!("shell({first}*)"))
        }
        "computer" => {
            let action = s("action");
            let risk = match action.as_str() {
                "screenshot" | "cursor_position" | "wait" => Risk::Safe,
                "key" if s("text").to_lowercase().contains("alt+f4") || s("text").to_lowercase().contains("shift+delete") => Risk::Danger,
                _ => Risk::Write,
            };
            let detail = match action.as_str() {
                "type" => format!(" \"{}\"", short(&s("text"), 60)),
                "key" => format!(" {}", s("text")),
                _ => input.get("x").map(|_| format!(" ({}, {})", input["x"], input["y"])).unwrap_or_default(),
            };
            mk(risk, format!("masaüstü: {action}{detail}"), format!("computer({action})"), "computer".into())
        }
        "browser" => {
            let action = s("action");
            let label = match input["index"].as_i64() {
                Some(i) if matches!(action.as_str(), "click" | "double_click" | "right_click" | "type" | "select" | "key") => {
                    crate::tools::browser::element_label(&ctx.session, i).await.unwrap_or_default()
                }
                _ => String::new(),
            };
            let risk = match action.as_str() {
                "navigate" | "back" | "snapshot" | "screenshot" | "scroll" | "hover" | "close" => Risk::Safe,
                _ if danger_label().is_match(&label) => Risk::Danger,
                // Ödeme/gönderme formunda Enter da tehlikeli olabilir; etiket görünmüyorsa Write
                _ => Risk::Write,
            };
            let what = match action.as_str() {
                "navigate" => format!(" {}", s("url")),
                "type" => format!(" \"{}\"", short(&s("text"), 60)),
                "key" => format!(" {}", s("text")),
                "evaluate" => format!(" JS: {}", short(&s("text"), 80)),
                _ => String::new(),
            };
            let on = if label.is_empty() { String::new() } else { format!(" → \"{}\"", short(&label, 60)) };
            mk(risk, format!("tarayıcı: {action}{what}{on}"), format!("browser({action} {label})"), "browser".into())
        }
        t if t.starts_with("mcp__") => {
            mk(Risk::Write, format!("{t} {}", short(&input.to_string(), 120)), format!("{t}({input})"), t.into())
        }
        t => mk(Risk::Write, format!("{t} {}", short(&input.to_string(), 120)), t.into(), t.into()),
    }
}

pub enum Verdict {
    Allow,
    Deny(String),
    Ask,
}

pub fn evaluate(perms: &Permissions, grants: &std::collections::HashSet<String>, tool: &str, a: &Assessment) -> Verdict {
    if let Some(r) = perms.deny.iter().find(|r| rule_matches(r, tool, &a.subject)) {
        return Verdict::Deny(format!("izin kuralı yasaklıyor ({r})"));
    }
    if perms.allow.iter().any(|r| rule_matches(r, tool, &a.subject)) {
        return Verdict::Allow;
    }
    match (perms.mode, a.risk) {
        (PermissionMode::Yolo, _) | (_, Risk::Safe) => Verdict::Allow,
        (PermissionMode::Readonly, _) => Verdict::Deny("salt okunur mod: değişiklik yapan işlemler kapalı".into()),
        (_, Risk::Danger) => Verdict::Ask,
        (PermissionMode::Auto, Risk::Write) => Verdict::Allow,
        (PermissionMode::Ask, Risk::Write) => {
            if grants.contains(&a.grant_key) { Verdict::Allow } else { Verdict::Ask }
        }
    }
}

/// "Her zaman izin ver" kararını kalıcı kural olarak kaydeder
pub fn persist_allow(rule: &str) -> Result<()> {
    let mut cfg = Config::load()?;
    if !cfg.permissions.allow.iter().any(|r| r == rule) {
        cfg.permissions.allow.push(rule.to_string());
        cfg.save()?;
    }
    Ok(())
}

pub fn set_mode(mode: PermissionMode) -> Result<()> {
    let mut cfg = Config::load()?;
    cfg.permissions.mode = mode;
    cfg.save()
}

pub fn parse_mode(s: &str) -> Option<PermissionMode> {
    Some(match s {
        "ask" | "sor" => PermissionMode::Ask,
        "auto" | "otomatik" => PermissionMode::Auto,
        "yolo" => PermissionMode::Yolo,
        "readonly" | "salt-okunur" | "read-only" => PermissionMode::Readonly,
        _ => return None,
    })
}

// ------------------------------------------------------------------ Telegram üzerinden uzaktan onay

fn pending() -> &'static Mutex<HashMap<String, oneshot::Sender<Decision>>> {
    static P: OnceLock<Mutex<HashMap<String, oneshot::Sender<Decision>>>> = OnceLock::new();
    P.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Telegram buton yanıtını bekleyen isteğe iletir
pub fn resolve(id: &str, d: Decision) -> bool {
    match pending().lock().unwrap().remove(id) {
        Some(tx) => tx.send(d).is_ok(),
        None => false,
    }
}

/// Onayı Telegram'dan ister (butonlu mesaj). Yanıt gelmezse veya Telegram yoksa reddeder.
/// Not: butonlara basılınca yanıtı `serve` modundaki bot döngüsü iletir.
pub async fn ask_telegram(req: &ApprovalRequest, chat_ids: &[i64]) -> Decision {
    let Ok(cfg) = Config::load() else { return Decision::Deny };
    let token = cfg.telegram.bot_token.clone();
    let chats: Vec<i64> = if chat_ids.is_empty() { cfg.telegram.allowed_chat_ids.clone() } else { chat_ids.to_vec() };
    if token.is_empty() || chats.is_empty() {
        return Decision::Deny;
    }
    let (tx, rx) = oneshot::channel();
    pending().lock().unwrap().insert(req.id.clone(), tx);
    let text = format!(
        "Onay gerekiyor ({})\nAgent: {}\nAraç: {}\nİşlem: {}",
        req.risk.label(),
        req.agent,
        req.tool,
        req.summary
    );
    let mut row = vec![json!({"text": "Evet", "callback_data": format!("ap|{}|y", req.id)})];
    if req.risk != Risk::Danger {
        row.push(json!({"text": "Oturum boyunca", "callback_data": format!("ap|{}|s", req.id)}));
    }
    row.push(json!({"text": "Hayır", "callback_data": format!("ap|{}|n", req.id)}));
    let http = reqwest::Client::new();
    for c in &chats {
        let _ = http
            .post(format!("https://api.telegram.org/bot{token}/sendMessage"))
            .json(&json!({"chat_id": c, "text": text, "reply_markup": {"inline_keyboard": [row]}}))
            .send()
            .await;
    }
    match tokio::time::timeout(Duration::from_secs(15 * 60), rx).await {
        Ok(Ok(d)) => d,
        _ => {
            pending().lock().unwrap().remove(&req.id);
            Decision::Deny
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a(tool: &str, input: Value) -> Assessment {
        let ctx = ToolCtx { agent: "t".into(), session: "t".into() };
        tokio::runtime::Runtime::new().unwrap().block_on(assess(tool, &input, &ctx))
    }

    #[test]
    fn shell_risk_levels() {
        assert_eq!(a("shell", json!({"command": "git status"})).risk, Risk::Safe);
        assert_eq!(a("shell", json!({"command": "Get-ChildItem C:/"})).risk, Risk::Safe);
        assert_eq!(a("shell", json!({"command": "git status; rm -rf /"})).risk, Risk::Danger);
        assert_eq!(a("shell", json!({"command": "Remove-Item -Recurse -Force C:/x"})).risk, Risk::Danger);
        assert_eq!(a("shell", json!({"command": "git push --force origin main"})).risk, Risk::Danger);
        assert_eq!(a("shell", json!({"command": "cargo build --release"})).risk, Risk::Safe);
        assert_eq!(a("shell", json!({"command": "npm install express"})).risk, Risk::Write);
        assert_eq!(a("shell", json!({"command": "ls > out.txt"})).risk, Risk::Write);
    }

    #[test]
    fn rules_and_modes() {
        let mut p = Permissions::default();
        let g = std::collections::HashSet::new();
        let push = a("shell", json!({"command": "git push origin main"}));
        assert!(matches!(evaluate(&p, &g, "shell", &push), Verdict::Ask));
        p.allow.push("shell(git push*)".into());
        assert!(matches!(evaluate(&p, &g, "shell", &push), Verdict::Allow));
        p.deny.push("shell(*origin main*)".into());
        assert!(matches!(evaluate(&p, &g, "shell", &push), Verdict::Deny(_)));

        let p = Permissions { mode: PermissionMode::Readonly, ..Default::default() };
        let w = a("write_file", json!({"path": "a.txt", "content": "x"}));
        assert!(matches!(evaluate(&p, &g, "write_file", &w), Verdict::Deny(_)));
        let r = a("read_file", json!({"path": "a.txt"}));
        assert!(matches!(evaluate(&p, &g, "read_file", &r), Verdict::Allow));

        let p = Permissions { mode: PermissionMode::Auto, ..Default::default() };
        assert!(matches!(evaluate(&p, &g, "write_file", &w), Verdict::Allow));
        let rm = a("shell", json!({"command": "rm -rf build"}));
        assert!(matches!(evaluate(&p, &g, "shell", &rm), Verdict::Ask));
        let ssh = a("write_file", json!({"path": "C:/Users/x/.ssh/authorized_keys", "content": "k"}));
        assert_eq!(ssh.risk, Risk::Danger);
    }

    #[test]
    fn session_grant() {
        let p = Permissions::default();
        let mut g = std::collections::HashSet::new();
        let c = a("computer", json!({"action": "click", "x": 1, "y": 2}));
        assert!(matches!(evaluate(&p, &g, "computer", &c), Verdict::Ask));
        g.insert(c.grant_key.clone());
        assert!(matches!(evaluate(&p, &g, "computer", &c), Verdict::Allow));
    }

    #[test]
    fn danger_labels() {
        for l in ["Satın Al", "Siparişi Tamamla", "Buy now", "Ödemeyi yap", "Hesabı sil", "Gönder"] {
            assert!(danger_label().is_match(l), "{l}");
        }
        for l in ["Ara", "Giriş yap", "Sonraki sayfa", "Learn more"] {
            assert!(!danger_label().is_match(l), "{l}");
        }
    }
}
