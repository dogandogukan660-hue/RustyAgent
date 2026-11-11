# RustyAgent

Rust ile yazılmış, Windows / macOS / Linux'ta çalışan, Claude Code benzeri arayüze sahip
çok-agent'lı kişisel AI asistanı. Bir **ana agent** tüm alt agent'ları yönetir; terminalden,
Telegram'dan ve (Aşama 2'de) telefonla sesli konuşarak iş yaptırılabilir.

## Hafıza (second brain)

Tüm sohbetler `~/.rustyagent/sessions/` içinde saklanır; kalıcı bilgiler ise `~/.rustyagent/brain/` altında
Obsidian uyumlu Markdown notlar olarak durur (`[[bağlantılar]]`, etiketler, geri bağlantılar).

- Agent'lar seninle ilgili kalıcı bilgileri (tercihler, kişisel bilgiler, projeler, kararlar) `memory_save` ile kendiliğinden kaydeder.
- Her yeni sohbetin başında profil notların ve mesajınla ilgili notlar modele otomatik verilir.
- Geçmişe dair sorularda agent `memory_search` ve `history_search` (tüm sohbetlerde tam metin arama) kullanır.
- Yeni sohbete geçince bitmiş sohbet arka planda özetlenip "Sohbet özeti" notu olur; sıkıştırma özetleri de kaydedilir.
- Arama Türkçe karakter duyarsızdır ("ogrenci" → "öğrenci").
- Uygulamada **Hafıza** sayfası: notları ara/filtrele/düzenle/sil, bağlantılar arasında gez, sohbet geçmişinde ara ve sohbeti aç.

## Masaüstü uygulaması

İki program derlenir:
- **`RustyAgent.exe`** — masaüstü uygulaması; çift tıklayarak aç. Windows'ta arkada konsol (cmd) penceresi açmaz.
- **`rusty-agent.exe`** — komut satırı (`run`, `tui`, `serve`, `mcp`, `skill`, `plugin`…); argümansız çalıştırınca da uygulamayı açar.

Agent'ların çalıştırdığı komutlar, MCP sunucuları ve imleç katmanı da pencere açmadan arka planda çalışır.

- **Tasarım**: sıcak nötr palet (koyu/açık), sistem yazı tipleri (yanıtlar serif), çizgi ikonlar, emoji yok.
  Kenar çubuğunda yeni sohbet, sayfalar ve tarihe göre gruplanmış son sohbetler (arama, silme).
- **Mesaj kutusu**: dosya/görsel/PDF ekleme (sürükle-bırak da olur), model seçici, gönder/durdur.
  Yanıtlarda Markdown, kopyalanabilir kod blokları, mesaj kopyalama ve yeniden oluşturma.
  Kısayollar: Ctrl+N yeni sohbet, Esc durdur, Ctrl+B kenar çubuğu.
- **Sohbet**: sağda (gizlenebilir) "ekip" paneli — her agent'ın animasyonlu karakteri durumunu gösterir
  (boşta göz kırpar, düşünür, çalışır, konuşur, onay bekler, sevinir, uzun süre boşta kalınca uyur);
  konuşurken konuşma balonu çıkar. Karaktere tıklayınca o agent'a iş verme cümlesi hazırlanır.
  Onay istekleri karakterle birlikte açılır pencerede gelir. `/` komutlarının hepsi burada da çalışır.
- **Agent'lar**: karakter (robot, baykuş, tilki, kedi, jöle, hayalet), renk, kişilik, model, araçlar,
  skill'ler ve talimatlar düzenlenir; canlı önizleme. Ana agent'ın (Rusty) adı/karakteri/kişiliği de buradan.
- **Modeller**: aktif model, sağlayıcı anahtarları, OpenRouter tarayıcı girişi, model listesi,
  Hugging Face arama + indirme (ilerleme göstergeli), Ollama modellerini silme.
- **MCP / Skill'ler / Plugin'ler**: kurulu olanlar, kayıt defteri/pazar araması ve tek tıkla kurulum, bağlantı testi.
- **Otomasyonlar**, **Oturumlar** (devam et / sil), **Ayarlar** (izin modu, yetenekler, Telegram, ses).

