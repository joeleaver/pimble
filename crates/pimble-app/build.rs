//! Embeds Pimble's icon (and its name and version) in `pimble.exe` when building for
//! Windows, the only place a program carries its own icon: Explorer, the Start menu,
//! shortcuts and a pinned taskbar button read it from the file. Without it they show
//! the blank default (reported 2026-11-13). Elsewhere this does nothing.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let icon = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/brand/pimble.ico");
    println!("cargo:rerun-if-changed={icon}");
    let mut resource = winresource::WindowsResource::new();
    resource
        .set_icon(icon)
        .set("ProductName", "Pimble")
        .set("FileDescription", "Pimble")
        .set("CompanyName", "Pimble");
    // A failure here is a build failure: a Windows package without its icon is
    // the bug this exists to prevent.
    resource.compile().expect("embed Pimble's icon in pimble.exe");
}
