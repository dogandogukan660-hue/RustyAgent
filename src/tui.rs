//! Claude Code benzeri terminal arayüzü (ratatui).

use crate::agent::{AgentEvent, EventKind, Runtime, MAIN_AGENT};
use crate::commands;
use crate::permissions::{ApprovalHandle, Decision, Risk};
use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;

#[derive(Clone, Copy, PartialEq)]
enum Kind { User, Text, Thinking, Tool, ToolOk, ToolErr, Info, Error }

struct Entry {
    kind: Kind,
    agent: String,
    text: String,
}

enum UiMsg {
    Agent(AgentEvent),
    Info(String),
    Error(String),
    TurnFinished,
    Model(String),
}

enum Job {
    Prompt(String),
    Reset,
    SetModel(String),
    Compact,
    /// None = en son oturum
    Resume(Option<String>),
}

struct App {
    entries: Vec<Entry>,
    input: String,
    cursor: usize,
    busy: bool,
    scroll_up: u16,
    tokens_in: u64,
    tokens_out: u64,
    spinner: usize,
    history: Vec<String>,
    hist_idx: Option<usize>,
    model: String,
    /// Kullanıcı onayı bekleyen işlemler (sırayla gösterilir)
    approvals: std::collections::VecDeque<ApprovalHandle>,
}

impl App {
    fn push(&mut self, kind: Kind, agent: &str, text: String) {
        self.entries.push(Entry { kind, agent: agent.into(), text });
        self.scroll_up = 0;
    }
    fn append(&mut self, kind: Kind, agent: &str, text: &str) {
        if let Some(last) = self.entries.last_mut() {
            if last.kind == kind && last.agent == agent {
                last.text.push_str(text);
                return;
            }
        }
        self.push(kind, agent, text.to_string());
    }
}

