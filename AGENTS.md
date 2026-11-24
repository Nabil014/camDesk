# AGENTS.md — Camdesk

Contrato para cualquier agente que escriba código en este proyecto. El diseño está en [PLAN.md](PLAN.md). El protocolo del teléfono está en `d:\DESARROLLO\Proyectos\cellCam\PROTOCOL.md` y manda sobre este repo; la copia de lectura es [docs/PROTOCOL-v1.md](docs/PROTOCOL-v1.md).

## Qué es esto

Camdesk es un escritorio Windows, en Rust, que usa el celular Android como webcam a través de la app **cellCam** ya publicada en el teléfono. Publica esa imagen como cámara de Windows para Zoom y Meet, y graba la pantalla con audio del sistema y micrófono de la PC.

## Antes de tocar código

1. Leer [PLAN.md](PLAN.md) y `docs/PROTOCOL-v1.md`. No redescubrir el protocolo leyendo OBS ni DroidCam. Si `docs/PROTOCOL-v1.md` no existe todavía, pedírselo al usuario; no inventar campos.
2. Quedarse en el crate que se te asignó. Si el cambio cruza un límite de crate, parar y actualizar PLAN.md primero.
3. cellCam (`d:\DESARROLLO\Proyectos\cellCam`) es de solo lectura. El flag de audio no se “termina” desde aquí.
4. No commitear salvo que el usuario lo pida. No hay secretos en este repo; no traer `cellcam-release.jks` ni `keystore.properties`.
5. Cada agente trabaja en su propio worktree y su rama. El merge lo hace el usuario.

## Reparto

Varios agentes pueden trabajar a la vez. Cada uno es dueño de una carpeta. Las firmas iniciales están en PLAN.md, "Firmas públicas iniciales": nadie espera a que otro crate exista para empezar.

| Agente | Carpeta | Puede empezar | Depende de |
|---|---|---|---|
| Aplicación | `desktop/Cargo.toml`, `desktop/crates/app` | Ya: primero el esqueleto del workspace. Cablear el resto al final | Todos |
| Protocolo | `desktop/crates/protocol` | Ya | Nada |
| Conexión | `desktop/crates/link` | Ya, con las firmas de `protocol` en PLAN.md | `protocol` |
| Decode | `desktop/crates/decode` | Ya, con las firmas de `protocol` en PLAN.md | `protocol` |
| Efectos | `desktop/crates/fx` | Ya | Nada |
| Memoria de cámara | `desktop/crates/vcam-shm` | Ya | Nada |
| DLL de cámara | `spikes/vcam`, luego `desktop/crates/vcam` | El paso 0, ya. El crate, cuando el paso 0 tenga resultados en PLAN.md | `vcam-shm` |
| Grabación | `desktop/crates/record` | Ya | Nada |

Nadie edita el crate de otro. La integración vive solo en `app`. `desktop/Cargo.toml` lo edita solo Aplicación; las dependencias de cada crate van en su propio `Cargo.toml`.

## Límites que no se cruzan

- `protocol` no hace E/S: no enlaza FFmpeg, `windows`, egui, sockets ni adb. Parsea y serializa bytes. Los tests son vectores fijos, sin teléfono.
- `link` abre el TCP, corre `adb` y reintenta. No decodifica ni pinta. No reimplementa el parseo: usa `protocol`.
- `decode` no pinta UI, no escribe el MP4 y no abre sockets. Entrega BGRA ya rotado para la preview y escribe NV12 1920x1080 con barras en los buffers que le pasan.
- `fx` trabaja sobre un buffer BGRA que le pasan. No conoce cellCam, el decoder, el lienzo ni la UI. No redistribuye ni empaqueta DLL o modelos de NVIDIA: los busca en el runtime que instaló el usuario.
- `vcam-shm` no llama a Media Foundation ni a `MFCreateVirtualCamera`. Solo el archivo mapeado descrito en PLAN.md.
- `vcam` no decodifica H.264 y no abre el socket del teléfono. Lee NV12 con el `Reader` de `vcam-shm` y lo entrega como `IMFMediaSource`. Su parte `rlib` da de alta y de baja la cámara para `app`.
- `record` no conoce cellCam. Recibe “empezá a grabar este monitor con estas dos fuentes de audio”.
- `app` no reimplementa el handshake, la reconexión, el seqlock ni Media Foundation. Los usa.

## Invariantes

