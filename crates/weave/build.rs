fn main() {
    println!("cargo:rustc-check-cfg=cfg(weave_vt)");
    let wasm = std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() == Ok("wasm32");
    if wasm || std::env::var_os("CARGO_FEATURE_VIRTUAL_TIME").is_some() {
        println!("cargo:rustc-cfg=weave_vt");
    }
}
