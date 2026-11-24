//! Embute o ícone do app (assets/arcdockwin.ico) no .exe: barra de título, Alt+Tab, dock e
//! Explorer usam ele.

fn main() {
    println!("cargo:rerun-if-changed=assets/arcdockwin.ico");
    println!("cargo:rerun-if-changed=assets/app.rc");
    embed_resource::compile("assets/app.rc", embed_resource::NONE).manifest_optional().unwrap();
}