/// resume: Some(None) = son oturuma devam, Some(Some(id)) = belirli oturuma devam
pub async fn run(rt: Arc<Runtime>, resume: Option<Option<String>>) -> Result<()> {
    let (ui_tx, mut ui_rx) = mpsc::unbounded_channel::<UiMsg>();
    let (job_tx, mut job_rx) = mpsc::unbounded_channel::<Job>();

    // Agent olaylarını UI'a aktar
    let (ev_tx, mut ev_rx) = mpsc::unbounded_channel::<AgentEvent>();
    {
        let ui_tx = ui_tx.clone();
        tokio::spawn(async move {
            while let Some(e) = ev_rx.recv().await {
                let _ = ui_tx.send(UiMsg::Agent(e));
            }
        });
    }

    // Ana agent oturumunu çalıştıran görev
    {
        let rt = rt.clone();
        let ui_tx = ui_tx.clone();
        tokio::spawn(async move {
            let mut session = rt.main_session(Some(ev_tx));
            while let Some(job) = job_rx.recv().await {
                match job {
                    Job::Reset => session.reset(),
                    Job::SetModel(m) => session.model = m,
                    Job::Compact => {
                        if let Err(e) = session.compact().await {
                            let _ = ui_tx.send(UiMsg::Error(format!("sıkıştırılamadı: {e:#}")));
                        }
                        let _ = ui_tx.send(UiMsg::TurnFinished);
                    }
                    Job::Resume(id) => {
                        let loaded = match &id {
                            Some(i) => crate::sessions::load(i),
                            None => crate::sessions::latest(),
                        };
                        match loaded {
                            Ok(saved) => {
                                let info = format!("Oturuma devam ediliyor: {} — \"{}\" ({} mesaj, model {})",
                                    saved.id, saved.title, saved.messages.len(), saved.model);
                                let last = saved.messages.iter().rev()
                                    .find(|m| m["role"] == "assistant")
                                    .map(|m| crate::llm::text_of(m["content"].as_array().map(|v| v.as_slice()).unwrap_or(&[])))
                                    .unwrap_or_default();
                                let model = saved.model.clone();
                                session.resume(saved);
                                let _ = ui_tx.send(UiMsg::Info(info));
                                let _ = ui_tx.send(UiMsg::Model(model));
                                if !last.is_empty() {
                                    let _ = ui_tx.send(UiMsg::Info(format!("Son yanıt:\n{last}")));
                                }
                            }
                            Err(e) => { let _ = ui_tx.send(UiMsg::Error(format!("{e:#}"))); }
                        }
                    }
                    Job::Prompt(p) => {
                        if let Err(e) = session.send(&p).await {
                            let _ = ui_tx.send(UiMsg::Error(format!("{e:#}")));
                        }
                        let _ = ui_tx.send(UiMsg::TurnFinished);
                    }
                }
            }
        });
    }

    let mut app = App {
        entries: vec![],
        input: String::new(),
        cursor: 0,
        busy: false,
        scroll_up: 0,
        tokens_in: 0,
        tokens_out: 0,
        spinner: 0,
        history: vec![],
        hist_idx: None,
        model: rt.cfg.model.clone(),
        approvals: Default::default(),
    };
    if let Some(id) = resume {
        let _ = job_tx.send(Job::Resume(id));
    }
    let n_agents = crate::agent::load_agents().len();
    let n_skills = crate::skills::load_all().len();
    app.push(Kind::Info, "", format!(
        "RustyAgent v{} — model: {} — {} agent, {} skill, {} araç\n/help ile komutları gör. Ctrl+C: çıkış, Esc: girdiyi temizle, PgUp/PgDn: kaydır.",
        env!("CARGO_PKG_VERSION"), rt.cfg.model, n_agents, n_skills, rt.base_tools.names().len()
    ));
    for e in &rt.mcp_errors {
        app.push(Kind::Error, "", format!("MCP başlatılamadı: {e}"));
    }

    let mut terminal = ratatui::init();
    let result: Result<()> = async {
        loop {
            while let Ok(m) = ui_rx.try_recv() {
                handle_msg(&mut app, m);
            }
            app.spinner = app.spinner.wrapping_add(1);
            terminal.draw(|f| draw(f, &app, &rt))?;

            if event::poll(Duration::from_millis(40))? {
                let Event::Key(k) = event::read()? else { continue };
                if k.kind != KeyEventKind::Press {
                    continue;
                }
                // Onay penceresi açıksa tuşlar ona gider
                if let Some(h) = app.approvals.front().cloned() {
                    let danger = h.req.risk == Risk::Danger;
                    let decision = match k.code {
                        KeyCode::Char('e') | KeyCode::Char('y') | KeyCode::Enter => Some(Decision::Allow),
                        KeyCode::Char('o') | KeyCode::Char('s') if !danger => Some(Decision::AllowSession),
                        KeyCode::Char('k') | KeyCode::Char('a') if !danger => Some(Decision::AllowAlways),
                        KeyCode::Char('h') | KeyCode::Char('n') | KeyCode::Esc => Some(Decision::Deny),
                        KeyCode::Char('c') if k.modifiers == KeyModifiers::CONTROL => {
                            h.respond(Decision::Deny);
                            break;
                        }
                        _ => None,
                    };
                    if let Some(d) = decision {
                        h.respond(d);
                        app.approvals.pop_front();
                        let (mark, word) = match d {
                            Decision::Allow => ("✓", "onaylandı".to_string()),
                            Decision::AllowSession => ("✓", format!("bu oturum boyunca izin verildi ({})", h.req.grant_key)),
                            Decision::AllowAlways => ("✓", format!("kalıcı kural eklendi: allow \"{}\"", h.req.grant_key)),
                            Decision::Deny => ("✗", "reddedildi".to_string()),
                        };
                        app.push(Kind::Info, &h.req.agent, format!("{mark} {word}: {}", h.req.summary));
                    }
                    continue;
                }
                match (k.code, k.modifiers) {
                    (KeyCode::Char('c'), KeyModifiers::CONTROL) | (KeyCode::Char('d'), KeyModifiers::CONTROL) => break,
                    (KeyCode::Esc, _) => { app.input.clear(); app.cursor = 0; }
                    (KeyCode::PageUp, _) => app.scroll_up = app.scroll_up.saturating_add(10),
                    (KeyCode::PageDown, _) => app.scroll_up = app.scroll_up.saturating_sub(10),
                    (KeyCode::Up, _) if !app.history.is_empty() => {
                        let i = app.hist_idx.map(|i| i.saturating_sub(1)).unwrap_or(app.history.len() - 1);
                        app.hist_idx = Some(i);
                        app.input = app.history[i].clone();
                        app.cursor = app.input.chars().count();
                    }
                    (KeyCode::Down, _) => {
                        if let Some(i) = app.hist_idx {
                            if i + 1 < app.history.len() {
                                app.hist_idx = Some(i + 1);
                                app.input = app.history[i + 1].clone();
                            } else {
                                app.hist_idx = None;
                                app.input.clear();
                            }
                            app.cursor = app.input.chars().count();
                        }
                    }
                    (KeyCode::Left, _) => app.cursor = app.cursor.saturating_sub(1),
                    (KeyCode::Right, _) => app.cursor = (app.cursor + 1).min(app.input.chars().count()),
                    (KeyCode::Home, _) => app.cursor = 0,
                    (KeyCode::End, _) => app.cursor = app.input.chars().count(),
                    (KeyCode::Backspace, _) => {
                        if app.cursor > 0 {
                            let idx = byte_idx(&app.input, app.cursor - 1);
                            app.input.remove(idx);
                            app.cursor -= 1;
                        }
                    }
                    (KeyCode::Enter, KeyModifiers::SHIFT) | (KeyCode::Enter, KeyModifiers::ALT) => {
                        let idx = byte_idx(&app.input, app.cursor);
                        app.input.insert(idx, '\n');
                        app.cursor += 1;
                    }
                    (KeyCode::Enter, _) => {
                        let text = app.input.trim().to_string();
                        app.input.clear();
                        app.cursor = 0;
                        app.hist_idx = None;
                        if text.is_empty() {
                            continue;
                        }
                        app.history.push(text.clone());
                        if let Some(cmd) = text.strip_prefix('/') {
                            let args = commands::split_args(cmd);
                            match args.first().map(|s| s.as_str()) {
                                Some("quit") | Some("exit") => break,
                                Some("reset") | Some("clear") | Some("new") => {
                                    let _ = job_tx.send(Job::Reset);
                                    app.entries.clear();
                                    app.push(Kind::Info, "", "Yeni sohbet başladı (eskisi /resume ile açılabilir).".into());
                                }
                                Some("compact") if !app.busy => {
                                    app.busy = true;
                                    let _ = job_tx.send(Job::Compact);
                                }
                                Some("resume") if args.len() >= 2 && !app.busy => {
                                    app.entries.clear();
                                    let _ = job_tx.send(Job::Resume(Some(args[1].clone())));
                                }
                                Some("continue") if !app.busy => {
                                    app.entries.clear();
                                    let _ = job_tx.send(Job::Resume(None));
                                }
                                Some("mode") if args.len() == 2 => {
                                    match crate::permissions::parse_mode(&args[1]) {
                                        Some(m) => {
                                            rt.perms.write().unwrap().mode = m;
                                            let _ = crate::permissions::set_mode(m);
                                            app.push(Kind::Info, "", format!("İzin modu: {} (kaydedildi)", args[1]));
                                        }
                                        None => app.push(Kind::Error, "", "Modlar: ask | auto | yolo | readonly".into()),
                                    }
                                }
                                _ => {
                                    app.push(Kind::User, "", text.clone());
                                    if args.len() == 2 && args[0] == "model" {
                                        let _ = job_tx.send(Job::SetModel(args[1].clone()));
                                        app.model = args[1].clone();
                                    }
                                    let cfg = rt.cfg.clone();
                                    let ui_tx = ui_tx.clone();
                                    tokio::spawn(async move {
                                        let p = ui_tx.clone();
                                        let progress = move |s: String| { let _ = p.send(UiMsg::Info(s)); };
                                        let _ = ui_tx.send(match commands::run(&cfg, &args, &progress).await {
                                            Ok(s) => UiMsg::Info(s),
                                            Err(e) => UiMsg::Error(format!("{e:#}")),
                                        });
                                    });
                                }
                            }
                        } else if app.busy {
                            app.push(Kind::Info, "", "Agent hâlâ çalışıyor; bitince tekrar gönder.".into());
                            app.input = text;
                            app.cursor = app.input.chars().count();
                        } else {
                            app.push(Kind::User, "", text.clone());
                            app.busy = true;
                            let _ = job_tx.send(Job::Prompt(text));
                        }
                    }
                    (KeyCode::Char(c), _) => {
                        let idx = byte_idx(&app.input, app.cursor);
                        app.input.insert(idx, c);
                        app.cursor += 1;
                    }
                    _ => {}
                }
            }
        }
        Ok(())
    }
    .await;
    ratatui::restore();
    result
}

