fn main() {
    let ffmpeg = pkg_config::Config::new()
        .cargo_metadata(false)
        .probe("libavcodec")
        .expect("FFmpeg development files must be provided by Nix");
    let mut build = cc::Build::new();
    if std::env::var("PROFILE").as_deref() == Ok("debug") {
        build.opt_level(1);
    }
    build
        .file("native/media.c")
        .includes(ffmpeg.include_paths)
        .flag_if_supported("-std=c11")
        .flag_if_supported("-Wno-deprecated-declarations")
        .compile("chr_media");
    for package in ["libavformat", "libavcodec", "libavutil", "vulkan"] {
        pkg_config::probe_library(package).unwrap();
    }
    println!("cargo:rerun-if-changed=native/media.c");
    println!("cargo:rerun-if-changed=native/media.h");
}
