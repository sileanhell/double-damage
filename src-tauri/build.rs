fn main() {
    // On MSVC, webview2-com-sys already links WebView2LoaderStatic.lib by itself.
    // On windows-gnu/gnullvm it imports WebView2Loader.dll instead, so link the static lib here.
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let target_env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    if target_os == "windows" && target_env != "msvc" {
        // Link WebView2Loader statically so dd.exe does not need WebView2Loader.dll next to it.
        // Pulling the whole static lib in first defines those symbols, so the DLL import is dropped.
        // msvc_shim.c provides the few MSVC runtime symbols the static lib needs.
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("webview2");
        println!("cargo:rustc-link-search=native={}", dir.display());
        println!("cargo:rustc-link-lib=static:+whole-archive=WebView2LoaderStatic");
        cc::Build::new().file(dir.join("msvc_shim.c")).compile("msvc_shim");
    }
    println!("cargo:rerun-if-changed=webview2");
    // The line above disables cargo's default "rerun on any change", so track icons explicitly.
    println!("cargo:rerun-if-changed=icons");

    tauri_build::build()
}
