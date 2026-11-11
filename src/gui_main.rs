//! RustyAgent.exe — masaüstü uygulaması. Windows'ta "windows" alt sistemiyle derlenir:
//! çift tıklayınca arkada konsol (cmd) penceresi açılmaz.

#![cfg_attr(windows, windows_subsystem = "windows")]

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // Sanal imleç katmanı bu programın alt işlemi olarak da çalışabilir
    if args.first().map(|a| a == "overlay").unwrap_or(false) {
        let _ = rusty_agent::overlay::run_overlay();
        return;
    }
    let resume = if args.iter().any(|a| a == "-c" || a == "--continue") { Some(None) } else { None };
    let page = args.iter().position(|a| a == "--page").and_then(|i| args.get(i + 1).cloned());
    if let Err(e) = rusty_agent::launch_gui(resume, page) {
        // Konsol olmadığı için hatayı pencereyle göster
        rfd::MessageDialog::new()
            .set_title("RustyAgent başlatılamadı")
            .set_description(format!("{e:#}"))
            .set_level(rfd::MessageLevel::Error)
            .show();
    }
}
