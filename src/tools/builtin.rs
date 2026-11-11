//! Yerleşik araçlar: shell, dosya okuma/yazma/düzenleme, glob, grep, web_fetch.

use super::{Tool, ToolOutput, ToolSet};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;

pub fn register(ts: &mut ToolSet) {
    ts.add(Arc::new(Shell));
    ts.add(Arc::new(ReadFile));
    ts.add(Arc::new(WriteFile));
    ts.add(Arc::new(EditFile));
    ts.add(Arc::new(Glob));
    ts.add(Arc::new(Grep));
    ts.add(Arc::new(WebFetch));
}

fn s<'a>(v: &'a Value, k: &str) -> Option<&'a str> {
    v.get(k).and_then(|x| x.as_str())
}

// ---------------------------------------------------------------- shell
pub struct Shell;

#[async_trait]
impl Tool for Shell {
    fn name(&self) -> String { "shell".into() }
    fn description(&self) -> String {
        let sh = if cfg!(windows) { "PowerShell" } else { "sh" };
        format!("Bir {sh} komutu çalıştırır ve stdout+stderr döndürür. Varsayılan zaman aşımı 120 sn.")
    }
    fn input_schema(&self) -> Value {
        json!({"type":"object","properties":{
            "command":{"type":"string","description":"Çalıştırılacak komut"},
            "timeout_secs":{"type":"integer","description":"Zaman aşımı (sn), en fazla 600"}
        },"required":["command"],"additionalProperties":false})
    }
    async fn call(&self, input: Value) -> ToolOutput {
        let Some(cmd) = s(&input, "command") else { return ToolOutput::Error("command gerekli".into()) };
        let timeout = input["timeout_secs"].as_u64().unwrap_or(120).min(600);
        let mut c = if cfg!(windows) {
            let mut c = tokio::process::Command::new("powershell");
            c.args(["-NoProfile", "-NonInteractive", "-Command", cmd]);
            c
        } else {
            let mut c = tokio::process::Command::new("sh");
            c.args(["-c", cmd]);
            c
        };
        c.kill_on_drop(true).stdin(std::process::Stdio::null());
        crate::hide_console(&mut c);
        match tokio::time::timeout(Duration::from_secs(timeout), c.output()).await {
            Err(_) => ToolOutput::Error(format!("Komut {timeout} sn içinde bitmedi")),
            Ok(Err(e)) => ToolOutput::Error(format!("Çalıştırılamadı: {e}")),
            Ok(Ok(out)) => {
                let mut text = String::from_utf8_lossy(&out.stdout).to_string();
                let err = String::from_utf8_lossy(&out.stderr);
                if !err.trim().is_empty() {
                    text.push_str("\n[stderr]\n");
                    text.push_str(&err);
                }
                let code = out.status.code().unwrap_or(-1);
                if text.trim().is_empty() {
                    text = "(çıktı yok)".into();
                }
                if out.status.success() {
                    ToolOutput::Text(text)
                } else {
                    ToolOutput::Error(format!("çıkış kodu {code}\n{text}"))
                }
            }
        }
    }
}

// ---------------------------------------------------------------- read_file
pub struct ReadFile;

#[async_trait]
impl Tool for ReadFile {
    fn name(&self) -> String { "read_file".into() }
    fn description(&self) -> String {
        "Bir metin dosyasını satır numaralarıyla okur. Büyük dosyalarda offset/limit kullan.".into()
    }
    fn input_schema(&self) -> Value {
        json!({"type":"object","properties":{
            "path":{"type":"string"},
            "offset":{"type":"integer","description":"Başlangıç satırı (1'den)"},
            "limit":{"type":"integer","description":"Okunacak satır sayısı (varsayılan 2000)"}
        },"required":["path"],"additionalProperties":false})
    }
    async fn call(&self, input: Value) -> ToolOutput {
        let Some(path) = s(&input, "path") else { return ToolOutput::Error("path gerekli".into()) };
        match tokio::fs::read(path).await {
            Err(e) => ToolOutput::Error(format!("{path}: {e}")),
            Ok(bytes) => {
                let text = String::from_utf8_lossy(&bytes);
                let offset = input["offset"].as_u64().unwrap_or(1).max(1) as usize;
                let limit = input["limit"].as_u64().unwrap_or(2000) as usize;
                let out: String = text
                    .lines()
                    .enumerate()
                    .skip(offset - 1)
                    .take(limit)
                    .map(|(i, l)| format!("{:>6}\t{}\n", i + 1, l))
                    .collect();
                ToolOutput::Text(if out.is_empty() { "(boş)".into() } else { out })
            }
        }
    }
}

