fn main() {
    let config = slint_build::CompilerConfiguration::new().with_style("fluent-light".into());
    slint_build::compile_with_config("ui/app.slint", config).expect("Slint build failed");

    // Embed the application icon and version info into the Windows executables.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        println!("cargo:rerun-if-changed=assets/archiver.ico");
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/archiver.ico")
            .set("ProductName", "Archiver")
            .set("FileDescription", "Archiver - archive manager")
            .set("LegalCopyright", "MIT License");
        res.compile().expect("failed to embed Windows resources");
    }
}
