fn main() {
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let target_env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    if target_os == "windows" && target_env != "msvc" {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("webview2");
        println!("cargo:rustc-link-search=native={}", dir.display());
        println!("cargo:rustc-link-lib=static:+whole-archive=WebView2LoaderStatic");
        cc::Build::new().file(dir.join("msvc_shim.c")).compile("msvc_shim");
    }
    println!("cargo:rerun-if-changed=webview2");
    println!("cargo:rerun-if-changed=icons");

    tauri_build::build()
}
