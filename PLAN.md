# Camdesk — plan de implementación

App de escritorio en Rust para Windows 11. Se conecta a la app Android **cellCam** (ya hecha), publica esa imagen como cámara virtual para Zoom y Google Meet, y graba un monitor con el audio del sistema y el micrófono de la PC.

Este archivo es el diseño. Las reglas para no pisarse entre agentes están en [AGENTS.md](AGENTS.md).

## Qué no se construye

- No hay app Android nueva. cellCam vive en `d:\DESARROLLO\Proyectos\cellCam` y **no se modifica**.
- No hay protocolo nuevo. Manda [PROTOCOL.md](d:\DESARROLLO\Proyectos\cellCam\PROTOCOL.md) versión 1. Si el código de cellCam y ese archivo discrepan, se corrige el código de cellCam en su propio repo, no se adapta Camdesk a un desvío.
- La cámara del teléfono no se compone encima de la grabación. La cámara virtual muestra solo el teléfono, ya rotado.
- cellCam no envía audio (`HS_FLAG_AUDIO` está reservado y no hace nada). Zoom y Meet usan el micrófono que el usuario elige dentro de cada app.
- Quedan fuera: HEVC, mDNS, escenas, filtros, streaming, micrófono virtual, instalar la cámara para todos los usuarios.

## Cómo conecta cellCam

El teléfono es el servidor. Camdesk es el cliente. Un solo cliente: si el plugin de OBS está conectado, cellCam rechaza a Camdesk.

| Dato | Valor |
|---|---|
| Puerto | 4747 |
| Transporte | TCP, `TCP_NODELAY`, big-endian |
| Handshake | 16 bytes, magic `CCAM` = `0x4343414D` |
| Frame | 20 bytes, magic `FRM0` = `0x46524D30`, luego el payload |
| Video | H.264 Annex-B. `CODEC_CONFIG` = SPS/PPS. Después, un IDR |
| Rotación | Bits 0-1 del campo `reserved` de la cabecera: 0, 90, 180, 270 grados horarios. La aplica el cliente |
| PTS | Microsegundos, origen arbitrario. Rebasar contra el primer frame |
| Control | Tipo 2, payload de 8 bytes. En v1 solo `CTRL_REQUEST_KEYFRAME` (1) hace algo |
| Congestión | El teléfono puede vaciar la cola y saltar el PTS. No es un error |

Pedido por defecto: H.264, 1280x720, 30 fps, cámara trasera, flag de keyframe. Hay selector de 720p/1080p y de cámara trasera/frontal. Si el teléfono responde `STATUS_ADJUSTED`, se usan el ancho y el alto devueltos.

Conexión, igual que el plugin `plugin/src/cellcam-source.c`:

- **Automática.** Si `adb devices` ve un teléfono: `adb forward tcp:4747 tcp:4747` y conectar a `127.0.0.1`. Si no, Wi-Fi a la IP que muestra cellCam.
- **Solo USB** y **Solo Wi-Fi**.
- `adb` se busca en el PATH y en `%LOCALAPPDATA%\Android\Sdk\platform-tools\adb.exe`. Al soltar el USB: `adb forward --remove`.
- Si el socket cae, reintentar cada 2 segundos mientras la sesión siga activa.
- Si el magic no es `FRM0` o el largo pasa de 4 MiB: buscar el siguiente `FRM0`, descartar hasta un keyframe y enviar `CTRL_REQUEST_KEYFRAME`.

Hasta recibir el IDR que sigue al SPS/PPS, no se muestra nada. Un frame P de una sesión anterior se ve como basura.

## Árbol

```
camdesk/
  PLAN.md
  AGENTS.md
  desktop/                  workspace Cargo
    crates/protocol/        bytes del socket, sin FFmpeg ni UI
    crates/decode/          H.264 → BGRA, rotación
    crates/vcam-shm/        contrato de memoria compartida (leer y escribir)
    crates/record/          pantalla + audios → MP4
    crates/app/             egui, arranque, cablea el resto
  vcam/                     DLL IMFMediaSource, solo lee vcam-shm
```

`protocol`, `vcam-shm` y `record` no se importan entre sí. Se pueden escribir en paralelo. `decode` depende de los frames que describe `protocol`. `app` es el único que une todo. `vcam` depende solo del layout de `vcam-shm`.

## Memoria compartida de la cámara virtual

Archivo mapeado: `%LOCALAPPDATA%\Camdesk\cellcam-vcam.bin`.

Cabecera de 64 bytes, little-endian (esto es IPC local de Windows, no el protocolo del teléfono):

