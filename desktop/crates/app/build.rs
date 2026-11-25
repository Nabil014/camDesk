//! Embebe `assets/icon.ico` en camdesk.exe para el Explorador y los accesos directos.
//! El icono de la ventana y de la barra de tareas lo pone `main` con `assets/icon.png`.

fn main() {
    println!("cargo:rerun-if-changed=assets/icon.ico");
    #[cfg(windows)]
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut resource = winresource::WindowsResource::new();
        resource.set_icon("assets/icon.ico");
        if let Err(error) = resource.compile() {
            // Sin rc.exe el binario igual compila, con el icono genérico de Windows.
            println!("cargo:warning=no se pudo embeber el icono: {error}");
        }
    }
}
