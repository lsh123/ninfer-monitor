// NInferMonitor
//
// This is free software; see the LICENSE.md file in the source distribution for precise wording.
//
// Copyright (C) 2026 Aleksey Sanin aleksey@aleksey.com. All Rights Reserved.

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-changed=src/ui");
    let debug = std::env::var("PROFILE").as_deref() == Ok("debug");
    let config = slint_build::CompilerConfiguration::new().with_debug_info(debug);
    slint_build::compile_with_config("src/ui/main_window.slint", config)?;
    #[cfg(windows)]
    {
        let version = env!("CARGO_PKG_VERSION");
        let mut res = winres::WindowsResource::new();
        res.set_icon("src/ui/icons/app_icon.ico");
        res.set("ProductName", "NInfer Monitor");
        res.set("FileDescription", "NInfer Monitor");
        res.set("LegalCopyright", "Copyright (C) 2026 Aleksey Sanin");
        res.set("FileVersion", version);
        res.set("ProductVersion", version);
        res.compile()?;
    }
    Ok(())
}
