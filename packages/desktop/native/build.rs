fn main() {
    for path in [
        "vendor/md4c/md4c.c",
        "vendor/md4c/md4c.h",
        "vendor/md4c/bridge.c",
    ] {
        println!("cargo:rerun-if-changed={path}");
    }
    cc::Build::new()
        .files(["vendor/md4c/md4c.c", "vendor/md4c/bridge.c"])
        .include("vendor/md4c")
        .flag_if_supported("-std=c99")
        .warnings(false)
        .compile("paseo_md4c");
}