| Offset | Campo | Tipo |
|---:|---|---|
| 0 | magic `CVC1` = `0x43564331` | u32 |
| 4 | version = 1 | u32 |
| 8 | width | u32 |
| 12 | height | u32 |
| 16 | stride de Y (NV12) | u32 |
| 20 | slot activo, 0 o 1 | u32 |
| 24 | sequence del slot 0 | u64 |
| 32 | sequence del slot 1 | u64 |
| 40 | pts en unidades de 100 ns | i64 |
| 48 | reservado, ceros | 16 bytes |

Después, dos slots NV12. Cada slot es `stride * height` de Y más `stride * height / 2` de UV intercalado. `width` y `height` son pares.

Seqlock: el escritor incrementa `sequence` del slot a impar, escribe el NV12, incrementa a par. El lector solo usa un slot con sequence par y estable tras dos lecturas iguales. Si no hay frame nuevo, repite el último.

Si el handshake cambia el tamaño, el escritor recrea el archivo y la cámara virtual se vuelve a registrar.

## Cámara virtual

Windows 11 build 22000 o más nuevo (esta máquina es 26200). `MFCreateVirtualCamera`:

- Nombre: `cellCam`. Windows agrega el sufijo "Windows Virtual Camera".
- `MFVirtualCameraLifetime_Session` y `MFVirtualCameraAccess_CurrentUser`. Existe mientras Camdesk está abierto. Sin administrador.
- La DLL `vcam` es el `IMFMediaSource`. Se registra en `HKCU` la primera vez.
- Entrega NV12 con timestamps desde cero: `frame * 1/30 s`, y clean point en cada muestra, para que Meet no congele la imagen.
- Si Zoom o Meet no listan la cámara, el respaldo es el filtro DirectShow [softcam](https://github.com/tshino/softcam), una sola vez y con administrador. No se implementan los dos a la vez. Primero la API de Windows 11.

## Grabación

- Pantalla: `windows-capture` (Windows.Graphics.Capture), monitor elegido en la UI.
- Audio del sistema: WASAPI loopback del dispositivo de reproducción por defecto.
- Micrófono: WASAPI del dispositivo de captura elegido. Por defecto, el de comunicaciones. No se escucha por los parlantes.
- Las dos pistas pasan a 48 kHz estéreo float, se suman y se recortan. Cada una se puede apagar.
- Salida: `Videos/Camdesk/grabacion-YYYYMMDD-HHMMSS.mp4`.
- Encoder de video, en este orden: NVENC, AMF, Quick Sync, `h264_mf`. `libx264` solo si no hay encoder de hardware.
- Audio AAC. Reloj común: `QueryPerformanceCounter`.
- Si el encode se atrasa, se descarta el frame de pantalla más viejo.
- Las DLL de FFmpeg van junto al exe.

## Arranque

La ventana egui abre sola. FFmpeg, la cámara virtual, WASAPI y la captura de pantalla se crean al conectar el teléfono o al pulsar Grabar. Objetivo: ventana usable en menos de un segundo. OBS tarda varios segundos porque carga Qt, plugins y la escena; Camdesk no hace eso.

En uso, el costo grande es el mismo que en OBS: el teléfono codifica y el PC decodifica H.264. Sin mezclador de escenas, el preview gasta un poco menos. La latencia hacia Zoom es la del protocolo más un frame de la cámara virtual.

## UI

Español. Controles de la primera versión:

- Modo de conexión: Automática, Solo USB, Solo Wi-Fi. Campo de IP.
- Resolución 720p o 1080p. Cámara trasera o frontal. Botón Conectar.
- Preview de la imagen ya rotada. Estado: conectando, en vivo, desconectado.
- Monitor, casillas de audio del sistema y de micrófono, desplegable de micrófono.
- Grabar / Detener y el tiempo transcurrido.

## Prueba de aceptación

1. cellCam en Iniciar stream. Conectar por Wi-Fi (misma red, sin aislamiento de clientes) y por USB con depuración.
2. La preview queda derecha al girar el teléfono.
3. Zoom, configuración de video, muestra cellCam y la imagen se mueve. Lo mismo en el selector de cámara de Meet en Chrome.
4. Grabar 15 segundos con un video sonando y hablando al micrófono. El MP4 muestra el monitor y se oyen las dos cosas.
5. Cortar el Wi-Fi: estado desconectado, la cámara virtual se queda en el último cuadro y vuelve al reconectar.

Los crates `protocol` y `vcam-shm` se prueban sin teléfono y sin Zoom, con fixtures de bytes.
