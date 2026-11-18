fn main() {
    println!("cargo::rustc-check-cfg=cfg(feeder_sparkle)");
    println!("cargo::rerun-if-env-changed=FEEDER_SPARKLE_DIR");
    println!("cargo::rerun-if-changed=src/updater.m");
    println!("cargo::rerun-if-changed=src/notify.m");

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        cc::Build::new().file("src/notify.m").flag("-fobjc-arc").compile("feeder_notify");
        println!("cargo::rustc-link-lib=framework=UserNotifications");
        println!("cargo::rustc-link-lib=framework=AppKit");
    }

    let Some(framework_dir) = std::env::var_os("FEEDER_SPARKLE_DIR")
        .filter(|dir| !dir.is_empty() && std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos"))
    else {
        return;
    };
    let framework_dir = std::path::PathBuf::from(framework_dir);
    cc::Build::new().file("src/updater.m").flag("-fobjc-arc").flag("-F").flag(&framework_dir).compile("feeder_updater");
    println!("cargo::rustc-link-search=framework={}", framework_dir.display());
    println!("cargo::rustc-link-lib=framework=Sparkle");
    println!("cargo::rustc-link-lib=framework=AppKit");
    println!("cargo::rustc-link-arg=-Wl,-rpath,@executable_path/../Frameworks");
    println!("cargo::rustc-cfg=feeder_sparkle");
}
