//! Yanıtlar için hafif Markdown görüntüleyici: başlıklar, listeler, alıntılar, ayırıcılar,
//! **kalın**, *italik*, `satır içi kod`, [bağlantı](url) ve kopyalanabilir kod blokları.

use crate::theme::{self, Icon, Palette};
use eframe::egui::{self, text::LayoutJob, Color32, FontId, Stroke, TextFormat};

struct Style {
    size: f32,
    serif: bool,
}

fn base_font(st: &Style, bold: bool) -> FontId {
    match (st.serif, bold) {
        (true, false) => theme::serif(st.size),
        (true, true) => theme::serif_bold(st.size),
        (false, false) => FontId::proportional(st.size),
        (false, true) => theme::bold(st.size),
    }
}

/// Satır içi biçimlendirmeyi LayoutJob'a çevirir
fn inline(text: &str, st: &Style, p: &Palette, color: Color32) -> LayoutJob {
    let mut job = LayoutJob::default();
    let chars: Vec<char> = text.chars().collect();
    let (mut bold, italic) = (false, false);
    let mut buf = String::new();
    let flush = |job: &mut LayoutJob, buf: &mut String, bold: bool, italic: bool| {
        if !buf.is_empty() {
            job.append(buf, 0.0, TextFormat { font_id: base_font(st, bold), color, italics: italic, ..Default::default() });
            buf.clear();
        }
    };
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        // `kod`
        if c == '`' {
            if let Some(end) = chars[i + 1..].iter().position(|x| *x == '`') {
                flush(&mut job, &mut buf, bold, italic);
                let code: String = chars[i + 1..i + 1 + end].iter().collect();
                job.append(
                    &code,
                    2.0,
                    TextFormat {
                        font_id: FontId::monospace(st.size * 0.86),
                        color: p.text,
                        background: p.code_bg,
                        ..Default::default()
                    },
                );
                job.append(" ", 0.0, TextFormat { font_id: base_font(st, false), ..Default::default() });
                i += end + 2;
                continue;
            }
        }
        // **kalın**
        if c == '*' && chars.get(i + 1) == Some(&'*') {
            flush(&mut job, &mut buf, bold, italic);
            bold = !bold;
            i += 2;
            continue;
        }
        // *italik* (kelime içindeki yıldızları ve liste işaretlerini atla)
        if (c == '*' || c == '_') && chars.get(i + 1).map(|n| !n.is_whitespace()).unwrap_or(false) && (i == 0 || !chars[i - 1].is_alphanumeric()) {
            if let Some(end) = chars[i + 1..].iter().position(|x| *x == c) {
                if end > 0 {
                    flush(&mut job, &mut buf, bold, italic);
                    let t: String = chars[i + 1..i + 1 + end].iter().collect();
                    job.append(&t, 0.0, TextFormat { font_id: base_font(st, bold), color, italics: true, ..Default::default() });
                    i += end + 2;
                    continue;
                }
            }
        }
        // [metin](url)
        if c == '[' {
            if let Some(close) = chars[i..].iter().position(|x| *x == ']') {
                let after = i + close + 1;
                if chars.get(after) == Some(&'(') {
                    if let Some(pend) = chars[after..].iter().position(|x| *x == ')') {
                        flush(&mut job, &mut buf, bold, italic);
                        let label: String = chars[i + 1..i + close].iter().collect();
                        job.append(
                            &label,
                            0.0,
                            TextFormat { font_id: base_font(st, bold), color: p.accent, underline: Stroke::new(1.0, p.accent.gamma_multiply(0.6)), ..Default::default() },
                        );
                        i = after + pend + 1;
                        continue;
                    }
                }
            }
        }
        buf.push(c);
        i += 1;
    }
    flush(&mut job, &mut buf, bold, italic);
    job
}

fn paragraph(ui: &mut egui::Ui, text: &str, st: &Style, p: &Palette, color: Color32) {
    let mut job = inline(text, st, p, color);
    job.wrap.max_width = ui.available_width();
    ui.label(job);
}

