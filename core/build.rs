fn main() {
    // unrar_sys (bundled C++ UnRAR) uses advapi32 APIs (registry, security, crypto)
    // but does not declare the link on MSVC.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        println!("cargo:rustc-link-lib=advapi32");
    }
}
