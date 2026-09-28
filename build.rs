fn main() {
    println!("cargo:rerun-if-changed=assets/dat.ico");

    if cfg!(target_os = "windows") {
        let mut res = winres::WindowsResource::new();

        res.set_icon_with_id("assets/dat.ico", "1");

        res.compile().expect("failed to compile Windows resources");
    }
}
