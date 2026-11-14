include!("../../build-support/resources.rs");

fn main() {
    // The installer reuses the organiser's manifest: common controls v6 (task dialogs),
    // per-monitor DPI awareness.
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let manifest = root.join("crates/app/tessera.manifest");
    println!("cargo:rerun-if-changed={}", manifest.display());
    println!("cargo:rustc-link-arg-bins=/MANIFEST:EMBED");
    println!("cargo:rustc-link-arg-bins=/MANIFESTINPUT:{}", manifest.display());
    embed_resources("Tessera Setup", "tessera-setup");

    // The payload: release builds of the two programs, embedded with include_bytes!.
    println!("cargo:rerun-if-env-changed=TESSERA_PAYLOAD");
    let dir = std::env::var_os("TESSERA_PAYLOAD").map(std::path::PathBuf::from).unwrap_or_else(|| root.join("target/release"));
    for exe in ["tessera.exe", "tessera-settings.exe"] {
        let path = dir.join(exe);
        assert!(path.exists(), "{} is missing: build the release payload first (scripts/package.ps1 does this)", path.display());
        println!("cargo:rerun-if-changed={}", path.display());
        let key = exe.trim_end_matches(".exe").replace('-', "_").to_uppercase();
        println!("cargo:rustc-env=PAYLOAD_{key}={}", path.display());
    }
}
