// Compiles src/decode.c against FFmpeg. FFMPEG_DIR names a folder with include/ and lib/ (Windows, or any
// FFmpeg kept outside the system); otherwise pkg-config finds it.
fn main() {
    println!("cargo:rerun-if-changed=src/decode.c");
    println!("cargo:rerun-if-env-changed=FFMPEG_DIR");
    let mut cc = cc::Build::new();
    cc.file("src/decode.c");
    if let Ok(dir) = std::env::var("FFMPEG_DIR") {
        cc.include(format!("{dir}/include"));
        println!("cargo:rustc-link-search=native={dir}/lib");
        for lib in ["avcodec", "avutil"] {
            println!("cargo:rustc-link-lib={lib}");
        }
    } else {
        for lib in ["libavcodec", "libavutil"] {
            let found = pkg_config::probe_library(lib)
                .unwrap_or_else(|e| panic!("{lib} not found: install FFmpeg's development files or set FFMPEG_DIR ({e})"));
            cc.includes(found.include_paths);
        }
    }
    cc.compile("hengedecode");
}
