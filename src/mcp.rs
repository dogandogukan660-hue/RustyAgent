//! MCP (Model Context Protocol) istemcisi — JSON-RPC 2.0.
//! Taşımalar: stdio (yerel süreç) ve Streamable HTTP (uzak sunucu).
//! Yapılandırma Claude Code ile uyumlu `.mcp.json` biçimindedir:
//! { "mcpServers": {
//!     "yerel": { "command": "npx", "args": [...], "env": {...} },
//!     "uzak":  { "type": "http", "url": "https://.../mcp", "headers": {"Authorization": "Bearer ..."} } } }
//! MCP araçları agent'lara `mcp__<sunucu>__<araç>` adıyla verilir.

use crate::config;
use crate::tools::{Tool, ToolOutput, ToolSet};
use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin};
use tokio::sync::{oneshot, Mutex};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ServerConfig {
    /// "stdio" (varsayılan) | "http"
    #[serde(rename = "type", default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub command: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    /// Uzak sunucu adresi (Streamable HTTP)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub headers: BTreeMap<String, String>,
}

impl ServerConfig {
    pub fn is_remote(&self) -> bool {
        self.url.is_some() || matches!(self.kind.as_deref(), Some("http") | Some("streamable-http") | Some("sse"))
    }
    pub fn describe(&self) -> String {
        match &self.url {
            Some(u) => format!("[http] {u}"),
            None => format!("{} {}", self.command, self.args.join(" ")),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct McpFile {
    #[serde(rename = "mcpServers", default)]
    pub servers: BTreeMap<String, ServerConfig>,
}

pub fn global_file() -> PathBuf {
    config::home_dir().join("mcp.json")
}

/// Global + proje + plugin MCP dosyalarını birleştirir
pub fn load_all() -> BTreeMap<String, ServerConfig> {
    let mut files = vec![global_file(), PathBuf::from(".mcp.json")];
    files.extend(crate::plugins::installed_dirs().into_iter().map(|d| d.join(".mcp.json")));
    let mut out = BTreeMap::new();
    for f in files {
        if let Ok(text) = std::fs::read_to_string(&f) {
            if let Ok(parsed) = serde_json::from_str::<McpFile>(&text) {
                out.extend(parsed.servers);
            }
        }
    }
    out
}

pub fn add_server(name: &str, cfg: ServerConfig) -> Result<()> {
    let path = global_file();
    let mut file: McpFile = std::fs::read_to_string(&path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default();
    file.servers.insert(name.to_string(), cfg);
    std::fs::create_dir_all(path.parent().unwrap())?;
    std::fs::write(&path, serde_json::to_string_pretty(&file)?)?;
    Ok(())
}

pub fn remove_server(name: &str) -> Result<bool> {
    let path = global_file();
    let mut file: McpFile = std::fs::read_to_string(&path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default();
    let removed = file.servers.remove(name).is_some();
    std::fs::write(&path, serde_json::to_string_pretty(&file)?)?;
    Ok(removed)
}

// ------------------------------------------------------------------ client

type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<Value>>>>;

enum Transport {
    Stdio {
        stdin: Mutex<ChildStdin>,
        pending: Pending,
        _child: Mutex<Child>,
    },
    Http {
        http: reqwest::Client,
        url: String,
        headers: BTreeMap<String, String>,
        /// Sunucunun verdiği oturum kimliği (Mcp-Session-Id)
        session: std::sync::Mutex<Option<String>>,
        protocol: std::sync::Mutex<Option<String>>,
    },
}

pub struct McpClient {
    pub name: String,
    transport: Transport,
    next_id: AtomicU64,
}

const PROTOCOL_VERSION: &str = "2025-06-18";

impl McpClient {
    /// Yapılandırmaya göre stdio süreci başlatır veya uzak sunucuya bağlanır
    pub async fn spawn(name: &str, cfg: &ServerConfig) -> Result<Arc<Self>> {
        if cfg.is_remote() {
            return Self::connect_http(name, cfg).await;
        }
        // Windows'ta npx/uvx gibi .cmd betikleri doğrudan çalıştırılamaz
        let mut cmd = if cfg!(windows) {
            let mut c = tokio::process::Command::new("cmd");
            c.arg("/C").arg(&cfg.command).args(&cfg.args);
            c
        } else {
            let mut c = tokio::process::Command::new(&cfg.command);
            c.args(&cfg.args);
            c
        };
        crate::hide_console(&mut cmd);
        cmd.envs(&cfg.env)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true);
        let mut child = cmd.spawn().with_context(|| format!("MCP '{name}' başlatılamadı"))?;
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let pending: Pending = Arc::new(Mutex::new(HashMap::new()));

        let p2 = pending.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let Ok(msg) = serde_json::from_str::<Value>(&line) else { continue };
                if let Some(id) = msg["id"].as_u64() {
                    if msg.get("method").is_none() {
                        if let Some(tx) = p2.lock().await.remove(&id) {
                            let _ = tx.send(msg);
                        }
                    }
                }
            }
        });

        let client = Arc::new(Self {
            name: name.to_string(),
            transport: Transport::Stdio { stdin: Mutex::new(stdin), pending, _child: Mutex::new(child) },
            next_id: AtomicU64::new(1),
        });
        client.initialize().await?;
        Ok(client)
    }

    async fn connect_http(name: &str, cfg: &ServerConfig) -> Result<Arc<Self>> {
        let url = cfg.url.clone().context("uzak MCP için url gerekli")?;
        if cfg.kind.as_deref() == Some("sse") {
            bail!("'{name}': eski SSE taşıması desteklenmiyor; sunucunun Streamable HTTP adresini kullan (genelde .../mcp)");
        }
        let client = Arc::new(Self {
            name: name.to_string(),
            transport: Transport::Http {
                http: reqwest::Client::builder().connect_timeout(std::time::Duration::from_secs(15)).build()?,
                url,
                headers: cfg.headers.clone(),
                session: std::sync::Mutex::new(None),
                protocol: std::sync::Mutex::new(None),
            },
            next_id: AtomicU64::new(1),
        });
        client.initialize().await?;
        Ok(client)
    }

    async fn initialize(&self) -> Result<()> {
        let r = self
            .request(
                "initialize",
                json!({
                    "protocolVersion": PROTOCOL_VERSION,
                    "capabilities": {},
                    "clientInfo": {"name": "rusty-agent", "version": env!("CARGO_PKG_VERSION")}
                }),
            )
            .await?;
        if let Transport::Http { protocol, .. } = &self.transport {
            *protocol.lock().unwrap() = r["protocolVersion"].as_str().map(String::from).or(Some(PROTOCOL_VERSION.into()));
        }
        self.notify("notifications/initialized", json!({})).await
    }

    /// HTTP: bir JSON-RPC mesajı gönderir; yanıt JSON veya SSE akışı olabilir
    async fn http_post(&self, msg: &Value, want_id: Option<u64>) -> Result<Option<Value>> {
        let Transport::Http { http, url, headers, session, protocol } = &self.transport else { unreachable!() };
        let mut rb = http
            .post(url)
            .header("Content-Type", "application/json")
            .header("Accept", "application/json, text/event-stream")
            .json(msg);
        for (k, v) in headers {
            rb = rb.header(k, v);
        }
        if let Some(s) = session.lock().unwrap().clone() {
            rb = rb.header("Mcp-Session-Id", s);
        }
        if let Some(p) = protocol.lock().unwrap().clone() {
            rb = rb.header("MCP-Protocol-Version", p);
        }
        let resp = rb.send().await.with_context(|| format!("MCP '{}' bağlantı hatası", self.name))?;
        if let Some(s) = resp.headers().get("mcp-session-id").and_then(|v| v.to_str().ok()) {
            *session.lock().unwrap() = Some(s.to_string());
        }
        let status = resp.status();
        if status.as_u16() == 202 || want_id.is_none() {
            return Ok(None);
        }
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            if status.as_u16() == 401 || status.as_u16() == 403 {
                bail!("MCP '{}' yetkilendirme hatası ({status}); mcp.json'da headers ile anahtar ver: {body}", self.name);
            }
            bail!("MCP '{}' HTTP {status}: {body}", self.name);
        }
        let is_sse = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .map(|c| c.contains("text/event-stream"))
            .unwrap_or(false);
        if !is_sse {
            return Ok(Some(resp.json().await?));
        }
        // SSE: bizim id'mize ait yanıt gelene kadar oku
        let mut sse = crate::llm::SseParser::default();
        let mut stream = resp.bytes_stream();
        use futures::StreamExt;
        while let Some(chunk) = stream.next().await {
            for data in sse.push(&chunk?) {
                let Ok(v) = serde_json::from_str::<Value>(&data) else { continue };
                if v["id"].as_u64() == want_id && v.get("method").is_none() {
                    return Ok(Some(v));
                }
            }
        }
        bail!("MCP '{}': yanıt gelmeden akış kapandı", self.name)
    }

