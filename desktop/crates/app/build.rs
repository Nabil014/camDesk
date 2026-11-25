fn main() {
    #[cfg(windows)]
    {
        let icon = "assets/camdesk.ico";
        println!("cargo:rerun-if-changed={icon}");
        winresource::WindowsResource::new()
            .set_icon(icon)
            .compile()
            .expect("no se pudo incluir el icono de Camdesk");
    }
}
