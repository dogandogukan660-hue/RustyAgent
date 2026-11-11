//! RustyAgent — çok-agent'lı, MCP/skill/plugin destekli kişisel AI agent platformu.


use anyhow::Result;
use rusty_agent::{agent, automation, commands, config, gui, mcp, overlay, permissions, sessions, telegram, tools, tui, voice};
use clap::{Parser, Subcommand};
use config::Config;

#[derive(Parser)]
#[command(name = "rusty-agent", version, about = "Çok-agent'lı kişisel AI asistanı")]
struct Cli {
    #[command(subcommand)]
    cmd: Option<Cmd>,
    /// En son oturuma devam et
    #[arg(short = 'c', long = "continue")]
    cont: bool,
    /// Belirli bir oturuma devam et (id veya önek; liste: rusty-agent sessions)
    #[arg(long)]
    resume: Option<String>,
    /// (test) masaüstü uygulamasını bu sayfada aç: chat, brain, agents, models, mcp, skills, plugins, automations, settings
    #[arg(long, hide = true)]
    page: Option<String>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Terminal arayüzü (argümansız çalıştırınca masaüstü uygulaması açılır)
    Tui,
    /// Tek bir görevi arayüzsüz çalıştır ve sonucu yazdır
    Run {
        prompt: Vec<String>,
        /// Hiç onay sorma (yolo) — dikkat
        #[arg(long)]
        yes: bool,
        /// Bu çalıştırma için model (ör. gemini/gemini-3.8-flash)
        #[arg(long)]
        model: Option<String>,
    },
    /// Arka plan servisi: Telegram botu + otomasyon zamanlayıcı
    Serve,
    /// Yapılandırma dosyasının yolunu göster
    Config,
    /// Bir aracı doğrudan çalıştır (test için): rusty-agent tool browser '{"action":"navigate","url":"example.com"}'
    Tool {
        name: String,
        input: String,
        #[arg(long, default_value = "test")]
        agent: String,
    },
    /// (iç kullanım) sanal imleç katmanı
    #[command(hide = true)]
    Overlay,
    /// Diğer komutlar: agents, mcp, skill, plugin, automation ... (bkz. `rusty-agent x help`)
    #[command(external_subcommand)]
    Other(Vec<String>),
}

/// `run` komutunda onayı terminalden sorar; terminal yoksa reddeder
async fn ask_stdin(req: &permissions::ApprovalRequest) -> permissions::Decision {
    use permissions::Decision;
    use std::io::IsTerminal;
    if !std::io::stdin().is_terminal() {
        eprintln!("⚠ Onay gerekiyordu ama terminal yok, reddedildi: {}", req.summary);
        return Decision::Deny;
    }
    let danger = req.risk == permissions::Risk::Danger;
    let prompt = format!(
        "\n⚠ [{}] onay istiyor ({}): {}\n  {} > ",
        req.agent,
        req.risk.label(),
        req.summary,
        if danger { "[e]vet / [h]ayır" } else { "[e]vet / [o]turum boyunca / [k]alıcı kural / [h]ayır" }
    );
    tokio::task::spawn_blocking(move || {
        eprint!("{prompt}");
        let mut line = String::new();
        let _ = std::io::stdin().read_line(&mut line);
        match line.trim().to_lowercase().as_str() {
            "e" | "y" | "evet" | "yes" => Decision::Allow,
            "o" | "s" if !danger => Decision::AllowSession,
            "k" | "a" if !danger => Decision::AllowAlways,
            _ => Decision::Deny,
        }
    })
    .await
    .unwrap_or(Decision::Deny)
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    // Katman penceresi ana iş parçacığında, tokio dışında çalışmalı (macOS gereksinimi)
    if let Some(Cmd::Overlay) = cli.cmd {
        return overlay::run_overlay();
    }
    let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    // Masaüstü uygulaması (varsayılan): pencere ana iş parçacığında, agent'lar tokio'da
    if cli.cmd.is_none() {
        let cfg = Config::load()?;
        let rt = runtime.block_on(agent::Runtime::new(cfg))?;
        let resume = if let Some(id) = cli.resume { Some(Some(id)) } else if cli.cont { Some(None) } else { None };
        return gui::run(rt, runtime.handle().clone(), resume, cli.page);
    }
    runtime.block_on(async_main(cli))
}

