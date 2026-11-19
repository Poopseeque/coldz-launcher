//! 0.5.2: the launcher never vanishes without a word at start (streamer report 20.11: «он даже процесс не создаёт»).
//!
//! * Before Tauri: is the WebView2 runtime there? Without it Tauri cannot make the window, `build()` fails and the
//!   process used to exit at once (release builds have no console, so nobody saw why). The installer only looks at
//!   the EdgeUpdate registry key, so a runtime that a «debloat» tool removed or broke (the key stays) is never
//!   repaired, and the portable zip has no installer at all. Now: a message in the Windows language with «Yes» =
//!   open Microsoft's WebView2 download page.
//! * Any panic on the main thread (like 0.5.0's double `.manage`, c0000409): written to `start-error.log` in the
//!   settings folder and shown in a message box (Ctrl+C copies its text), instead of a silent exit.

use std::io::Write;
use std::path::PathBuf;

/// Microsoft's WebView2 Runtime download (the same link the installer uses).
pub const WEBVIEW2_URL: &str = "https://go.microsoft.com/fwlink/p/?LinkId=2124703";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Lang {
    Ru,
    En,
    Es,
    Pt,
}

/// The language of the message boxes from the primary language of the Windows UI (LANGID & 0x3ff).
pub fn lang_of(langid: u16) -> Lang {
    match langid & 0x3ff {
        0x19 | 0x22 | 0x23 | 0x3f | 0x40 | 0x43 | 0x44 => Lang::Ru, // ru, uk, be, kk, ky, uz, tt
        0x0a => Lang::Es,
        0x16 => Lang::Pt,
        _ => Lang::En,
    }
}

fn ui_lang() -> Lang {
    #[cfg(windows)]
    {
        // SAFETY: no arguments, returns a LANGID.
        lang_of(unsafe { windows_sys::Win32::Globalization::GetUserDefaultUILanguage() })
    }
    #[cfg(not(windows))]
    {
        Lang::En
    }
}

pub fn webview2_text(lang: Lang, err: &str) -> (String, String) {
    let (title, body) = match lang {
        Lang::Ru => (
            "COLDZ: нужен WebView2",
            "Для окна лаунчера COLDZ нужен компонент Windows «Microsoft Edge WebView2 Runtime». \
             На этом компьютере его нет или он повреждён, поэтому лаунчер не может открыться.\n\n\
             Нажмите «Да»: откроется страница Microsoft. Скачайте и запустите установщик WebView2, \
             потом снова откройте лаунчер COLDZ.",
        ),
        Lang::Es => (
            "COLDZ: falta WebView2",
            "La ventana del launcher COLDZ necesita el componente de Windows «Microsoft Edge WebView2 Runtime». \
             En este equipo no está o está dañado, por eso el launcher no puede abrirse.\n\n\
             Pulse «Sí»: se abrirá la página de Microsoft. Descargue e instale WebView2 y vuelva a abrir el launcher COLDZ.",
        ),
        Lang::Pt => (
            "COLDZ: falta o WebView2",
            "A janela do launcher COLDZ precisa do componente do Windows «Microsoft Edge WebView2 Runtime». \
             Neste computador ele não existe ou está danificado, por isso o launcher não abre.\n\n\
             Clique em «Sim»: a página da Microsoft vai abrir. Baixe e instale o WebView2 e abra o launcher COLDZ de novo.",
        ),
        Lang::En => (
            "COLDZ: WebView2 needed",
            "The COLDZ launcher window needs the Windows component «Microsoft Edge WebView2 Runtime». \
             It is missing or damaged on this computer, so the launcher cannot open.\n\n\
             Click «Yes» to open Microsoft's page. Download and install WebView2, then open the COLDZ launcher again.",
        ),
    };
    (title.to_string(), format!("{body}\n\n({err})"))
}

pub fn crash_text(lang: Lang, err: &str, log: Option<&PathBuf>) -> (String, String) {
    let log = log.map(|p| p.display().to_string()).unwrap_or_default();
    let (title, body) = match lang {
        Lang::Ru => (
            "COLDZ: лаунчер не запустился",
            "Лаунчер COLDZ не смог запуститься. Нажмите Ctrl+C, чтобы скопировать этот текст, и пришлите его \
             в группу COLDZ ВКонтакте. Играть можно и без лаунчера: FiveM, поиск «COLDZ».",
        ),
        Lang::Es => (
            "COLDZ: el launcher no se inició",
            "El launcher COLDZ no pudo iniciarse. Pulse Ctrl+C para copiar este texto y envíelo al grupo de COLDZ. \
             También puede jugar sin el launcher: FiveM, busque «COLDZ».",
        ),
        Lang::Pt => (
            "COLDZ: o launcher não iniciou",
            "O launcher COLDZ não conseguiu iniciar. Aperte Ctrl+C para copiar este texto e envie ao grupo do COLDZ. \
             Também dá para jogar sem o launcher: FiveM, procure «COLDZ».",
        ),
        Lang::En => (
            "COLDZ: the launcher did not start",
            "The COLDZ launcher could not start. Press Ctrl+C to copy this text and send it to the COLDZ group. \
             You can also play without the launcher: FiveM, search «COLDZ».",
        ),
    };
    let mut text = format!("{body}\n\n{err}");
    if !log.is_empty() {
        text.push_str(&format!("\n\n{log}"));
    }
    (title.to_string(), text)
}