fn byte_idx(s: &str, char_idx: usize) -> usize {
    s.char_indices().nth(char_idx).map(|(i, _)| i).unwrap_or(s.len())
}

fn handle_msg(app: &mut App, m: UiMsg) {
    match m {
        UiMsg::Info(s) => app.push(Kind::Info, "", s),
        UiMsg::Error(s) => app.push(Kind::Error, "", s),
        UiMsg::TurnFinished => app.busy = false,
        UiMsg::Model(m) => app.model = m,
        UiMsg::Agent(e) => match e.kind {
            EventKind::Text(t) => app.append(Kind::Text, &e.agent, &t),
            EventKind::Thinking(t) => app.append(Kind::Thinking, &e.agent, &t),
            EventKind::ToolCall { name, input } => app.push(Kind::Tool, &e.agent, format!("{name}({input})")),
            EventKind::ToolResult { name, summary, is_error } => app.push(
                if is_error { Kind::ToolErr } else { Kind::ToolOk }, &e.agent, format!("{name} → {summary}")),
            EventKind::TurnDone { input_tokens, output_tokens } => {
                app.tokens_in += input_tokens;
                app.tokens_out += output_tokens;
            }
            EventKind::Error(s) => app.push(Kind::Error, &e.agent, s),
            EventKind::Info(s) => app.push(Kind::Info, &e.agent, s),
            EventKind::Status(s) => app.push(Kind::Info, &e.agent, s),
            EventKind::ModelSwitched(m) => {
                if e.agent == MAIN_AGENT {
                    app.model = m;
                }
            }
            EventKind::Approval(h) => {
                app.approvals.push_back(h);
                app.scroll_up = 0;
            }
        },
    }
}