    async fn send(&self, msg: Value) -> Result<()> {
        match &self.transport {
            Transport::Stdio { stdin, .. } => {
                let mut s = stdin.lock().await;
                s.write_all(format!("{msg}\n").as_bytes()).await?;
                s.flush().await?;
                Ok(())
            }
            Transport::Http { .. } => self.http_post(&msg, None).await.map(|_| ()),
        }
    }

    async fn notify(&self, method: &str, params: Value) -> Result<()> {
        self.send(json!({"jsonrpc":"2.0","method":method,"params":params})).await
    }

    pub async fn request(&self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let msg = json!({"jsonrpc":"2.0","id":id,"method":method,"params":params});
        let resp = match &self.transport {
            Transport::Stdio { pending, .. } => {
                let (tx, rx) = oneshot::channel();
                pending.lock().await.insert(id, tx);
                self.send(msg).await?;
                tokio::time::timeout(std::time::Duration::from_secs(120), rx)
                    .await
                    .context("MCP zaman aşımı")??
            }
            Transport::Http { .. } => tokio::time::timeout(std::time::Duration::from_secs(120), self.http_post(&msg, Some(id)))
                .await
                .context("MCP zaman aşımı")??
                .context("boş yanıt")?,
        };
        if let Some(err) = resp.get("error") {
            bail!("MCP hatası: {err}");
        }
        Ok(resp["result"].clone())
    }