/// `start-error.log`: next to settings.json (portable `data\`, else `%APPDATA%\com.coldz.launcher`).
fn log_path(portable: Option<PathBuf>) -> Option<PathBuf> {
    let dir = portable.or_else(|| std::env::var_os("APPDATA").map(|a| PathBuf::from(a).join("com.coldz.launcher")))?;
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir.join("start-error.log"))
}

fn write_log(path: Option<&PathBuf>, line: &str) {
    let Some(path) = path else { return };
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(f, "[unix {secs}] v{} {line}", env!("CARGO_PKG_VERSION"));
    }
}

#[cfg(windows)]
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// A plain Windows message box (works before any window exists). `yes_no`: true when «Yes» was pressed.
#[cfg(windows)]
fn message_box(title: &str, text: &str, yes_no: bool) -> bool {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        MessageBoxW, IDYES, MB_ICONERROR, MB_ICONWARNING, MB_OK, MB_SETFOREGROUND, MB_TOPMOST, MB_YESNO,
    };
    let (t, b) = (wide(title), wide(text));
    let style = if yes_no { MB_YESNO | MB_ICONWARNING } else { MB_OK | MB_ICONERROR } | MB_SETFOREGROUND | MB_TOPMOST;
    // SAFETY: both strings are NUL-terminated UTF-16 that live until the call returns; no owner window.
    unsafe { MessageBoxW(std::ptr::null_mut(), b.as_ptr(), t.as_ptr(), style) == IDYES }
}

#[cfg(not(windows))]
fn message_box(title: &str, text: &str, _yes_no: bool) -> bool {
    eprintln!("{title}: {text}");
    false
}

#[cfg(windows)]
fn open_url(url: &str) {
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    let (op, u) = (wide("open"), wide(url));
    // SAFETY: NUL-terminated strings, no parent window.
    unsafe {
        ShellExecuteW(std::ptr::null_mut(), op.as_ptr(), u.as_ptr(), std::ptr::null(), std::ptr::null(), SW_SHOWNORMAL);
    }
}

#[cfg(not(windows))]
fn open_url(_url: &str) {}

/// True = WebView2 is usable, go on. False = it is missing; the player was told (and maybe sent to the download page).
/// `COLDZ_FAKE_NO_WEBVIEW2=1` makes it act as missing (smoke test of this message on a PC that has WebView2).
pub fn check_webview2(portable: Option<PathBuf>) -> bool {
    let fake = std::env::var_os("COLDZ_FAKE_NO_WEBVIEW2").is_some();
    let err = match tauri::webview_version() {
        Ok(v) if !fake && !v.trim().is_empty() => return true,
        Ok(v) if !fake => format!("WebView2 version empty: {v:?}"),
        Ok(_) => "COLDZ_FAKE_NO_WEBVIEW2".to_string(),
        Err(e) => format!("WebView2: {e}"),
    };
    write_log(log_path(portable).as_ref(), &err);
    let (title, text) = webview2_text(ui_lang(), &err);
    if message_box(&title, &text, true) {
        open_url(WEBVIEW2_URL);
    }
    false
}

/// Every panic goes to `start-error.log`; one on the main thread (start-up, `build()`) also shows a message box,
/// since the process ends right after it. Panics in worker threads are only logged (tokio keeps those alive).
pub fn install_panic_hook(portable: Option<PathBuf>) {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let msg = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "panic".into());
        let at = info.location().map(|l| format!(" ({}:{})", l.file(), l.line())).unwrap_or_default();
        let thread = std::thread::current();
        let name = thread.name().unwrap_or("?").to_string();
        let line = format!("panic in thread {name}: {msg}{at}");
        let log = log_path(portable.clone());
        write_log(log.as_ref(), &line);
        default(info);
        if name == "main" {
            let (title, text) = crash_text(ui_lang(), &format!("v{}: {msg}{at}", env!("CARGO_PKG_VERSION")), log.as_ref());
            message_box(&title, &text, false);
        }
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn languages_from_langid() {
        assert_eq!(lang_of(0x0419), Lang::Ru); // ru-RU
        assert_eq!(lang_of(0x0422), Lang::Ru); // uk-UA
        assert_eq!(lang_of(0x0416), Lang::Pt); // pt-BR
        assert_eq!(lang_of(0x0c0a), Lang::Es); // es-ES
        assert_eq!(lang_of(0x0409), Lang::En);
        assert_eq!(lang_of(0x0407), Lang::En); // de: English text
    }

    #[test]
    fn texts_have_no_long_dashes_and_name_webview2() {
        for l in [Lang::Ru, Lang::En, Lang::Es, Lang::Pt] {
            let (t1, b1) = webview2_text(l, "x");
            let (t2, b2) = crash_text(l, "x", Some(&PathBuf::from("C:\\a\\start-error.log")));
            for s in [&t1, &b1, &t2, &b2] {
                assert!(!s.contains('\u{2014}') && !s.contains('\u{2013}'), "{s}");
            }
            assert!(b1.contains("WebView2"));
            assert!(b2.contains("start-error.log"));
        }
    }
}