- Magic de handshake `0x4343414D`, 16 bytes. Magic de frame `0x46524D30`, 20 bytes. Puerto 4747. Big-endian en el socket.
- No mostrar video hasta el primer IDR posterior al SPS/PPS. Cada reconexión reinicia el decoder.
- La rotación la aplica Camdesk, con los bits 0-1 de `reserved`, y puede cambiar en cualquier frame.
- Un solo cliente TCP. Reintento cada 2 segundos. Al desconectar USB, `adb -s <serial> forward --remove tcp:4747`; nunca `--remove-all`. adb nunca en el hilo de la UI.
- La cámara virtual es siempre 1920x1080 NV12 a 30 fps. No cambia con la rotación ni con la resolución pedida al teléfono. El archivo mapeado no se recrea durante la ejecución.
- La DLL de la cámara corre dentro de Frame Server como `LOCAL SERVICE`, no como el usuario. No usa `%LOCALAPPDATA%` ni rutas del perfil del usuario. La ruta del archivo mapeado sale solo de `vcam_shm::default_path()`.
- La cámara virtual se llama `cellCam`, vive mientras la app está abierta y solo para el usuario actual. Si el paso 0 muestra que hace falta administrador, se para y se decide con el usuario.
- La grabación usa encoder de hardware (NVENC, AMF, Quick Sync) y `h264_mf` como último recurso. No se usa `libx264`. FFmpeg es build LGPL.
- La grabación es a 30 fps constantes, en MP4 fragmentado, y rellena con silencio cuando el loopback no entrega audio.
- El micrófono de la grabación no se reproduce por los parlantes.
- Orden de efectos por frame: rotación → desenfoque de fondo → corrección de color → preview y lienzo. Los efectos no tocan la grabación de pantalla.
- Un efecto que falla se apaga y avisa; nunca corta la cámara virtual. Sin RTX o sin runtime, el desenfoque queda deshabilitado con el motivo, sin respaldo.
- Al abrir la ventana no se carga FFmpeg (carga diferida), no se carga el runtime de NVIDIA, no se registra la cámara virtual, no se crea el archivo mapeado, no se corre adb, no se abre WASAPI y no se captura la pantalla. Eso ocurre al conectar, al grabar o al activar el desenfoque.
- Textos de la interfaz en español.

## Cómo coordinarse

- Un cambio de firma pública entre crates se anota en PLAN.md, "Firmas públicas iniciales", en el mismo cambio que el código. El otro agente no se entera si solo vive en un commit ajeno.
- El layout del archivo mapeado es el contrato entre `vcam-shm` y `vcam`. Cambiar un offset rompe Zoom. Si hace falta cambiarlo, se sube `version` y se actualizan los dos lados juntos.
- Los resultados del paso 0 se anotan en PLAN.md, "Resultados del paso 0", antes de escribir el crate `vcam`.
- Si Zoom o Meet no listan la cámara de Windows 11, no se improvisa otro backend. Se avisa y se decide con el usuario el respaldo softcam que ya está escrito en PLAN.md.
- Si un test necesita el teléfono, el micrófono o Zoom, no entra en `cargo test` del crate. Eso es la prueba de aceptación de PLAN.md y la corre una persona.

## Verificación por crate

- `protocol`, `vcam-shm` y `link`: `cargo test -p <crate>` tiene que pasar sin hardware, en Windows y en macOS. `link` se prueba con un servidor TCP local que reproduce fixtures de bytes: handshake, corte a mitad de frame, magic roto, frame de más de 4 MiB, rechazo por otro cliente.
- `vcam-shm`: test de escritor y lector en hilos separados que nunca entrega un frame mezclado.
- `decode`: test con un Annex-B mínimo grabado en el repo, sin red. Cubre las cuatro rotaciones y el lienzo con barras.
- `fx`: `cargo test -p fx` prueba la corrección de color con píxeles fijos (neutro no cambia nada, extremos sin desborde) en cualquier sistema. El desenfoque NVIDIA no tiene test automático: quien lo toque deja dicho en qué GPU y versión del runtime lo probó, y a cuántos fps.
- `record`: no se exige un MP4 en CI. Quien lo toque deja dicho en el resumen cómo lo probó, qué encoder abrió, o que no pudo abrir el encoder de hardware.
- `vcam`: no tiene test automático. Quien lo toque deja dicho si Zoom y Meet listaron la cámara.
- `app`: `cargo build -p camdesk` tiene que compilar en Windows. La prueba con el teléfono, Zoom, Meet y el MP4 es la lista de PLAN.md, no un sustituto de los tests de crate.

## Cuando termines

Dejá el resumen en este orden: qué crate cambiaste, qué invariante tocaste, qué comando corriste y qué no pudiste probar.