// ---------------------------------------------------------------- write_file
pub struct WriteFile;

#[async_trait]
impl Tool for WriteFile {
    fn name(&self) -> String { "write_file".into() }
    fn description(&self) -> String { "Dosya oluşturur veya tamamen üzerine yazar. Klasörleri otomatik oluşturur.".into() }
    fn input_schema(&self) -> Value {
        json!({"type":"object","properties":{
            "path":{"type":"string"},"content":{"type":"string"}
        },"required":["path","content"],"additionalProperties":false})
    }
    async fn call(&self, input: Value) -> ToolOutput {
        let (Some(path), Some(content)) = (s(&input, "path"), s(&input, "content")) else {
            return ToolOutput::Error("path ve content gerekli".into());
        };
        if let Some(parent) = std::path::Path::new(path).parent() {
            let _ = tokio::fs::create_dir_all(parent).await;
        }
        match tokio::fs::write(path, content).await {
            Ok(_) => ToolOutput::Text(format!("{path} yazıldı ({} bayt)", content.len())),
            Err(e) => ToolOutput::Error(format!("{path}: {e}")),
        }
    }
}

// ---------------------------------------------------------------- edit_file
pub struct EditFile;

#[async_trait]
impl Tool for EditFile {
    fn name(&self) -> String { "edit_file".into() }
    fn description(&self) -> String {
        "Dosyada old_string'i new_string ile değiştirir. old_string dosyada tam olarak bir kez geçmeli (replace_all=true değilse).".into()
    }
    fn input_schema(&self) -> Value {
        json!({"type":"object","properties":{
            "path":{"type":"string"},"old_string":{"type":"string"},"new_string":{"type":"string"},
            "replace_all":{"type":"boolean"}
        },"required":["path","old_string","new_string"],"additionalProperties":false})
    }
    async fn call(&self, input: Value) -> ToolOutput {
        let (Some(path), Some(old), Some(new)) = (s(&input, "path"), s(&input, "old_string"), s(&input, "new_string")) else {
            return ToolOutput::Error("path, old_string, new_string gerekli".into());
        };
        let text = match tokio::fs::read_to_string(path).await {
            Ok(t) => t,
            Err(e) => return ToolOutput::Error(format!("{path}: {e}")),
        };
        let count = text.matches(old).count();
        let all = input["replace_all"].as_bool().unwrap_or(false);
        if count == 0 {
            return ToolOutput::Error("old_string dosyada bulunamadı".into());
        }
        if count > 1 && !all {
            return ToolOutput::Error(format!("old_string {count} kez geçiyor; daha fazla bağlam ekle veya replace_all kullan"));
        }
        let updated = if all { text.replace(old, new) } else { text.replacen(old, new, 1) };
        match tokio::fs::write(path, updated).await {
            Ok(_) => ToolOutput::Text(format!("{path} düzenlendi ({count} değişiklik)")),
            Err(e) => ToolOutput::Error(e.to_string()),
        }
    }
}

// ---------------------------------------------------------------- glob
pub struct Glob;