    pub async fn list_tools(&self) -> Result<Vec<Value>> {
        let r = self.request("tools/list", json!({})).await?;
        Ok(r["tools"].as_array().cloned().unwrap_or_default())
    }
}

/// Bir MCP aracını yerel Tool arayüzüne saran adaptör
struct McpTool {
    client: Arc<McpClient>,
    tool_name: String,
    description: String,
    schema: Value,
}

#[async_trait]
impl Tool for McpTool {
    fn name(&self) -> String {
        format!("mcp__{}__{}", sanitize(&self.client.name), sanitize(&self.tool_name))
    }
    fn description(&self) -> String { self.description.clone() }
    fn input_schema(&self) -> Value { self.schema.clone() }
    async fn call(&self, input: Value) -> ToolOutput {
        match self.client.request("tools/call", json!({"name": self.tool_name, "arguments": input})).await {
            Err(e) => ToolOutput::Error(e.to_string()),
            Ok(r) => {
                let blocks: Vec<Value> = r["content"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default()
                    .into_iter()
                    .filter_map(|c| match c["type"].as_str() {
                        Some("text") => Some(json!({"type":"text","text": c["text"]})),
                        Some("image") => Some(json!({"type":"image","source":{
                            "type":"base64","media_type": c["mimeType"],"data": c["data"]}})),
                        _ => Some(json!({"type":"text","text": c.to_string()})),
                    })
                    .collect();
                if r["isError"].as_bool().unwrap_or(false) {
                    ToolOutput::Error(blocks.iter().filter_map(|b| b["text"].as_str()).collect::<Vec<_>>().join("\n"))
                } else if blocks.is_empty() {
                    ToolOutput::Text("(boş sonuç)".into())
                } else {
                    ToolOutput::Blocks(blocks)
                }
            }
        }
    }
}

fn sanitize(s: &str) -> String {
    s.chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '_' }).collect()
}