Agent dosyasında karakter alanları:
```yaml
character: owl          # robot | owl | fox | cat | slime | ghost
color: "#8b5cf6"
persona: Bilge, sakin ve meraklı bir baykuşsun; az ama öz konuşursun.
```

## Kurulum

```powershell
# Windows: Visual Studio Build Tools (C++) gerekir
cargo build --release
$env:ANTHROPIC_API_KEY = "sk-ant-..."
./target/release/rusty-agent          # terminal arayüzü
```

İlk çalıştırmada `~/.rustyagent/config.toml` oluşturulur:

```toml
model = "claude-opus-5-5"
effort = "high"            # low | medium | high | xhigh | max
max_tokens = 64000
computer_use = false       # true: agent'lar ekranı görüp mouse/klavye kullanabilir
plugin_marketplaces = ["anthropics/claude-plugins-official"]
skill_sources = ["anthropics/skills/skills"]

[telegram]
bot_token = ""             # @BotFather'dan
allowed_chat_ids = []      # bota /start yaz, sana id'ni söyler

[voice]                    # telefonla arama (Twilio)
twilio_account_sid = ""
twilio_auth_token = ""
twilio_phone_number = ""   # Twilio numaran
owner_phone_number = ""    # seni arayacağı numara, +90...
deepgram_api_key = ""      # Aşama 2: gerçek zamanlı konuşma
cartesia_api_key = ""
public_base_url = ""
```

## Hesap bağlama (tarayıcıdan)

Uygulamada **Modeller → Hesap bağla** (veya `/login <sağlayıcı>`): sağlayıcının sayfası tarayıcıda açılır,
kendi hesabınla (Google, GitHub, e-posta) giriş yapıp anahtar oluşturur ve **kopyalarsın**; uygulama anahtarı
panodan otomatik yakalar, sağlayıcıya küçük bir istekle doğrular, kaydeder ve güvenlik için panodan siler.
Mevcut model kullanılamıyorsa o sağlayıcının önerilen modeline geçer.

| Sağlayıcı | Sayfa | Not |
|---|---|---|
| Gemini | aistudio.google.com/apikey | Google hesabıyla, ücretsiz katman |
| OpenRouter | gerçek OAuth girişi | anahtar kopyalamaya gerek yok |
| Claude / OpenAI / xAI (Grok) / DeepSeek / Mistral | ilgili konsol | ücretli |
| Groq | console.groq.com/keys | ücretsiz katman, çok hızlı |

ChatGPT / Claude.ai / Gemini / Antigravity uygulamalarının "hesabınla giriş" sistemleri yalnızca o şirketlerin
kendi uygulamalarına özeldir; burada aynı hesabınla resmi API anahtarı alınır.

## Model dayanıklılığı

Model adları sabit değildir; bağlı sağlayıcının gerçek model listesi çekilir ve sohbet için uygun olmayanlar
(ses, görsel, TTS, gömme…) elenir. Bir istek başarısız olursa agent beklemek yerine yedek modele geçer:

| Durum | Davranış |
|---|---|
| Model kaldırılmış / yeni kullanıcılara kapalı (404) | Sağlayıcının önerdiği modele geçilir; varsayılan model de güncellenir |
| Kota dolmuş (uzun bekleme süreli 429) | Aynı sağlayıcının kotası olan başka modeline geçilir (varsayılan değişmez) |
| Sağlayıcı yoğun (503) veya 20 sn içinde yanıt yok | İki kısa denemeden sonra sıradaki modele geçilir; model 10 dk tekrar denenmez |

Geçişler sohbette bilgi satırı olarak görünür. Gemini 3 modellerinin araç çağrılarında istediği düşünce imzası
(`thought_signature`) saklanıp geri gönderilir.

## Sohbet yönetimi

- **Düzenle / geri sar**: her mesajının altındaki kalem ikonu sohbeti o mesaja geri sarar; metin düzenlenip yeniden gönderilir.
  Üst çubuktaki geri al ikonu son mesajı geri alır.
