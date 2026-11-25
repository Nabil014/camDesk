fn main() {
    let manifest = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let def = std::path::Path::new(&manifest).join("cellcam.def");
    println!("cargo:rerun-if-changed={}", def.display());
    println!("cargo:rustc-cdylib-link-arg=/DEF:{}", def.display());
}
