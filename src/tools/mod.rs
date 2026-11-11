//! Araç (tool) sistemi. Her araç Claude'a JSON şema ile tanıtılır.

pub mod browser;
pub mod builtin;
pub mod computer;

use async_trait::async_trait;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::sync::Arc;

pub enum ToolOutput {
    Text(String),
    /// Zengin içerik (ör. ekran görüntüsü image bloğu)
    Blocks(Vec<Value>),
    Error(String),
}

impl ToolOutput {
    pub fn to_result_block(self, tool_use_id: &str) -> Value {
        match self {
            ToolOutput::Text(t) => json!({"type": "tool_result", "tool_use_id": tool_use_id, "content": truncate(&t)}),
            ToolOutput::Blocks(b) => json!({"type": "tool_result", "tool_use_id": tool_use_id, "content": b}),
            ToolOutput::Error(e) => json!({"type": "tool_result", "tool_use_id": tool_use_id, "content": e, "is_error": true}),
        }
    }
    pub fn summary(&self) -> String {
        match self {
            ToolOutput::Text(t) => first_line(t),
            ToolOutput::Blocks(_) => "[görsel/zengin içerik]".into(),
            ToolOutput::Error(e) => format!("HATA: {}", first_line(e)),
        }
    }
}

fn first_line(s: &str) -> String {
    let l = s.lines().next().unwrap_or("");
    if l.chars().count() > 120 { format!("{}…", l.chars().take(120).collect::<String>()) } else { l.to_string() }
}

/// Çok uzun çıktıları kes (bağlam penceresini korumak için)
pub fn truncate(s: &str) -> String {
    const MAX: usize = 60_000;
    if s.len() <= MAX {
        return s.to_string();
    }
    let mut end = MAX;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n\n[... çıktı kesildi, toplam {} bayt]", &s[..end], s.len())
}

/// Aracı çağıran agent bilgisi (tarayıcı sayfası ve sanal imleç agent'a özeldir)
#[derive(Clone, Debug)]
pub struct ToolCtx {
    /// Agent adı (imleç etiketi)
    pub agent: String,
    /// Oturum kimliği (aynı agent'ın paralel kopyaları ayrı sayfa alır)
    pub session: String,
}

#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> String;
    fn description(&self) -> String;
    fn input_schema(&self) -> Value;
    async fn call(&self, input: Value) -> ToolOutput;
    /// Agent bağlamıyla çağrı; bağlama ihtiyaç duyan araçlar bunu ezer
    async fn call_ctx(&self, input: Value, _ctx: &ToolCtx) -> ToolOutput {
        self.call(input).await
    }

    fn definition(&self) -> Value {
        json!({
            "name": self.name(),
            "description": self.description(),
            "input_schema": self.input_schema(),
            // Büyük girdiler (dosya içerikleri) üretilirken akış halinde gelsin
            "eager_input_streaming": true,
        })
    }
}

#[derive(Clone, Default)]
pub struct ToolSet {
    tools: BTreeMap<String, Arc<dyn Tool>>,
    /// Anthropic sunucu araçları (web arama vb.) — istemcide çalışmaz
    pub server_tools: Vec<Value>,
}

impl ToolSet {
    pub fn add(&mut self, t: Arc<dyn Tool>) {
        self.tools.insert(t.name(), t);
    }
    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.get(name).cloned()
    }
    pub fn names(&self) -> Vec<String> {
        self.tools.keys().cloned().collect()
    }
    /// Sadece izin verilen araçları tut ("*" veya boş = hepsi). "mcp__x__*" gibi önek desteklenir.
    pub fn filtered(&self, allow: &[String]) -> ToolSet {
        if allow.is_empty() || allow.iter().any(|a| a == "*") {
            return self.clone();
        }
        let ok = |n: &str| {
            allow.iter().any(|a| match a.strip_suffix('*') {
                Some(prefix) => n.starts_with(prefix),
                None => a == n,
            })
        };
        let mut out = ToolSet { server_tools: self.server_tools.clone(), ..Default::default() };
        for (k, v) in &self.tools {
            if ok(k) {
                out.tools.insert(k.clone(), v.clone());
            }
        }
        if !ok("web_search") {
            out.server_tools.clear();
        }
        out
    }
    /// API'ye gönderilecek araç listesi (deterministik sıra => prompt cache dostu)
    pub fn definitions(&self) -> Vec<Value> {
        let mut v: Vec<Value> = self.tools.values().map(|t| t.definition()).collect();
        v.extend(self.server_tools.iter().cloned());
        v
    }
}

/// Varsayılan yerleşik araçlar
pub fn default_toolset(computer_use: bool, browser: bool) -> ToolSet {
    let mut ts = ToolSet::default();
    builtin::register(&mut ts);
    if computer_use {
        ts.add(Arc::new(computer::Computer::new()));
    }
    if browser {
        ts.add(Arc::new(browser::BrowserTool));
    }
    ts.server_tools.push(json!({"type": "web_search_20260209", "name": "web_search", "max_uses": 8}));
    ts
}