fn code_block(ui: &mut egui::Ui, lang: &str, code: &str, p: &Palette) {
    egui::Frame::new()
        .fill(p.code_bg)
        .stroke(Stroke::new(1.0, p.border))
        .corner_radius(10)
        .inner_margin(egui::Margin { left: 12, right: 8, top: 6, bottom: 10 })
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(if lang.is_empty() { "kod" } else { lang }).size(12.0).color(p.faint));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if theme::icon_button(ui, Icon::Copy, "Kodu kopyala", p).clicked() {
                        ui.ctx().copy_text(code.to_string());
                    }
                });
            });
            egui::ScrollArea::horizontal().id_salt(ui.next_auto_id()).show(ui, |ui| {
                ui.label(egui::RichText::new(code.trim_end_matches('\n')).monospace().size(13.0).color(p.text));
            });
        });
}

/// Markdown metni çizer. `serif`: yanıt metni için serif yazı tipi.
pub fn render(ui: &mut egui::Ui, text: &str, p: &Palette, size: f32, serif: bool) {
    let st = Style { size, serif };
    let mut in_code = false;
    let mut code = String::new();
    let mut lang = String::new();
    let mut para: Vec<String> = Vec::new();
    let flush_para = |ui: &mut egui::Ui, para: &mut Vec<String>| {
        if !para.is_empty() {
            paragraph(ui, &para.join(" "), &st, p, p.text);
            ui.add_space(size * 0.5);
            para.clear();
        }
    };
    for raw in text.lines() {
        let line = raw.trim_end();
        if let Some(rest) = line.trim_start().strip_prefix("```") {
            if in_code {
                code_block(ui, &lang, &code, p);
                ui.add_space(6.0);
                code.clear();
                in_code = false;
            } else {
                flush_para(ui, &mut para);
                lang = rest.trim().to_string();
                in_code = true;
            }
            continue;
        }
        if in_code {
            code.push_str(raw);
            code.push('\n');
            continue;
        }
        let t = line.trim_start();
        if t.is_empty() {
            flush_para(ui, &mut para);
            continue;
        }
        // Başlıklar
        let hashes = t.chars().take_while(|c| *c == '#').count();
        if (1..=4).contains(&hashes) && t.chars().nth(hashes) == Some(' ') {
            flush_para(ui, &mut para);
            let hs = Style { size: size * [1.45, 1.25, 1.1, 1.0][hashes - 1], serif };
            ui.add_space(size * 0.3);
            let mut job = LayoutJob::default();
            job.append(&t[hashes + 1..].replace("**", ""), 0.0, TextFormat { font_id: base_font(&hs, true), color: p.text, ..Default::default() });
            job.wrap.max_width = ui.available_width();
            ui.label(job);
            ui.add_space(size * 0.25);
            continue;
        }
        // Ayırıcı
        if t == "---" || t == "***" || t == "___" {
            flush_para(ui, &mut para);
            ui.separator();
            continue;
        }
        // Alıntı
        if let Some(q) = t.strip_prefix('>') {
            flush_para(ui, &mut para);
            egui::Frame::new().inner_margin(egui::Margin { left: 12, right: 0, top: 2, bottom: 2 }).show(ui, |ui| {
                let r = ui.max_rect();
                ui.painter().line_segment([r.left_top(), r.left_bottom()], Stroke::new(3.0, p.border));
                paragraph(ui, q.trim(), &st, p, p.muted);
            });
            continue;
        }
        // Listeler
        let indent = (line.len() - t.len()) as f32;
        let bullet = if t.starts_with("- ") || t.starts_with("* ") || t.starts_with("+ ") {
            Some(("•".to_string(), &t[2..]))
        } else {
            let digits: String = t.chars().take_while(|c| c.is_ascii_digit()).collect();
            if !digits.is_empty() && t[digits.len()..].starts_with(". ") {
                Some((format!("{digits}."), &t[digits.len() + 2..]))
            } else {
                None
            }
        };
        if let Some((mark, body)) = bullet {
            flush_para(ui, &mut para);
            ui.horizontal_top(|ui| {
                ui.add_space(6.0 + indent * 6.0);
                ui.label(egui::RichText::new(mark).font(base_font(&st, false)).color(p.muted));
                ui.vertical(|ui| paragraph(ui, body, &st, p, p.text));
            });
            continue;
        }
        // Tablo satırları: olduğu gibi, eşit genişlikli yazı ile
        if t.starts_with('|') {
            flush_para(ui, &mut para);
            if !t.chars().all(|c| matches!(c, '|' | '-' | ':' | ' ')) {
                ui.label(egui::RichText::new(t).monospace().size(size * 0.85).color(p.text));
            }
            continue;
        }
        para.push(t.to_string());
    }
    if in_code {
        code_block(ui, &lang, &code, p);
    }
    flush_para(ui, &mut para);
}
