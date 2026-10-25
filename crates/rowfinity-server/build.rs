// rust-embed needs the web build directory to exist; create an empty one so the
// server still compiles before the UI has been built.
fn main() {
    let dist = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../web/dist");
    let _ = std::fs::create_dir_all(dist);
    println!("cargo:rerun-if-changed=../../web/dist");
}