- **Sil ve geri al**: silinen sohbet çöp kutusuna gider; 8 saniye boyunca "Geri al" çıkar, sonrasında
  Hafıza → Sohbet geçmişi altındaki "Silinen sohbetler"den 30 gün içinde geri getirilebilir.
- **Yeniden adlandır** (başlığa tıkla), **dışa aktar** (Markdown), **yeniden oluştur**, **kopyala**, **durdur** (Esc).

## Modeller (çok sağlayıcılı)

Model adı `sağlayıcı/model` biçimindedir; TUI'de `/model <ad>` ile anında değişir.

| Sağlayıcı | Nasıl bağlanır | Örnek |
|---|---|---|
| `openrouter` | **`/login openrouter`** → tarayıcıda giriş, anahtar otomatik alınır | `openrouter/openai/gpt-5`, `openrouter/google/gemini-2.5-pro`, `openrouter/deepseek/deepseek-chat` |
| `ollama` | Yerel, ücretsiz, anahtarsız. `/hf browse qwen` → `/hf pull <repo> Q4_K_M` | `ollama/qwen3:8b`, `ollama/hf.co/Qwen/Qwen2.5-7B-Instruct-GGUF:Q4_K_M` |
| `lmstudio` | Yerel (LM Studio sunucusu açıkken) | `lmstudio/<model>` |
| `anthropic` | `ANTHROPIC_API_KEY` | `anthropic/claude-opus-5-5` |
| `openai` | `OPENAI_API_KEY` | `openai/gpt-5` |
| `gemini` | `GEMINI_API_KEY` (aistudio.google.com'dan ücretsiz) | `gemini/gemini-2.5-flash` |

> ChatGPT Plus / Gemini / Claude.ai **abonelik** girişleri bilerek desteklenmez: bu girişler o şirketlerin
> kendi uygulamalarına özeldir; üçüncü taraf uygulamada kullanmak kullanım koşullarına aykırıdır ve hesabı riske atar.
> OpenRouter girişi bunun meşru karşılığıdır (tek hesapla hepsi).

Araç kullanımı (dosya, shell, MCP...) için modelin "tool calling" desteklemesi gerekir. Yerelde iyi sonuç için
en az 7-8B'lik modeller önerilir (Qwen2.5/Qwen3, Llama 3.1+, Mistral); 0.5-1B modeller araçları sık hatalı kullanır.

## Sesli konuşma (telefon + Telegram)

```
Telefon ⇄ Twilio ⇄ (WebSocket) ⇄ RustyAgent ses sunucusu
                                  ├─ Deepgram  : akışlı konuşma→yazı (ara sonuçlar, uç nokta algılama)
                                  ├─ Ana agent : yanıt akış halinde
                                  └─ Cartesia  : cümle cümle yazı→ses (doğrudan μ-law 8 kHz)
```
- İlk cümle biter bitmez seslendirilir; konuşurken araya girersen agent **anında susar** (barge-in).
- Agent araç kullanırken konuşursan söz sıraya alınır, iş bitince ona geçer.
- Gelen aramalar **yalnızca** `owner_phone_number`'dan kabul edilir; ses akışı gizli anahtarla doğrulanır.

Kurulum:
1. Twilio hesabı + telefon numarası; Deepgram ve Cartesia API anahtarları (config.toml `[voice]`).
   `cartesia_voice_id` için play.cartesia.ai'den Türkçe bir ses seç.
2. Public adres: `cloudflared tunnel --url http://localhost:8787` → çıkan https adresini `public_base_url` yap.
3. Twilio numarasının *A call comes in* webhook'u: `<public_base_url>/twilio/voice` (HTTP POST).
4. `rusty-agent serve` → numarayı ara ve konuş. Agent de `notify_user(call=true)` ile seni arayıp konuşur.
5. Hız için `[voice] model` alanına hızlı bir model verilebilir (ör. `anthropic/claude-haiku-5-5`).

Telegram'da sesli mesaj gönderirsen yazıya çevrilir, yanıt hem yazılı hem sesli gelir.

## Sanal imleçler ve paralel tarayıcı agent'ları

- Her agent'ın ekranda **kendi renkli, isim etiketli imleci** vardır: nereye gittiği, tıkladığı (dalga efekti)
  ve yazdığı metin görünür. Katman şeffaftır, tıklamaları alttaki pencerelere geçirir; seni engellemez.
- **`browser` aracı**: her agent kendi Chrome/Edge/Brave penceresini alır; tıklama ve yazma Chrome DevTools
  Protocol ile doğrudan sayfaya gider, **gerçek mouse hiç hareket etmez**. Bu yüzden birden çok agent aynı anda
  farklı sitelerde çalışır, sen de bu sırada bilgisayarını kullanmaya devam edersin. Pencereler ızgaraya dizilir.
- Tarayıcı profili kalıcıdır (`~/.rustyagent/browser-profile`): bir siteye bu pencerede bir kez giriş yaparsan agent'lar oturumu kullanır.
- `computer` aracı (masaüstü uygulamaları) gerçek mouse'u kullanır; agent'lar sırayla erişir, o an kimin kontrol ettiği imleçle görünür.

Ayarlar (`config.toml`): `browser = true`, `cursor_overlay = true`, `browser_path = ""` (boşsa otomatik bulunur).

Araçları LLM olmadan denemek için:
```powershell
rusty-agent tool browser '{"action":"navigate","url":"example.com"}' --agent deneme
rusty-agent tool browser @adimlar.json        # {"agent1":[adımlar], "agent2":[adımlar]} → paralel
```

## İzinler ve onay sistemi

Her araç çağrısı çalışmadan önce risk seviyesine göre sınıflanır:

| Seviye | Örnekler | `ask` (varsayılan) | `auto` | `yolo` | `readonly` |
|---|---|---|---|---|---|
| güvenli | dosya okuma, arama, ekran görüntüsü, sayfa açma, `git status` | çalışır | çalışır | çalışır | çalışır |
| değişiklik yapar | dosya yazma, çoğu shell komutu, mouse/klavye, form doldurma, MCP | **sorar** | çalışır | çalışır | reddedilir |
| tehlikeli | `rm -rf`, `Remove-Item -Recurse`, `git push --force`, `.ssh`/`.env` yazma, "Satın Al / Öde / Gönder / Sil" butonları | **her seferinde sorar** | **sorar** | çalışır | reddedilir |

- Onay: terminal arayüzünde pencere (`e` evet · `o` bu oturumda hep · `k` kalıcı kural · `h` hayır),
  Telegram'da butonlar, telefon görüşmesinde sesli uyarı + Telegram butonu, `run` komutunda klavye.
- Arayüz yoksa (otomasyonlar) onay Telegram'dan istenir; o da yoksa işlem reddedilir.
- Mod: `/mode ask|auto|yolo|readonly`. Kurallar `config.toml`:
  ```toml
  [permissions]
  mode = "ask"
  allow = ["shell(git *)", "shell(cargo *)", "browser", "write_file(C:/Projeler/*)"]
  deny  = ["shell(*format*)", "computer"]
  ```

## Oturumlar ve bağlam

- Ana agent konuşmaları `~/.rustyagent/sessions/` içine her turdan sonra kaydedilir.
  `/sessions` liste · `/resume <id>` · `/continue` · komut satırında `rusty-agent -c` veya `--resume <id>`.
- Konuşma `compact_at_tokens` (uzak modeller, varsayılan 150K) veya `compact_at_tokens_local`
  (yerel modeller, 12K) sınırını geçince tur arasında otomatik özetlenir; elle: `/compact`.

## Uzak MCP sunucuları

Yerel (stdio) sunucuların yanında uzak **Streamable HTTP** sunucuları da desteklenir:
```
/mcp add-http deepwiki https://mcp.deepwiki.com/mcp
/mcp add-http github https://api.githubcopilot.com/mcp/ "Authorization: Bearer <token>"
/mcp tools          # bağlan ve araçları listele
```
`/mcp install` kayıt defterinde yalnızca uzak adresi olan sunucuları da kurar.

## Kullanım

| Komut | Ne yapar |
|---|---|
| `rusty-agent` | Terminal arayüzü (TUI) |
| `rusty-agent run "görev"` | Arayüzsüz tek görev |
| `rusty-agent serve` | Arka plan servisi: Telegram botu + otomasyonlar |
| `rusty-agent mcp browse github` | MCP kayıt defterinde ara |
| `rusty-agent mcp install <ad>` | Kayıt defterinden MCP kur |
| `rusty-agent skill browse` | Skill kütüphanesi (anthropics/skills) |
| `rusty-agent skill install pdf` | Skill kur |
| `rusty-agent plugin browse` / `plugin install <ad>` | Plugin pazarı |
| `rusty-agent automation add rapor "0 0 9 * * *" Günlük rapor hazırla` | Zamanlanmış görev |

TUI içinde aynı komutlar `/` ile çalışır (`/help`).

## Dizin yapısı

```
~/.rustyagent/            (veya proje içinde ./.rustyagent/ — öncelikli)
  config.toml
  agents/*.md             agent tanımları (örnekler: examples/agents/)
  skills/<ad>/SKILL.md    Anthropic Agent Skills biçimi
  plugins/<ad>/           agents/, skills/, .mcp.json, automations.toml
  mcp.json                {"mcpServers": {...}}  (Claude Code ile uyumlu)
  automations.toml
```

## Mimari

```
            Terminal (TUI)   Telegram   Telefon (Aşama 2)   Otomasyon (cron)
                   \            |             |                /
                    +-------- Ana agent (orkestratör) --------+
                    |  delegate · create_agent · notify_user  |
                    +------+-----------+-----------+----------+
                           |           |           |      (paralel)
                      arastirmaci  yazilimci  bilgisayar-operatoru ...
                           |           |           |
        araçlar: shell, dosya, grep/glob, web_search, web_fetch, computer,
                 use_skill, mcp__<sunucu>__<araç>
```

- **Claude API**: Rust için resmi SDK olmadığından ham HTTP + SSE streaming (`src/llm.rs`).
  Adaptive thinking, prompt caching, effort ayarı ve sunucu tarafı yedek model (`fallbacks: "default"`) açık.
- **Paralel araçlar**: Bir yanıttaki tüm araç çağrıları (ör. 3 ayrı `delegate`) aynı anda çalışır.
- **Ekran kontrolü**: Tek gerçek imleç olduğundan agent'lar ekranı global kilitle sırayla kullanır.

## Yol haritası

- [x] **Aşama 1 — Çekirdek**: agent döngüsü, ana agent + alt agent'lar, TUI, araçlar, MCP (stdio),
      MCP/skill/plugin browse+install, otomasyonlar, Telegram, tek yönlü telefon bildirimi
- [x] **Aşama 2 — Ses + çok model**: telefonla çift yönlü konuşma (Twilio + Deepgram + Cartesia, barge-in),
      Telegram sesli mesaj; OpenAI uyumlu sağlayıcılar, OpenRouter tarayıcı girişi, Hugging Face → Ollama indirme
- [ ] **Aşama 2b**: bilgisayar mikrofonundan konuşma; tamamen yerel ses (Whisper STT + Piper/Kokoro TTS)
- [x] **Aşama 3 — Sanal imleçler**: agent başına renkli overlay imleç, CDP tabanlı paralel tarayıcı agent'ları,
      pencere döşeme, kalıcı tarayıcı profili
- [x] **Aşama 4**: araç onay sistemi (risk sınıflandırma, 4 mod, kurallar, terminal/Telegram/telefon onayı),
      bağlam sıkıştırma, oturum kaydı/devam ettirme, uzak MCP (Streamable HTTP)
- [x] **Aşama 5**: masaüstü uygulaması (egui) — tüm özellikler + animasyonlu özgün agent karakterleri ve kişilikleri
- [ ] **Sonraki**: uygulama içinden mikrofonla konuşma ve karakterlerin sesli yanıtı, uzak MCP için OAuth girişi
