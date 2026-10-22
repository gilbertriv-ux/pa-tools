//! Windows only: embed the app icon and version info (VERSIONINFO) into `pa-tools.exe`, so it
//! shows in Explorer, the taskbar, the Start menu and Alt-Tab.
//!
//! On every other target this does nothing. A missing resource compiler is a warning, so a
//! cross-compile from macOS or Linux still links, unless `SOUNDCRAFT_REQUIRE_WINRES=1` turns it
//! into an error (for release builds).

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=../../assets/app-icon/patools.ico");
    println!("cargo:rerun-if-env-changed=SOUNDCRAFT_REQUIRE_WINRES");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let exe = format!("{}.exe", soundcraft_brand::BIN_NAME);
    let mut res = winresource::WindowsResource::new();
    res.set_icon("../../assets/app-icon/patools.ico")
        .set("ProductName", soundcraft_brand::APP_NAME)
        .set("FileDescription", &format!("{} digital audio workstation", soundcraft_brand::APP_NAME))
        .set("CompanyName", soundcraft_brand::VENDOR)
        .set("LegalCopyright", soundcraft_brand::LEGAL_COPYRIGHT)
        .set("OriginalFilename", &exe)
        .set("InternalName", soundcraft_brand::BIN_NAME);
    if let Err(e) = res.compile() {
        if std::env::var_os("SOUNDCRAFT_REQUIRE_WINRES").is_some() {
            eprintln!("embedding Windows resources failed: {e}");
            std::process::exit(1);
        }
        println!("cargo:warning={exe} built without icon/version resources: {e}");
    }
}
