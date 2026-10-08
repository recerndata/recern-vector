fn main() {
    // Link with chained fixups on macOS. The legacy dyld-info layout that ld
    // otherwise emits for this extension can leave the symbol string table
    // 4-byte aligned, which dyld on macOS 27 rejects at import time with
    // "mis-aligned LINKEDIT string pool".
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        println!("cargo:rustc-cdylib-link-arg=-Wl,-fixup_chains");
    }
}
