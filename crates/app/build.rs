include!("../../build-support/resources.rs");

fn main() {
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tessera.manifest");
    println!("cargo:rerun-if-changed={}", manifest.display());
    println!("cargo:rustc-link-arg-bins=/MANIFEST:EMBED");
    println!("cargo:rustc-link-arg-bins=/MANIFESTINPUT:{}", manifest.display());
    embed_resources("Tessera", "tessera");
}
