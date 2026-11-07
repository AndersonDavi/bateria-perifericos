// Embeds the app icon and the name Windows shows in notifications and Task Manager.
fn main() {
    println!("cargo:rerun-if-changed=assets/app.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/app.ico")
            .set("FileDescription", "Batería de periféricos")
            .set("ProductName", "Batería de periféricos")
            .set("OriginalFilename", "bateria-perifericos.exe");
        res.compile().expect("recursos de Windows");
    }
}
