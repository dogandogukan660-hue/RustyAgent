//! RustyAgent — çok-agent'lı, MCP/skill/plugin destekli kişisel AI agent platformu.
//!
//! İki program bu kütüphaneyi kullanır:
//!   RustyAgent.exe  — masaüstü uygulaması (Windows'ta konsol penceresi açmaz)
//!   rusty-agent.exe — komut satırı: run, tui, serve, mcp, skill, plugin…

pub mod agent;
pub mod automation;
pub mod brain;
pub mod character;
pub mod commands;
pub mod config;
pub mod gui;
pub mod llm;
pub mod markdown;
pub mod mcp;
pub mod models;
pub mod notify;
pub mod openai;
pub mod overlay;
pub mod permissions;
pub mod plugins;
pub mod sessions;
pub mod skills;
pub mod telegram;
pub mod theme;
pub mod tools;
pub mod tui;
pub mod voice;

/// Windows'ta alt işlemlerin konsol penceresi açmaması için bayrak (CREATE_NO_WINDOW)
pub const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// std::process::Command için: Windows'ta pencere açmadan çalıştır
pub fn hide_console_std(cmd: &mut std::process::Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(not(windows))]
    let _ = cmd;
}

/// tokio::process::Command için: Windows'ta pencere açmadan çalıştır
pub fn hide_console(cmd: &mut tokio::process::Command) {
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);
    #[cfg(not(windows))]
    let _ = cmd;
}

/// Masaüstü uygulamasını başlatır (ana iş parçacığında çağrılmalı)
pub fn launch_gui(resume: Option<Option<String>>, page: Option<String>) -> anyhow::Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    let cfg = config::Config::load()?;
    let rt = runtime.block_on(agent::Runtime::new(cfg))?;
    gui::run(rt, runtime.handle().clone(), resume, page)
}