#[async_trait]
impl Tool for Glob {
    fn name(&self) -> String { "glob".into() }
    fn description(&self) -> String { "Glob desenine uyan dosyaları listeler (ör. src/**/*.rs).".into() }
    fn input_schema(&self) -> Value {
        json!({"type":"object","properties":{
            "pattern":{"type":"string","description":"ör. **/*.rs"},
            "path":{"type":"string","description":"Aranacak klasör (varsayılan: .)"}
        },"required":["pattern"],"additionalProperties":false})
    }
    async fn call(&self, input: Value) -> ToolOutput {
        let Some(p) = s(&input, "pattern") else { return ToolOutput::Error("pattern gerekli".into()) };
        let full = match s(&input, "path") {
            Some(dir) if !dir.is_empty() && dir != "." => format!("{}/{}", dir.trim_end_matches(['/', '\\']), p),
            _ => p.to_string(),
        };
        match glob::glob(&full) {
            Err(e) => ToolOutput::Error(e.to_string()),
            Ok(paths) => {
                let list: Vec<String> = paths.filter_map(|r| r.ok()).take(500).map(|p| p.display().to_string()).collect();
                ToolOutput::Text(if list.is_empty() { "eşleşme yok".into() } else { list.join("\n") })
            }
        }
    }
}

// ---------------------------------------------------------------- grep
pub struct Grep;

#[async_trait]
impl Tool for Grep {
    fn name(&self) -> String { "grep".into() }
    fn description(&self) -> String { "Bir klasörde regex ile içerik arar; dosya:satır: içerik döndürür.".into() }
    fn input_schema(&self) -> Value {
        json!({"type":"object","properties":{
            "pattern":{"type":"string"},
            "path":{"type":"string","description":"Varsayılan: ."},
            "ignore_case":{"type":"boolean"}
        },"required":["pattern"],"additionalProperties":false})
    }
    async fn call(&self, input: Value) -> ToolOutput {
        let Some(p) = s(&input, "pattern") else { return ToolOutput::Error("pattern gerekli".into()) };
        let pat = if input["ignore_case"].as_bool().unwrap_or(false) { format!("(?i){p}") } else { p.to_string() };
        let re = match regex::Regex::new(&pat) {
            Ok(r) => r,
            Err(e) => return ToolOutput::Error(e.to_string()),
        };
        let root = s(&input, "path").unwrap_or(".").to_string();
        let res = tokio::task::spawn_blocking(move || {
            let mut out = Vec::new();
            for e in walkdir::WalkDir::new(&root)
                .into_iter()
                .filter_entry(|e| {
                    let n = e.file_name().to_string_lossy();
                    !(n == ".git" || n == "target" || n == "node_modules")
                })
                .filter_map(|e| e.ok())
                .filter(|e| e.file_type().is_file())
            {
                let Ok(text) = std::fs::read_to_string(e.path()) else { continue };
                for (i, line) in text.lines().enumerate() {
                    if re.is_match(line) {
                        out.push(format!("{}:{}: {}", e.path().display(), i + 1, line.trim()));
                        if out.len() >= 300 {
                            return out;
                        }
                    }
                }
            }
            out
        })
        .await
        .unwrap_or_default();
        ToolOutput::Text(if res.is_empty() { "eşleşme yok".into() } else { res.join("\n") })
    }
}

// ---------------------------------------------------------------- web_fetch
pub struct WebFetch;

#[async_trait]
impl Tool for WebFetch {
    fn name(&self) -> String { "web_fetch".into() }
    fn description(&self) -> String { "Bir URL'yi indirir ve metnini döndürür (HTML etiketleri temizlenir).".into() }
    fn input_schema(&self) -> Value {
        json!({"type":"object","properties":{"url":{"type":"string"}},"required":["url"],"additionalProperties":false})
    }
    async fn call(&self, input: Value) -> ToolOutput {
        let Some(url) = s(&input, "url") else { return ToolOutput::Error("url gerekli".into()) };
        let client = reqwest::Client::builder().user_agent("rusty-agent").timeout(Duration::from_secs(30)).build().unwrap();
        match client.get(url).send().await {
            Err(e) => ToolOutput::Error(e.to_string()),
            Ok(r) => {
                let status = r.status();
                let body = r.text().await.unwrap_or_default();
                let tag = regex::Regex::new(r"(?s)<script.*?</script>|<style.*?</style>|<[^>]+>").unwrap();
                let ws = regex::Regex::new(r"\n\s*\n+").unwrap();
                let clean = ws.replace_all(&tag.replace_all(&body, ""), "\n\n").to_string();
                ToolOutput::Text(format!("HTTP {status}\n\n{clean}"))
            }
        }
    }
}
