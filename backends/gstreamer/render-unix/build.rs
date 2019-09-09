use std::env;

fn main() {
    let target = env::var("TARGET").expect("TARGET was not set");
    if target.contains("linux") ||
       target.contains("dragonfly") ||
       target.contains("freebsd") ||
       target.contains("netbsd") ||
       target.contains("openbsd") {
        println!("cargo:rustc-cfg='feature=\"gl-egl\"'");
        println!("cargo:rustc-cfg='feature=\"gl-x11\"'");
    }
}
