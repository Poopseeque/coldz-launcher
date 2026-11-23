// Hide the console window in release builds on Windows.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // 0.4.0: the copy started as administrator writes the sign-in block into the hosts file (src/hosts.rs); 0.5.7: with
    // --coldz-watch it stays until the launcher closes and takes the block out itself. Before Tauri: the
    // single-instance plugin would otherwise hand the arguments to the running launcher and quit.
    let args: Vec<String> = std::env::args().collect();
    if let Some(i) = args.iter().position(|a| a == "--coldz-hosts") {
        std::process::exit(coldz_launcher_lib::hosts_main(&args[i + 1..]));
    }
    coldz_launcher_lib::run()
}