async fn async_main(cli: Cli) -> Result<()> {
    let cfg = Config::load()?;

    match cli.cmd {
        Some(Cmd::Overlay) => unreachable!(),
        Some(Cmd::Tool { name, input, agent }) => {
            let mut ts = tools::default_toolset(true, true);
            if name.starts_with("mcp__") {
                let hub = mcp::McpHub::start_all().await;
                hub.register_tools(&mut ts).await;
                Box::leak(Box::new(hub));
            }
            let t = ts.get(&name).ok_or_else(|| anyhow::anyhow!("'{name}' adında araç yok: {:?}", ts.names()))?;
            // Tek girdi ya da aynı oturumda sırayla çalışacak girdiler dizisi
            let input = match input.strip_prefix('@') {
                Some(path) => std::fs::read_to_string(path)?,
                None => input,
            };
            let input: serde_json::Value = serde_json::from_str(input.trim_start_matches('\u{feff}'))?;
            // Biçimler: {adım} | [adımlar] | {"agent1": [adımlar], "agent2": [...]} (agent'lar paralel)
            let plans: Vec<(String, Vec<serde_json::Value>)> = match input {
                serde_json::Value::Array(a) => vec![(agent, a)],
                serde_json::Value::Object(o) if !o.is_empty() && o.values().all(|v| v.is_array()) => o
                    .into_iter()
                    .map(|(k, v)| (k, v.as_array().cloned().unwrap_or_default()))
                    .collect(),
                v => vec![(agent, vec![v])],
            };
            let runs = plans.into_iter().map(|(agent, steps)| {
                let t = t.clone();
                async move {
                    let ctx = tools::ToolCtx { agent: agent.clone(), session: agent.clone() };
                    for step in steps {
                        if let Some(ms) = step["__sleep_ms"].as_u64() {
                            tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
                            continue;
                        }
                        let ts = chrono::Local::now().format("%H:%M:%S%.3f");
                        match t.call_ctx(step, &ctx).await {
                            tools::ToolOutput::Text(s) => println!("[{ts} {agent}] {}", s.lines().take(4).collect::<Vec<_>>().join(" | ")),
                            tools::ToolOutput::Blocks(b) => {
                                let text: Vec<&str> = b.iter().filter_map(|x| x["text"].as_str()).collect();
                                println!("[{ts} {agent}] {} blok | {}", b.len(), text.join(" ").lines().take(4).collect::<Vec<_>>().join(" | "))
                            }
                            tools::ToolOutput::Error(e) => println!("[{ts} {agent}] HATA: {e}"),
                        }
                    }
                }
            });
            futures::future::join_all(runs).await;
            Ok(())
        }
        None => unreachable!(),
        Some(Cmd::Tui) => {
            let rt = agent::Runtime::new(cfg).await?;
            let resume = if let Some(id) = cli.resume { Some(Some(id)) } else if cli.cont { Some(None) } else { None };
            tui::run(rt, resume).await
        }
        Some(Cmd::Run { prompt, yes, model }) => {
            let mut cfg = cfg;
            if let Some(m) = model {
                cfg.model = m;
            }
            let rt = agent::Runtime::new(cfg).await?;
            if yes {
                rt.perms.write().unwrap().mode = config::PermissionMode::Yolo;
            }
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<agent::AgentEvent>();
            // Onaylar sırayla sorulur (aynı anda birden çok soru terminali karıştırmasın)
            let ask_lock = std::sync::Arc::new(tokio::sync::Mutex::new(()));
            tokio::spawn(async move {
                while let Some(e) = rx.recv().await {
                    match e.kind {
                        agent::EventKind::ToolCall { name, input } => eprintln!("[{}] ⏺ {name}({input})", e.agent),
                        agent::EventKind::ToolResult { name, summary, .. } => eprintln!("[{}]   ⎿ {name} → {summary}", e.agent),
                        agent::EventKind::Info(s) | agent::EventKind::Status(s) => eprintln!("[{}] ℹ {s}", e.agent),
                        agent::EventKind::Approval(h) => {
                            let lock = ask_lock.clone();
                            tokio::spawn(async move {
                                let _g = lock.lock().await;
                                h.respond(ask_stdin(&h.req).await);
                            });
                        }
                        _ => {}
                    }
                }
            });
            let mut s = rt.main_session(Some(tx));
            // run -c / --resume: önceki oturuma devam et
            let saved = match (&cli.resume, cli.cont) {
                (Some(id), _) => Some(sessions::load(id)?),
                (None, true) => Some(sessions::latest()?),
                _ => None,
            };
            if let Some(saved) = saved {
                eprintln!("ℹ oturuma devam: {} — {}", saved.id, saved.title);
                s.resume(saved);
            }
            let out = s.send(&prompt.join(" ")).await?;
            println!("{out}");
            Ok(())
        }
        Some(Cmd::Serve) => {
            let rt = agent::Runtime::new(cfg).await?;
            tokio::spawn(automation::scheduler(rt.clone()));
            println!("Otomasyon zamanlayıcı çalışıyor ({} görev).", automation::load().len());
            if rt.cfg.voice.realtime_ready() {
                let rt2 = rt.clone();
                tokio::spawn(async move {
                    if let Err(e) = voice::serve(rt2).await {
                        eprintln!("Ses sunucusu durdu: {e:#}");
                    }
                });
            } else {
                println!("Sesli konuşma kapalı (config.toml [voice] eksik).");
            }
            if rt.cfg.telegram.bot_token.is_empty() {
                println!("Telegram ayarlı değil; sadece otomasyonlar çalışacak. Ctrl+C ile çık.");
                tokio::signal::ctrl_c().await?;
                Ok(())
            } else {
                telegram::run(rt).await
            }
        }
        Some(Cmd::Config) => {
            println!("{}", Config::path().display());
            Ok(())
        }
        Some(Cmd::Other(args)) => {
            let progress = |s: String| eprintln!("{s}");
            println!("{}", commands::run(&cfg, &args, &progress).await?);
            Ok(())
        }
    }
}