/// Çalışan tüm MCP sunucuları (uygulama ömrü boyunca açık kalır)
pub struct McpHub {
    pub clients: Vec<Arc<McpClient>>,
    pub errors: Vec<String>,
}

impl McpHub {
    pub async fn start_all() -> Self {
        let mut clients = Vec::new();
        let mut errors = Vec::new();
        let futs = load_all().into_iter().map(|(name, cfg)| async move {
            (name.clone(), McpClient::spawn(&name, &cfg).await)
        });
        for (name, res) in futures::future::join_all(futs).await {
            match res {
                Ok(c) => clients.push(c),
                Err(e) => errors.push(format!("{name}: {e:#}")),
            }
        }
        Self { clients, errors }
    }

    pub async fn register_tools(&self, ts: &mut ToolSet) {
        for c in &self.clients {
            match c.list_tools().await {
                Ok(tools) => {
                    for t in tools {
                        ts.add(Arc::new(McpTool {
                            client: c.clone(),
                            tool_name: t["name"].as_str().unwrap_or("").into(),
                            description: t["description"].as_str().unwrap_or("").into(),
                            schema: if t["inputSchema"].is_object() { t["inputSchema"].clone() } else { json!({"type":"object"}) },
                        }));
                    }
                }
                Err(e) => eprintln!("MCP {} araçları alınamadı: {e}", c.name),
            }
        }
    }
}

// ------------------------------------------------------------------ browse

#[derive(Debug, Clone)]
pub struct RegistryEntry {
    pub name: String,
    pub description: String,
    /// Önerilen kurulum (ör. npx -y paket)
    pub install: Option<ServerConfig>,
}

/// Resmi MCP kayıt defterinde arama: registry.modelcontextprotocol.io
pub async fn browse(query: &str) -> Result<Vec<RegistryEntry>> {
    let client = reqwest::Client::builder().user_agent("rusty-agent").build()?;
    let v: Value = client
        .get("https://registry.modelcontextprotocol.io/v0/servers")
        .query(&[("search", query), ("limit", "30")])
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let mut out = Vec::new();
    for item in v["servers"].as_array().cloned().unwrap_or_default() {
        let s = if item["server"].is_object() { &item["server"] } else { &item };
        let stdio = |command: &str, args: Vec<String>| ServerConfig { command: command.into(), args, ..Default::default() };
        let install = s["packages"]
            .as_array()
            .and_then(|pkgs| {
                pkgs.iter().find_map(|p| {
                    let id = p["identifier"].as_str().or(p["name"].as_str())?;
                    match p["registryType"].as_str().or(p["registry_name"].as_str())? {
                        "npm" => Some(stdio("npx", vec!["-y".into(), id.into()])),
                        "pypi" => Some(stdio("uvx", vec![id.into()])),
                        "oci" | "docker" => Some(stdio("docker", vec!["run".into(), "-i".into(), "--rm".into(), id.into()])),
                        _ => None,
                    }
                })
            })
            // Yerel paket yoksa uzak (Streamable HTTP) adresi
            .or_else(|| {
                s["remotes"].as_array()?.iter().find_map(|r| {
                    let t = r["type"].as_str().unwrap_or("");
                    (t == "streamable-http" || t == "http").then(|| ServerConfig {
                        kind: Some("http".into()),
                        url: r["url"].as_str().map(String::from),
                        ..Default::default()
                    })
                })
            });
        // Kayıt defteri her sürümü ayrı döndürür; ad başına bir kayıt tut
        let name = s["name"].as_str().unwrap_or("?");
        if out.iter().any(|e: &RegistryEntry| e.name == name) {
            continue;
        }
        out.push(RegistryEntry {
            name: s["name"].as_str().unwrap_or("?").into(),
            description: s["description"].as_str().unwrap_or("").into(),
            install,
        });
    }
    Ok(out)
}
