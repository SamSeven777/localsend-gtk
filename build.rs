fn main() {
    let out =
        std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap()).join("localsend.gresource");
    println!("cargo:rerun-if-changed=gresources.xml");
    println!("cargo:rerun-if-changed=assets");
    let status = std::process::Command::new("glib-compile-resources")
        .arg("gresources.xml")
        .arg("--target")
        .arg(out)
        .status()
        .expect("Install glib-compile-resources (GLib development tools)");
    assert!(status.success(), "Could not compile LocalSend resources");
}
