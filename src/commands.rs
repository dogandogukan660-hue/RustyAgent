//! Slash komutları — hem TUI (/mcp browse x) hem CLI (rusty-agent mcp browse x) kullanır.

use crate::{agent, automation, config::Config, mcp, models, plugins, skills};
use anyhow::{bail, Result};

pub const HELP: &str = "\
Komutlar:
  /model                           aktif model + sağlayıcı durumu
  /model <sağlayıcı/model>         modeli değiştir (ör. /model openrouter/openai/gpt-5, /model ollama/llama3.2)
  /models <sağlayıcı> [filtre]     sağlayıcının modelleri (anthropic, openai, gemini, openrouter, ollama, lmstudio)
  /login openrouter                tarayıcıdan giriş → GPT, Gemini, Claude, Llama, DeepSeek... (anahtar kopyalamadan)
  /login <sağlayıcı>               gemini, anthropic, openai, groq, xai, deepseek, mistral: tarayıcıda anahtar sayfası açılır,
                                   kopyaladığın anahtar otomatik yakalanır, doğrulanır ve kaydedilir
  /logout <sağlayıcı>
  /hf browse <arama>               Hugging Face'te açık kaynak model ara (GGUF, yerelde çalışır)
  /hf quants <repo>                modelin boyut/niceleme seçenekleri
  /hf pull <repo> [niceleme]       Ollama ile indir (ör. /hf pull bartowski/Qwen2.5-7B-Instruct-GGUF Q4_K_M)
  /hf remove <ollama-model>        indirilen modeli sil
  /agents                          agent listesi
  /agent new <ad> <açıklama>       yeni agent dosyası oluştur (sonra düzenle)
  /mcp                             bağlı MCP sunucuları
  /mcp tools                       sunuculara bağlanıp araçlarını listele (bağlantı testi)
  /mcp browse <arama>              resmi MCP kayıt defterinde ara
  /mcp add <ad> <komut> [arg...]   MCP sunucusu ekle (ör. /mcp add fs npx -y @modelcontextprotocol/server-filesystem C:/)
  /mcp add-http <ad> <url> [\"Başlık: değer\"]   uzak MCP sunucusu ekle (Streamable HTTP)
  /mcp install <kayıt-adı>         kayıt defterinden bulup otomatik ekle
  /mcp remove <ad>
  /skills                          kurulu skill'ler
  /skill browse [arama]            skill kütüphanesinde ara (anthropics/skills)
  /skill install <kaynak>          ör. /skill install anthropics/skills/skills/pdf
  /skill remove <ad>
  /plugins                         kurulu plugin'ler
  /plugin browse [arama]           plugin pazarlarında ara
  /plugin install <ad>
  /plugin remove <ad>
  /automations                     zamanlanmış görevler
  /automation add <ad> \"<cron>\" <prompt>   ör. /automation add rapor \"0 0 9 * * *\" Günlük rapor hazırla
  /automation remove <ad>
  /mode [ask|auto|yolo|readonly]   izin modu (onay sistemi)
  /sessions                        kayıtlı oturumlar
  /resume <id>  ·  /continue       eski oturuma / en sonuncusuna devam et
  /compact                         konuşmayı şimdi özetle (bağlamı küçült)
  /new                             yeni sohbet (eskisi kayıtlı kalır)
  /quit                            çıkış
(MCP/plugin değişiklikleri yeniden başlatınca etkin olur.)";

/// Tırnakları dikkate alarak argümanlara böler
pub fn split_args(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    for c in s.chars() {
        match c {
            '"' => quoted = !quoted,
            ' ' if !quoted => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            _ => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

pub async fn run(cfg: &Config, args: &[String], progress: &(dyn Fn(String) + Send + Sync)) -> Result<String> {
    let a: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    let rest = |i: usize| a.get(i..).map(|r| r.join(" ")).unwrap_or_default();
    Ok(match a.as_slice() {
        ["help"] | [] => HELP.to_string(),

        ["sessions"] | ["resume"] => {
            let list = crate::sessions::list()?;
            if list.is_empty() { "Kayıtlı oturum yok.".into() } else {
                list.iter().take(20).map(|s| format!("• {}  {}  ({} mesaj, {})", s.id, s.title, s.messages.len(),
                    s.updated.get(..16).unwrap_or(&s.updated).replace('T', " "))).collect::<Vec<_>>().join("\n")
                    + "\n\nDevam: /resume <id>  ·  en sonuncusu: /continue"
            }
        }
        ["compact"] | ["continue"] | ["resume", _] => "Agent çalışırken yapılamaz; bitince tekrar dene.".into(),
        ["mode"] | ["permissions"] => {
            let c = Config::load()?;
            let p = &c.permissions;
            format!(
                "İzin modu: {:?}\n  ask = değişiklik/tehlikeli işlemlerde sor · auto = yalnızca tehlikelide sor · yolo = hiç sorma · readonly = sadece okuma\n\
                 İzinli kurallar: {}\nYasak kurallar: {}\nDeğiştir: /mode <ask|auto|yolo|readonly>  ·  kurallar: ~/.rustyagent/config.toml [permissions]",
                p.mode,
                if p.allow.is_empty() { "-".into() } else { p.allow.join(", ") },
                if p.deny.is_empty() { "-".into() } else { p.deny.join(", ") }
            )
        }
        ["mode", m] => {
            let mode = crate::permissions::parse_mode(m).ok_or_else(|| anyhow::anyhow!("Modlar: ask | auto | yolo | readonly"))?;
            crate::permissions::set_mode(mode)?;
            format!("İzin modu: {m}")
        }
        ["model"] | ["providers"] => models::status()?,
        ["model", m] => {
            models::set_default(m)?;
            format!("Model: {m} (varsayılan olarak kaydedildi)")
        }
        ["models", provider, ..] => {
            let list = models::list(provider, &rest(2)).await?;
            if list.is_empty() { "Model bulunamadı.".into() } else {
                let n = list.len();
                let mut s = list.into_iter().take(80).collect::<Vec<_>>().join("\n");
                if n > 80 { s.push_str(&format!("\n… toplam {n} model; filtre ekle: /models {provider} <arama>")); }
                s
            }
        }
        ["login", "openrouter"] => {
            progress("Tarayıcı açılıyor; OpenRouter'a giriş yapıp izin ver…".into());
            let msg = models::login_openrouter().await?;
            if !models::model_ready(&Config::load()?.model) {
                models::set_default(models::recommended("openrouter"))?;
            }
            msg
        }
        ["login", other] => {
            if models::key_site(other).is_none() {
                bail!(
                    "'{other}' tanınmadı. Seçenekler: openrouter, {}",
                    models::KEY_SITES.iter().map(|k| k.provider).collect::<Vec<_>>().join(", ")
                );
            }
            let msg = models::connect_with_browser(other, progress).await?;
            if !models::model_ready(&Config::load()?.model) {
                models::set_default(models::recommended(other))?;
                format!("{msg}\nVarsayılan model: {}", models::recommended(other))
            } else {
                msg
            }
        }
        ["logout", p] => { models::logout(p)?; format!("{p} anahtarı silindi.") }
        ["hf", "browse", ..] => {
            let list = models::hf_browse(&rest(2)).await?;
            if list.is_empty() { "Sonuç yok.".into() } else {
                list.iter().map(|m| format!("• {}  ({} indirme · {} beğeni)", m.id, m.downloads, m.likes)).collect::<Vec<_>>().join("\n")
                    + "\n\nİndir: /hf pull <repo> [niceleme]   ·   Seçenekler: /hf quants <repo>\nİpucu: küçük RAM için 1-8B modeller + Q4_K_M seç."
            }
        }
        ["hf", "quants", repo] => {
            let q = models::hf_quants(repo).await?;
            if q.is_empty() { "GGUF dosyası bulunamadı.".into() } else { format!("{repo}: {}", q.join(", ")) }
        }
        ["hf", "pull", repo, ..] => {
            let name = models::hf_pull(repo, a.get(3).copied(), progress).await?;
            format!("İndirildi. Kullanmak için: /model {name}")
        }
        ["hf", "remove", name] => { models::ollama_remove(name).await?; "Silindi.".into() }

        ["agents"] => {
            let list = agent::load_agents();
            if list.is_empty() {
                "Tanımlı agent yok. /agent new ile oluştur ya da ana agent'tan iste.".into()
            } else {
                list.iter().map(|x| format!("• {} — {}\n    {}", x.name, x.description, x.source)).collect::<Vec<_>>().join("\n")
            }
        }
        ["agent", "new", name, ..] => {
            let p = agent::create_agent_file(name, &rest(3), &[], "Sen bu alanda uzman bir agent'sın. Görevi eksiksiz yap ve sonucu raporla.")?;
            format!("Oluşturuldu: {} (system prompt'u düzenleyebilirsin)", p.display())
        }

        ["mcp"] => {
            let all = mcp::load_all();
            if all.is_empty() { "MCP sunucusu yok. /mcp browse <arama> ile bul.".into() } else {
                all.iter().map(|(n, c)| format!("• {n}: {}", c.describe())).collect::<Vec<_>>().join("\n")
            }
        }
        ["mcp", "tools"] => {
            let hub = mcp::McpHub::start_all().await;
            let mut out = Vec::new();
            for c in &hub.clients {
                match c.list_tools().await {
                    Ok(t) => out.push(format!("• {} ({} araç): {}", c.name, t.len(),
                        t.iter().filter_map(|x| x["name"].as_str()).collect::<Vec<_>>().join(", "))),
                    Err(e) => out.push(format!("✗ {}: {e:#}", c.name)),
                }
            }
            for e in &hub.errors {
                out.push(format!("✗ {e}"));
            }
            if out.is_empty() { "MCP sunucusu yok.".into() } else { out.join("\n") }
        }
        ["mcp", "browse", ..] => {
            let list = mcp::browse(&rest(2)).await?;
            if list.is_empty() { return Ok("Sonuç yok.".into()); }
            list.iter().map(|e| {
                let inst = e.install.as_ref().map(|c| c.describe()).unwrap_or_else(|| "(manuel kurulum)".into());
                format!("• {}\n    {}\n    kurulum: {}", e.name, e.description, inst)
            }).collect::<Vec<_>>().join("\n")
        }
        ["mcp", "install", name] => {
            let list = mcp::browse(name).await?;
            let Some(e) = list.into_iter().find(|e| e.name == *name || e.name.ends_with(&format!("/{name}"))) else {
                bail!("'{name}' bulunamadı; tam adı /mcp browse ile bak");
            };
            let Some(cfg) = e.install else { bail!("Bu sunucu için otomatik kurulum yok") };
            let short = e.name.rsplit('/').next().unwrap_or(&e.name).to_string();
            mcp::add_server(&short, cfg)?;
            format!("'{short}' eklendi. Gerekiyorsa ~/.rustyagent/mcp.json içinde env (API anahtarı vb.) ekle.")
        }
        ["mcp", "add-http", name, url, ..] => {
            // Ek başlıklar: "Authorization: Bearer xyz" biçiminde
            let mut headers = std::collections::BTreeMap::new();
            for h in &a[4..] {
                if let Some((k, v)) = h.split_once(':') {
                    headers.insert(k.trim().to_string(), v.trim().to_string());
                }
            }
            mcp::add_server(name, mcp::ServerConfig { kind: Some("http".into()), url: Some(url.to_string()), headers, ..Default::default() })?;
            format!("Uzak MCP '{name}' eklendi ({url}). Yeniden başlatınca bağlanır.")
        }
        ["mcp", "add", name, command, ..] => {
            mcp::add_server(name, mcp::ServerConfig {
                command: command.to_string(),
                args: a[4..].iter().map(|s| s.to_string()).collect(),
                ..Default::default()
            })?;
            format!("MCP '{name}' eklendi.")
        }
        ["mcp", "remove", name] => if mcp::remove_server(name)? { "Silindi.".into() } else { "Bulunamadı.".into() },

        ["skills"] => {
            let list = skills::load_all();
            if list.is_empty() { "Skill yok. /skill browse ile kütüphaneye bak.".into() } else {
                list.iter().map(|s| format!("• {} — {}", s.name, s.description)).collect::<Vec<_>>().join("\n")
            }
        }
        ["skill", "browse", ..] => {
            let list = skills::browse(&cfg.skill_sources, &rest(2)).await?;
            if list.is_empty() { "Sonuç yok.".into() } else {
                list.iter().map(|s| format!("• {}   (/skill install {})", s.name, s.source)).collect::<Vec<_>>().join("\n")
            }
        }
        ["skill", "install", src] => {
            let src = if src.contains('/') { src.to_string() } else {
                // Kısa ad verildiyse kaynaklarda ara
                let list = skills::browse(&cfg.skill_sources, src).await?;
                list.into_iter().find(|s| s.name == *src).map(|s| s.source)
                    .ok_or_else(|| anyhow::anyhow!("'{src}' bulunamadı"))?
            };
            format!("Kuruldu: {}", skills::install(&src).await?.display())
        }
        ["skill", "remove", name] => { skills::remove(name)?; "Silindi.".into() }

        ["plugins"] => {
            let list = plugins::installed_dirs();
            if list.is_empty() { "Plugin yok. /plugin browse ile pazara bak.".into() } else {
                list.iter().map(|p| format!("• {}", p.file_name().unwrap().to_string_lossy())).collect::<Vec<_>>().join("\n")
            }
        }
        ["plugin", "browse", ..] => {
            let list = plugins::browse(&cfg.plugin_marketplaces, &rest(2)).await?;
            if list.is_empty() { "Sonuç yok.".into() } else {
                list.iter().map(|p| format!("• {} [{}]\n    {}", p.name, p.marketplace, p.description)).collect::<Vec<_>>().join("\n")
            }
        }
        ["plugin", "install", name] => format!("Kuruldu: {} (yeniden başlatınca etkin)", plugins::install(&cfg.plugin_marketplaces, name).await?.display()),
        ["plugin", "remove", name] => { plugins::remove(name)?; "Silindi.".into() }

        ["automations"] => {
            let list = automation::load();
            if list.is_empty() { "Otomasyon yok.".into() } else {
                list.iter().map(|x| format!("• {} [{}] {} → {}{}", x.name, x.schedule, x.agent, x.prompt,
                    if x.enabled { "" } else { " (kapalı)" })).collect::<Vec<_>>().join("\n")
            }
        }
        ["automation", "add", name, schedule, ..] => {
            automation::add(automation::Automation {
                name: name.to_string(),
                schedule: schedule.to_string(),
                prompt: rest(4),
                agent: agent::MAIN_AGENT.into(),
                notify: true,
                enabled: true,
            })?;
            "Otomasyon eklendi (serve modunda çalışır).".into()
        }
        ["automation", "remove", name] => if automation::remove(name)? { "Silindi.".into() } else { "Bulunamadı.".into() },

        _ => bail!("Bilinmeyen komut. /help yaz."),
    })
}
