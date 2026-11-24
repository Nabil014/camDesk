# AGENTS.md — Camdesk

Contrato para cualquier agente que escriba código en este proyecto. El diseño está en [PLAN.md](PLAN.md). El protocolo del teléfono está en `d:\DESARROLLO\Proyectos\cellCam\PROTOCOL.md` y manda sobre este repo.

## Qué es esto

Camdesk es un escritorio Windows, en Rust, que usa el celular Android como webcam a través de la app **cellCam** ya publicada en el teléfono. Publica esa imagen como cámara de Windows para Zoom y Meet, y graba la pantalla con audio del sistema y micrófono de la PC.

## Antes de tocar código

1. Leer [PLAN.md](PLAN.md) y el `PROTOCOL.md` de cellCam. No redescubrir el protocolo leyendo OBS ni DroidCam.
2. Quedarse en el crate que se te asignó. Si el cambio cruza un límite de crate, parar y actualizar PLAN.md primero.
3. cellCam (`d:\DESARROLLO\Proyectos\cellCam`) es de solo lectura. El flag de audio no se “termina” desde aquí.
4. No commitear salvo que el usuario lo pida. No hay secretos en este repo; no traer `cellcam-release.jks` ni `keystore.properties`.

## Reparto

Varios agentes pueden trabajar a la vez. Cada uno es dueño de una carpeta.

| Agente | Carpeta | Puede empezar | Depende de |
|---|---|---|---|
| Protocolo | `desktop/crates/protocol` | Ya | Nada |
| Memoria de cámara | `desktop/crates/vcam-shm` | Ya | Nada |
| Grabación | `desktop/crates/record` | Ya | Nada |
| Decode | `desktop/crates/decode` | Cuando `protocol` publique el tipo de frame | `protocol` |
| DLL de cámara | `vcam/` | Cuando el layout de `vcam-shm` esté en código | `vcam-shm` |
| Aplicación | `desktop/crates/app` | El esqueleto de la ventana, ya. Cablear el resto al final | Todos |

Nadie edita el crate de otro. La integración vive solo en `app`.

## Límites que no se cruzan

- `protocol` no enlaza FFmpeg, `windows`, egui ni sockets de adb. Parsea y serializa bytes. Los tests son vectores fijos, sin teléfono.
- `vcam-shm` no llama a Media Foundation ni a `MFCreateVirtualCamera`. Solo el archivo mapeado descrito en PLAN.md.
- `vcam/` no decodifica H.264 y no abre el socket del teléfono. Lee NV12 y lo entrega como `IMFMediaSource`.
- `record` no conoce cellCam. Recibe “empezá a grabar este monitor con estas dos fuentes de audio”.
- `decode` no pinta UI ni escribe el MP4. Entrega frames BGRA ya rotados, y NV12 para quien lo pida.
- `app` no reimplementa el handshake ni el seqlock. Los usa.

## Invariantes

- Magic de handshake `0x4343414D`, 16 bytes. Magic de frame `0x46524D30`, 20 bytes. Puerto 4747. Big-endian en el socket.
- No mostrar video hasta el primer IDR posterior al SPS/PPS.
- La rotación la aplica Camdesk, con los bits 0-1 de `reserved`.
- Un solo cliente TCP. Reintento cada 2 segundos. Al desconectar USB, `adb forward --remove`.
- La cámara virtual se llama `cellCam`, vive mientras la app está abierta y solo para el usuario actual. Registro en `HKCU`.
- La grabación usa encoder de hardware (NVENC, AMF, Quick Sync, `h264_mf`). `libx264` es el último recurso.
- El micrófono de la grabación no se reproduce por los parlantes.
- Al abrir la ventana no se carga FFmpeg, no se registra la cámara virtual, no se abre WASAPI y no se captura la pantalla. Eso ocurre al conectar o al grabar.
- Textos de la interfaz en español.

## Cómo coordinarse

- Un cambio de firma pública entre crates se anota en PLAN.md en el mismo cambio que el código. El otro agente no se entera si solo vive en un commit ajeno.
- El layout de `%LOCALAPPDATA%\Camdesk\cellcam-vcam.bin` es el contrato entre `vcam-shm` y `vcam/`. Cambiar un offset rompe Zoom. Si hace falta cambiarlo, se sube `version` y se actualizan los dos lados juntos.
- Si Zoom o Meet no listan la cámara de Windows 11, no se improvisa otro backend. Se avisa y se pasa al respaldo softcam que ya está escrito en PLAN.md.
- Si un test necesita el teléfono, el micrófono o Zoom, no entra en `cargo test` del crate. Eso es la prueba de aceptación de PLAN.md y la corre una persona.

## Verificación por crate

- `protocol` y `vcam-shm`: `cargo test -p <crate>` tiene que pasar sin hardware.
- `decode`: test con un Annex-B mínimo grabado en el repo, sin red.
- `record`: no se exige un MP4 en CI. Quien lo toque deja dicho en el resumen cómo lo probó, o que no pudo abrir el encoder de hardware.
- `app`: `cargo build -p camdesk` tiene que compilar. La prueba con el teléfono, Zoom, Meet y el MP4 es la lista de PLAN.md, no un sustituto de los tests de crate.

## Cuando termines

Dejá el resumen en este orden: qué crate cambiaste, qué invariante tocaste, qué comando corriste y qué no pudiste probar.
