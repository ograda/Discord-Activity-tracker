fn main() {
    println!("cargo:rerun-if-changed=assets/dat.ico");

    if cfg!(target_os = "windows") {
        let mut res = winres::WindowsResource::new();

        res.set_icon_with_id("assets/dat.ico", "1");

        for (index, name) in ["water", "stand_up", "stretch", "eye_rest", "generic"]
            .iter()
            .enumerate()
        {
            let path = format!("assets/reminders/{name}.ico");
            println!("cargo:rerun-if-changed={path}");
            res.set_icon_with_id(&path, &(11 + index).to_string());
        }
        res.compile().expect("failed to compile Windows resources");
    }
}