fn draw(f: &mut ratatui::Frame, app: &App, rt: &Runtime) {
    let input_lines = app.input.lines().count().max(1) as u16 + if app.input.ends_with('\n') { 1 } else { 0 };
    let approval_h = if app.approvals.is_empty() { 0 } else { 6 };
    let [body, approval_area, input_area, status] = Layout::vertical([
        Constraint::Min(3),
        Constraint::Length(approval_h),
        Constraint::Length(input_lines.min(8) + 2),
        Constraint::Length(1),
    ])
    .areas(f.area());

    // ----- onay penceresi
    if let Some(h) = app.approvals.front() {
        let danger = h.req.risk == Risk::Danger;
        let color = if danger { Color::Red } else { Color::Yellow };
        let more = if app.approvals.len() > 1 { format!(" (+{} bekleyen)", app.approvals.len() - 1) } else { String::new() };
        let keys = if danger {
            "[e] evet, bir kez   [h] hayır"
        } else {
            "[e] evet   [o] bu oturumda hep   [k] her zaman (kural kaydet)   [h] hayır"
        };
        let lines = vec![
            Line::from(vec![
                Span::styled(format!("[{}] ", h.req.agent), Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD)),
                Span::raw("şunu yapmak istiyor:"),
            ]),
            Line::from(Span::styled(h.req.summary.clone(), Style::default().add_modifier(Modifier::BOLD))),
            Line::raw(""),
            Line::from(Span::styled(keys, Style::default().fg(color))),
        ];
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(color).add_modifier(Modifier::BOLD))
            .title(format!(" ⚠ Onay gerekiyor — {}{more} ", h.req.risk.label()));
        f.render_widget(Paragraph::new(lines).block(block).wrap(Wrap { trim: true }), approval_area);
    }

    // ----- konuşma dökümü
    let mut lines: Vec<Line> = Vec::new();
    for e in &app.entries {
        let tag = if e.agent.is_empty() || e.agent == MAIN_AGENT { String::new() } else { format!("[{}] ", e.agent) };
        let tag_span = Span::styled(tag, Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD));
        let (prefix, style) = match e.kind {
            Kind::User => ("> ", Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
            Kind::Text => ("● ", Style::default()),
            Kind::Thinking => ("✻ ", Style::default().fg(Color::DarkGray).add_modifier(Modifier::ITALIC)),
            Kind::Tool => ("⏺ ", Style::default().fg(Color::Cyan)),
            Kind::ToolOk => ("  ⎿ ", Style::default().fg(Color::DarkGray)),
            Kind::ToolErr => ("  ⎿ ", Style::default().fg(Color::Red)),
            Kind::Info => ("ℹ ", Style::default().fg(Color::Yellow)),
            Kind::Error => ("✗ ", Style::default().fg(Color::Red)),
        };
        for (i, l) in e.text.lines().enumerate() {
            let p = if i == 0 { prefix.to_string() } else { " ".repeat(prefix.chars().count()) };
            let mut spans = vec![Span::styled(p, style)];
            if i == 0 {
                spans.push(tag_span.clone());
            }
            spans.push(Span::styled(l.to_string(), style));
            lines.push(Line::from(spans));
        }
        lines.push(Line::raw(""));
    }
    // Alt kısma kaydırma için sarılmış satır sayısını tahmin et
    let width = body.width.max(1) as usize;
    let total: usize = lines.iter().map(|l| (l.width().max(1) + width - 1) / width).sum();
    let visible = body.height as usize;
    let max_scroll = total.saturating_sub(visible);
    let scroll = max_scroll.saturating_sub(app.scroll_up as usize) as u16;
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }).scroll((scroll, 0)), body);

    // ----- girdi kutusu
    let spin = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"][app.spinner / 2 % 10];
    let title = if app.busy { format!(" {spin} çalışıyor… ") } else { " mesaj (Enter gönder, Shift+Enter yeni satır) ".into() };
    let block = Block::default().borders(Borders::ALL).border_style(Style::default().fg(if app.busy { Color::Yellow } else { Color::Cyan })).title(title);
    f.render_widget(Paragraph::new(app.input.as_str()).block(block).wrap(Wrap { trim: false }), input_area);
    // imleç
    let before: String = app.input.chars().take(app.cursor).collect();
    let row = before.matches('\n').count() as u16;
    let col = before.rsplit('\n').next().map(|s| s.chars().count()).unwrap_or(0) as u16;
    f.set_cursor_position((input_area.x + 1 + col, input_area.y + 1 + row));

    // ----- durum satırı
    let mode = format!("{:?}", rt.perms.read().unwrap().mode).to_lowercase();
    let st = format!(
        " {} · effort {} · izin: {mode} · ↑{} ↓{} token · {}",
        app.model, rt.cfg.effort, app.tokens_in, app.tokens_out,
        std::env::current_dir().map(|p| p.display().to_string()).unwrap_or_default()
    );
    f.render_widget(Paragraph::new(st).style(Style::default().fg(Color::DarkGray)), status);
}
