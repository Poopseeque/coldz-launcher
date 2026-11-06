fn main() {
    // Rebuild when the launcher settings or the bundled news change.
    println!("cargo:rerun-if-changed=../launcher.config.json");
    println!("cargo:rerun-if-changed=../news/news.json");
    tauri_build::build()
}
