# camDesk

## Publicar una versión

1. Subir `version` en `desktop/crates/app/Cargo.toml`.
2. Compilar una vez (`cargo build` en `desktop/`) para que `Cargo.lock` quede al día, commitear y pushear a `main`.
3. `pwsh installer/publish.ps1 -Publicar -Notas "Qué cambió"`

Sin `-Publicar` solo compila, arma el instalador en `desktop/target/installer` y firma. Hace falta Inno Setup, Git (por OpenSSL), `gh` con sesión iniciada, FFmpeg en `desktop/third_party` y la clave privada en `%USERPROFILE%\.camdesk\release-key.pem`.

**Guardar una copia de esa clave.** Si se pierde, las apps instaladas no aceptan más actualizaciones.

