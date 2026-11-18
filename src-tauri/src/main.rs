// Hide the console window in release builds on Windows.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // 0.4.0: the copy started as administrator writes the sign-in route block into the hosts file and exits
    // (src/hosts.rs). Before Tauri: the single-instance plugin would otherwise hand the arguments to the running
    // launcher and quit.
    let args: Vec<String> = std::env::args().collect();
    if let Some(i) = args.iter().position(|a| a == "--coldz-hosts") {
        let code = args.get(i + 1).map(|f| coldz_launcher_lib::hosts_main(f)).unwrap_or(2);
        std::process::exit(code);
    }
    coldz_launcher_lib::run()
}
