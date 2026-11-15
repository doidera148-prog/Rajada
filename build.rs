//! Embute o ícone e os metadados no executável do Windows.
//! Em outros sistemas não faz nada (o ícone da janela vem do próprio código).

fn main() {
    println!("cargo:rerun-if-changed=assets/rajada.ico");
    println!("cargo:rerun-if-changed=build.rs");

    #[cfg(windows)]
    {
        if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
            let mut recurso = winres::WindowsResource::new();
            recurso.set_icon("assets/rajada.ico");
            recurso.set("ProductName", "Rajada");
            recurso.set("FileDescription", "Rajada - acelerador de downloads");
            recurso.set("CompanyName", "Rajada");
            recurso.set("LegalCopyright", "MIT");
            recurso
                .compile()
                .expect("falha ao embutir o ícone no executável");
        }
    }
}
