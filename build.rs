//! Embeds the application icon and version information into the executables.
//!
//! Needs `rc.exe` from the Windows SDK, which the MSVC toolchain already requires.

fn main() {
    println!("cargo:rerun-if-changed=assets/tkbl.ico");

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let mut res = winresource::WindowsResource::new();
    res.set_icon("assets/tkbl.ico");
    res.set("ProductName", "ThinkPad Keyboard Backlight");
    res.set(
        "FileDescription",
        "Keyboard backlight control for ThinkPads",
    );
    res.set("LegalCopyright", "MIT licensed");
    if let Err(e) = res.compile() {
        // A missing resource compiler should not stop someone building the thing; they
        // just get binaries without an icon.
        println!("cargo:warning=icon not embedded: {e}");
    }
}
