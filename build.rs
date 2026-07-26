fn main() {
    // 将 .def 文件传递给 MSVC 链接器
    let manifest = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    // 只对 cdylib 目标应用 .def 文件，测试 EXE 不受影响
    println!("cargo:rustc-cdylib-link-arg=/DEF:{manifest}\\vxapo.def");
    println!("cargo:rerun-if-changed=vxapo.def");
}