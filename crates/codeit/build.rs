fn main() {
    // The web app is built into the binary (src/web/assets.rs): rebuild when it changes.
    println!("cargo:rerun-if-changed=web-dist");
}
