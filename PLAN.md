# Camdesk — plan de implementación

App de escritorio en Rust para Windows 11. Se conecta a la app Android **cellCam** (ya hecha), publica esa imagen como cámara virtual para Zoom y Google Meet, y graba un monitor con el audio del sistema y el micrófono de la PC.

Este archivo es el diseño. Las reglas para no pisarse entre agentes están en [AGENTS.md](AGENTS.md).

## Qué no se construye

- No hay app Android nueva. cellCam vive en `d:\DESARROLLO\Proyectos\cellCam` y **no se modifica**.
- No hay protocolo nuevo. Manda el `PROTOCOL.md` de cellCam, versión 1 (`d:\DESARROLLO\Proyectos\cellCam\PROTOCOL.md`). La copia de lectura para este repo es [docs/PROTOCOL-v1.md](docs/PROTOCOL-v1.md) (copia de cellCam; no se edita aquí). Si el código de cellCam y ese archivo discrepan, se corrige el código de cellCam en su propio repo, no se adapta Camdesk a un desvío.
- La cámara del teléfono no se compone encima de la grabación. La cámara virtual muestra solo el teléfono, ya rotado.
- cellCam no envía audio (`HS_FLAG_AUDIO` está reservado y no hace nada). Zoom y Meet usan el micrófono que el usuario elige dentro de cada app.
- Quedan fuera: HEVC, mDNS, escenas, filtros salvo los dos de "Efectos", reemplazo del fondo por una imagen, streaming, micrófono virtual, instalar la cámara para todos los usuarios, `libx264`.

## Cómo conecta cellCam

El teléfono es el servidor. Camdesk es el cliente. Un solo cliente: si el plugin de OBS está conectado, cellCam rechaza a Camdesk.

| Dato | Valor |
|---|---|
| Puerto | 4747 |
| Transporte | TCP, `TCP_NODELAY`, big-endian |
| Handshake | 16 bytes, magic `CCAM` = `0x4343414D` |
| Frame | 20 bytes, magic `FRM0` = `0x46524D30`, luego el payload |
| Video | H.264 Annex-B. `CODEC_CONFIG` = SPS/PPS. Después, un IDR |
| Rotación | Bits 0-1 del campo `reserved` de la cabecera: 0, 90, 180, 270 grados horarios. La aplica el cliente. Puede cambiar en cualquier frame |
| PTS | Microsegundos, origen arbitrario. Rebasar contra el primer frame |
| Control | Tipo 2, payload de 8 bytes. En v1 solo `CTRL_REQUEST_KEYFRAME` (1) hace algo |
| Congestión | El teléfono puede vaciar la cola y saltar el PTS. No es un error |

Pedido por defecto: H.264, 1280x720, 30 fps, cámara trasera, flag de keyframe. Hay selector de 720p/1080p y de cámara trasera/frontal. Si el teléfono responde `STATUS_ADJUSTED`, se usan el ancho y el alto devueltos. Ese tamaño es el del decode y la preview; la cámara virtual no cambia (ver "Lienzo").

Conexión, igual que el plugin `plugin/src/cellcam-source.c`. Todo esto vive en el crate `link`:

- **Automática.** Si `adb devices` ve un teléfono en estado `device`: `adb -s <serial> forward tcp:4747 tcp:4747` y conectar a `127.0.0.1`. Si no, Wi-Fi a la IP que muestra cellCam.
- **Solo USB** y **Solo Wi-Fi**.
- `adb` se busca en el PATH y en `%LOCALAPPDATA%\Android\Sdk\platform-tools\adb.exe`. Nunca se llama desde el hilo de la UI: levantar el servidor de adb puede tardar segundos.
- Si `adb devices` muestra `unauthorized`, el estado es "teléfono no autorizado", no "desconectado". Con varios teléfonos se usa el primero en estado `device`.
- Al soltar el USB o al cerrar la sesión: `adb -s <serial> forward --remove tcp:4747`. Nunca `--remove-all`: puede haber otros forwards de Android Studio.
- Si el socket cae, reintentar cada 2 segundos mientras la sesión siga activa.
- Si el magic no es `FRM0` o el largo pasa de 4 MiB: buscar el siguiente `FRM0`, descartar hasta un keyframe y enviar `CTRL_REQUEST_KEYFRAME`.
- Si el canal hacia `app` está lleno, el frame se pierde. Un P perdido deja basura hasta el próximo IDR, así que se hace lo mismo: descartar hasta un keyframe y enviar `CTRL_REQUEST_KEYFRAME`. El SPS/PPS pasa aunque se esté descartando.
- adb se lanza con `CREATE_NO_WINDOW`: la app no tiene consola y adb es un programa de consola.
- Rechazo por otro cliente conectado y versión incompatible: se detectan como diga `PROTOCOL.md` y tienen su propio estado.

Hasta recibir el IDR que sigue al SPS/PPS, no se muestra nada. Un frame P de una sesión anterior se ve como basura. Cada reconexión reinicia el decoder.

## Árbol

```
camDesk/
  PLAN.md
  AGENTS.md
  docs/PROTOCOL-v1.md       copia de lectura del protocolo de cellCam
  desktop/                  workspace Cargo
    Cargo.toml              miembros del workspace; dueño: Aplicación
    crates/protocol/        bytes del socket, sin red, sin FFmpeg ni UI
    crates/link/            TCP, adb, reintento, resync; entrega frames y estado
    crates/decode/          H.264 → BGRA rotado (preview) y NV12 1920x1080 (cámara)
    crates/fx/              desenfoque de fondo NVIDIA y corrección de color, sobre BGRA
    crates/vcam-shm/        contrato de memoria compartida (leer y escribir)
    crates/vcam/            DLL IMFMediaSource + registro de la cámara virtual
    crates/record/          pantalla + audios → MP4
    crates/app/             egui, arranque, cablea el resto (binario `camdesk`)
  spikes/vcam/              prueba descartable del paso 0
```

Dependencias permitidas:

```
protocol   ← link, decode
vcam-shm   ← vcam, app
record     ← app
fx         ← app
link, decode, fx, vcam, record, vcam-shm ← app
```

`protocol`, `vcam-shm`, `record` y `fx` no se importan entre sí. `link` y `decode` dependen solo de `protocol`. `vcam` depende solo de `vcam-shm`. `app` es el único que une todo.

`vcam` es Rust (`windows-rs`), `crate-type = ["cdylib", "rlib"]`: la DLL la carga Windows; la parte `rlib` le da a `app` el alta y la baja de la cámara virtual, para que `app` no toque Media Foundation.

### Plataformas

- Objetivo: `x86_64-pc-windows-msvc`, Windows 11 22000+.
- `protocol`, `vcam-shm` y `link` compilan y pasan `cargo test` también en macOS y Linux. Lo específico de Windows (ACL del archivo mapeado) va detrás de `#[cfg(windows)]`.
- `fx`: la corrección de color es Rust puro y se prueba en cualquier sistema; el desenfoque NVIDIA va detrás de `#[cfg(windows)]`.
- `decode`, `vcam`, `record` y `app` se compilan y prueban en Windows.

## Firmas públicas iniciales

Esqueleto para que todos arranquen a la vez. Los campos exactos del handshake y la cabecera salen de `PROTOCOL.md`; aquí solo se fija la forma. Cualquier cambio se anota aquí en el mismo cambio que el código.

```rust
// protocol — sin E/S
pub const PORT: u16 = 4747;
pub enum Rotation { R0, R90, R180, R270 }
pub enum HandshakeStatus { Ok, Adjusted, Version, Codec, Camera, Other(u8) }
pub enum ProtocolError { BadMagic, Desync }
pub struct HandshakeRequest { pub version: u8, pub codec: u8, pub width: u16, pub height: u16, pub fps: u8, pub camera: u8, pub flags: u8 }
pub struct HandshakeResponse { pub status: HandshakeStatus, pub version: u8, pub codec: u8, pub width: u32, pub height: u32, pub fps: u8, pub camera: u8, pub flags: u8 }
pub enum FrameKind { Video, CodecConfig, Audio, Control, Other(u8) }
pub struct FrameHeader { pub kind: FrameKind, pub keyframe: bool, pub rotation: Rotation, pub pts_us: i64, pub len: u32 }
pub struct Frame { pub header: FrameHeader, pub payload: Vec<u8> }
impl HandshakeRequest { pub fn video(width: u16, height: u16, fps: u8, front: bool) -> Self; pub fn to_bytes(self) -> [u8; 16]; pub fn parse(b: &[u8; 16]) -> Result<Self, ProtocolError>; }
impl HandshakeResponse { pub fn parse(b: &[u8; 16]) -> Result<Self, ProtocolError>; pub fn to_bytes(&self) -> [u8; 16]; }
pub struct FrameParser;            // push(&[u8]), next() -> Option<Result<Frame, ProtocolError>>; resync a FRM0
pub fn request_keyframe() -> Vec<u8>;

// link — usa std::net y procesos adb, sin FFmpeg ni UI
pub enum Mode { Auto, Usb, Wifi(std::net::Ipv4Addr) }
pub enum Resolution { P720, P1080 }
pub enum Camera { Back, Front }
pub struct SessionConfig { pub mode: Mode, pub resolution: Resolution, pub camera: Camera, pub wifi: Option<std::net::Ipv4Addr> }
pub enum Transport { Usb, Wifi }
pub enum Status { Conectando, EnVivo { width: u32, height: u32, camera: Camera, transport: Transport }, Desconectado, Ocupado, NoAutorizado, Incompatible }
pub enum Event { Status(Status), Frame(protocol::Frame) }
pub struct Session;                // start(cfg) -> (Session, Receiver<Event>); request_keyframe(); Drop = cerrar + forward --remove
```

`wifi` es el respaldo del modo automático cuando no hay un `device` de adb. El estado 4 del teléfono (`STATUS_ERROR_CAMERA`, también el rechazo de un segundo cliente) se informa como `Ocupado`.

```rust
// decode — carga FFmpeg en Decoder::new(), nunca antes
pub struct Decoder;                // new() -> Result; push(&Frame) -> Result<Option<Decoded>, DecodeError>; reset()
pub struct Decoded { pub width: u32, pub height: u32, pub bgra: Vec<u8>, pub pts_us: i64 } // ya rotado
pub enum DecodeError { NeedKeyframe, Message(String) }
impl Decoder { pub fn to_canvas_nv12(&mut self, d: &Decoded, y: &mut [u8], uv: &mut [u8], stride: usize); } // 1920x1080, barras Y=16 UV=128

// fx — sobre BGRA en memoria; no conoce cellCam, el decoder ni el lienzo
pub struct ColorParams { pub brightness: f32, pub contrast: f32, pub saturation: f32, pub temperature: f32 } // -1.0..=1.0, 0 = neutro
pub struct ColorCorrector;         // new(); set(ColorParams); apply(bgra: &mut [u8], width, height, stride); neutro = no toca el frame
pub enum BlurAvailability { Available, NoNvidiaGpu, NoRuntime }
pub fn blur_availability() -> BlurAvailability;  // chequeo liviano, no carga el runtime
pub struct FxError { pub message: String }
pub struct BackgroundBlur;         // new() -> Result (carga runtime y modelos); set_strength(f32); apply(...) -> Result<(), FxError>; notice() -> Option<&str>
// notice() avisa sin apagar el efecto, por ejemplo al pasar a modo rendimiento.

// vcam-shm
pub const WIDTH: u32 = 1920; pub const HEIGHT: u32 = 1080; pub const STRIDE: u32 = 1920;
pub fn default_path() -> std::path::PathBuf;    // %ProgramData%\Camdesk\cellcam-vcam.bin
pub struct FrameInfo { pub pts_100ns: i64, pub sequence: u64 }
pub struct Writer;                 // create(path) -> io::Result; open_existing(path); write(pts_100ns, |y, uv| …)
pub struct Reader;                 // open(path) -> io::Result; read_latest(&mut y, &mut uv) -> Option<FrameInfo>
// El lector solo hace loads Acquire. No usa InterlockedCompareExchange sobre el mapa de solo lectura.

// vcam (rlib). create() no corre al abrir la ventana: lo llama app al pulsar Conectar.
pub struct VcamError { pub message: String }
pub struct VirtualCamera;          // create() -> Result<Self, VcamError>; Drop = Stop, Remove, Shutdown
// DLL copiada a %ProgramData%\Camdesk\cellcam.dll, con lectura y ejecución para LOCAL SERVICE.
// Log al lado del archivo mapeado: cellcam-vcam.log. CLSID {8F4E2A11-6C3D-4B9E-9170-2A5B8C4D6E11} en HKLM.
// La primera vez HKLM pide administrador. Remove no borra esa clave. Sin el archivo mapeado, la cámara entrega negro.

// record — no conoce cellCam
pub struct MonitorId(pub String);  // número de pantalla de Windows (\\.\DISPLAYn)
pub struct MicId(pub String);
pub struct OutputId(pub String);   // id del endpoint de reproducción
pub struct MonitorInfo { pub id: MonitorId, pub name: String, pub width: u32, pub height: u32, pub primary: bool }
// name es único: «1 · modelo», «2 · modelo».
pub struct MicInfo { pub id: MicId, pub name: String }
pub struct OutputInfo { pub id: OutputId, pub name: String }
pub fn monitors() -> Vec<MonitorInfo>;
pub fn outputs() -> Vec<OutputInfo>;    // la primera es la predeterminada
pub fn microphones() -> Vec<MicInfo>;   // el primero es el de comunicaciones por defecto
pub fn default_output() -> std::path::PathBuf; // Videos\Camdesk\grabacion-YYYYMMDD-HHMMSS.mp4
pub fn video_encoder_name() -> Result<String, RecordError>; // abre y cierra NVENC, AMF, Quick Sync o h264_mf
pub struct RecordConfig { pub monitor: MonitorId, pub system_audio: bool, pub system_device: Option<OutputId>, pub mic: Option<MicId>, pub output: std::path::PathBuf }
// system_device None = la predeterminada. start() falla si la elegida no está.
pub struct ScreenPreview { pub width: u32, pub height: u32, pub rgba: Vec<u8> } // 960 de ancho como mucho
pub struct ScreenWatch;            // start(MonitorId) -> Result; monitor(); take_preview(); error(); Drop cierra la captura
pub struct Recording;              // start(cfg) -> Result; elapsed(); encoder(); levels() -> (f32, f32); take_preview() -> Option<ScreenPreview>; error(); stop() -> Result<PathBuf>
```

`decode` señala con `DecodeError` cuando necesita un keyframe; `app` llama a `Session::request_keyframe()`.

## Lienzo de la cámara virtual

`decode` respeta la matriz y el rango del cuadro H.264 al generar BGRA. Convierte el lienzo a NV12 con matriz BT.709 y rango limitado. `vcam` declara ambos atributos en el tipo de video de Media Foundation.

La cámara virtual es siempre **1920x1080 NV12 a 30 fps**. No cambia con la rotación, con el selector 720p/1080p ni con `STATUS_ADJUSTED`. Zoom y Meet pierden la cámara si cambia el formato.

- `decode` usa libswscale con filtrado bicúbico e interpolación de crominancia para escalar el frame ya rotado a NV12. Entra completo en 1920x1080, centrado, con barras negras (Y=16, UV=128).
- Teléfono horizontal a 720p: se escala a 1920x1080. Teléfono vertical: unos 608x1080 al centro, con barras a los costados.
- La preview de la UI usa el BGRA a tamaño nativo, no el lienzo, ya con los efectos aplicados.

## Efectos

Orden fijo por frame: decode → rotación → desenfoque de fondo → corrección de color → preview y lienzo NV12. `app` hace la cadena llamando a `decode` y a `fx`. El desenfoque va después de la rotación porque el modelo espera a la persona derecha, y antes del color para que la segmentación vea la imagen original.

Los efectos afectan a la preview y a la cámara virtual. No afectan a la grabación de pantalla. Los ajustes se recuerdan entre sesiones en `%LOCALAPPDATA%\Camdesk\config.toml` (dueño: `app`).

### Desenfoque de fondo (NVIDIA)

- Por qué NVIDIA y no otra cosa: el desenfoque de Zoom y Meet es peor. Windows Studio Effects solo existe en equipos con NPU y solo se aplica a cámaras físicas, no a una virtual. La app NVIDIA Broadcast encadenada da el mismo resultado, pero exige otra app abierta y otra cámara virtual en el medio.
- SDK: NVIDIA Maxine Video Effects. Efecto `GreenScreen` (máscara de la persona) y luego `BackgroundBlur` (imagen + máscara). FFI escrito a mano en `fx` contra `NVVideoEffects.dll`, cargada en tiempo de ejecución.
- Requisitos: GPU NVIDIA RTX (Turing o más nueva), driver actual y el runtime "NVIDIA Video Effects" instalado por el usuario desde el sitio de NVIDIA. **Camdesk no redistribuye DLL ni modelos de NVIDIA.** Antes de distribuir Camdesk, revisar la licencia del SDK.
- Ubicación del runtime: variable `NV_VIDEO_EFFECTS_PATH` y, si no está, `C:\Program Files\NVIDIA Corporation\NVIDIA Video Effects`. Es lo que hace OBS; confirmarlo con el runtime instalado.
- `blur_availability()` solo busca el archivo del runtime y un adaptador NVIDIA por DXGI. El chequeo definitivo (RTX, versión del driver, modelos) es `BackgroundBlur::new()`.
- Si no se cumple algún requisito, la casilla aparece deshabilitada con el motivo y el enlace de descarga del runtime. No hay desenfoque de respaldo.
- Controles: casilla y fuerza de 0 a 100 %. Modo calidad por defecto; si el efecto no sostiene 30 fps, pasa a modo rendimiento y se avisa en la UI.
- Camino de datos en v1: BGRA en CPU → GPU (`NvCVImage_Transfer`) → `GreenScreen` → `BackgroundBlur` → CPU. El decode sigue en CPU. Mantener todo en GPU (decode D3D11 + interop CUDA) queda como optimización posterior.
- Si cambia el tamaño del frame (rotación, 720p/1080p), `apply()` se reconfigura sola. El primer frame después del cambio puede tardar más.
- Si el efecto falla en medio de la sesión (driver, memoria de GPU), se apaga, se avisa en la UI y el video sigue sin desenfoque. La cámara virtual nunca se corta por un efecto.

### Corrección de color

- Brillo, contraste, saturación y temperatura (frío a cálido). Un slider por control y un botón Restablecer.
- En CPU sobre BGRA: una tabla de 256 entradas por canal para brillo, contraste y temperatura, y mezcla con la luma para la saturación. Con todo en neutro no se toca el frame.

## Memoria compartida de la cámara virtual

Archivo mapeado. Ruta candidata: `%ProgramData%\Camdesk\cellcam-vcam.bin`, con ACL de lectura para `LOCAL SERVICE`. **La confirma el paso 0**; hasta entonces `default_path()` es la única fuente de verdad y nadie escribe la ruta a mano.

El archivo se crea una sola vez por ejecución de Camdesk, al conectar por primera vez, con tamaño fijo. No se recrea en la sesión.

Cabecera de 64 bytes, little-endian (esto es IPC local de Windows, no el protocolo del teléfono):

| Offset | Campo | Tipo |
|---:|---|---|
| 0 | magic `CVC1` = `0x43564331` | u32 |
| 4 | version = 1 | u32 |
| 8 | width = 1920 | u32 |
| 12 | height = 1080 | u32 |
| 16 | stride de Y (NV12) = 1920 | u32 |
| 20 | slot activo, 0 o 1 | u32 |
| 24 | sequence del slot 0 | u64 |
| 32 | pts del slot 0, unidades de 100 ns | i64 |
| 40 | sequence del slot 1 | u64 |
| 48 | pts del slot 1, unidades de 100 ns | i64 |
| 56 | reservado, ceros | 8 bytes |

Slots a partir del offset 64. Cada slot es `stride * height` de Y más `stride * height / 2` de UV intercalado: 3 110 400 bytes. Slot 0 en 64, slot 1 en 3 110 464. Tamaño total: 6 220 864 bytes.

`width`, `height` y `stride` se escriben al crear el archivo y no cambian. El lector los valida contra las constantes y, si no coinciden o `magic`/`version` no son los esperados, entrega negro.

Seqlock. Las `sequence` se acceden como `AtomicU64` (offsets alineados a 8) y `slot activo` como `AtomicU32`.

Escritor (un solo hilo):
1. `s` = el slot que **no** es el activo.
2. Publicar una sequence par mayor que la de los dos slots (el contador es global; si cada slot solo suma 2, los dos quedan en el mismo número y el lector cree que el cuadro no cambió). Guardarla impar (`+1` pendiente) con `Release`.
3. Escribir NV12 y `pts[s]`.
4. Guardar la sequence par con `Release`.
5. `activo = s` con `Release`.

Lector:
1. `s = activo` con `Acquire`. `a = seq[s]` con `Acquire`. Si `a` es impar, reintentar.
2. Copiar NV12 y `pts[s]` a un buffer propio.
3. `fence(Acquire)`, `b = seq[s]`. Si `a == b`, el frame es válido. Si no, reintentar.
4. Hasta 3 intentos. Si ninguno sale, o `a` es igual al último frame entregado, se repite el último frame.

Estado inicial: al crear el archivo, el escritor llena los dos slots de negro con `sequence = 2`. La cámara muestra negro hasta el primer frame del teléfono y se queda en el último cuadro si la conexión cae.

Cambiar un offset rompe la cámara: se sube `version` y se actualizan `vcam-shm` y `vcam` en el mismo cambio.

## Cámara virtual

Si Windows impide reemplazar la DLL porque está en uso, Camdesk puede iniciar la cámara con la copia que ya está registrada en la ruta prevista y deja constancia en el log. Volverá a intentar la actualización al iniciar la cámara en la próxima ejecución.

La duración de cada muestra es la diferencia entre dos posiciones consecutivas a 30 fps (333333 o 333334 unidades de 100 ns). El origen de la sesión solo se suma al tiempo de presentación.

Windows 11 build 22000 o más nuevo (la máquina de prueba es 26200). `MFCreateVirtualCamera`:

- Nombre: `cellCam`. Cómo lo muestran Zoom y Meet se anota tras el paso 0.
- `MFVirtualCameraLifetime_Session` y `MFVirtualCameraAccess_CurrentUser`. Existe mientras Camdesk está abierto.
- La DLL del crate `vcam` es el `IMFMediaSource`. **No corre en el proceso de Camdesk**: la carga el servicio Frame Server (`svchost`, cuenta `LOCAL SERVICE`). Consecuencias: no puede usar `%LOCALAPPDATA%` del usuario, no puede leer archivos con ACL del perfil del usuario, y su registro COM tiene que ser visible para ese servicio.
- Entrega NV12 1920x1080 a 30 fps, con clean point en cada muestra. El tiempo de la muestra es el origen más `frame * 1/30 s`. Si `Start` trae la posición en `VT_I8`, ese valor es el origen. Si viene vacía, el origen es `MFGetSystemTime()` tomado una vez en `Start`: un cero absoluto queda en el pasado y Frame Server descarta todas las muestras. `RequestSample` espera a que ese tiempo esté vencido, así el contador no se adelanta al reloj. El `pts` de la memoria compartida es informativo.
- Log de la DLL: `OutputDebugString` y un archivo junto al archivo mapeado. No hay otra forma de depurar dentro de Frame Server.

### Paso 0 — prueba de la cámara virtual (antes de escribir `vcam`)

En `spikes/vcam/`, en Windows. Una DLL mínima que lee un patrón de un archivo mapeado y un exe que llama a `MFCreateVirtualCamera`. Responde, y se anota aquí:

1. ¿Alcanza con registrar el CLSID en `HKCU`, o Frame Server solo lo ve en `HKLM` (administrador)?
2. ¿Desde qué carpeta puede cargar la DLL Frame Server? (Perfil del usuario, `%ProgramData%`, `%ProgramFiles%`.)
3. ¿La ruta `%ProgramData%\Camdesk\` con ACL de lectura para `LOCAL SERVICE` funciona para el archivo mapeado?
4. ¿Zoom y Meet en Chrome listan la cámara y la imagen se mueve?

Si alguna respuesta exige administrador o Zoom/Meet no la listan, **se para y se decide con el usuario** entre "administrador una sola vez" y el respaldo DirectShow [softcam](https://github.com/tshino/softcam). No se implementan los dos a la vez.

Resultados del paso 0, en esta PC (Windows 11 build 26200). El usuario eligió administrador una sola vez.

1. El CLSID en `HKCU` no alcanza. Sin elevar, `MFCreateVirtualCamera` (`CurrentUser`) devuelve `S_OK` y `Start` devuelve `0x80070003` si el CLSID no está en `HKLM`. Frame Server corre como `LOCAL SERVICE` y no ve `HKCU`. Escribir el CLSID en `HKLM` sin elevar devuelve `0x80070005`. Con el CLSID ya escrito en `HKLM\Software\Classes`, un proceso sin elevar obtiene `Start` `S_OK`. La cámara aparece como `cellCam (Windows Virtual Camera)`: Windows agrega ese sufijo. `IMFVirtualCamera::Remove` no borra la clave; después de una sonda exitosa seguía en `HKLM`.
2. Frame Server carga la DLL como `LOCAL SERVICE` (`S-1-5-19`) desde `%ProgramData%\Camdesk\`. Desde el perfil del usuario, con la ACL de siempre, `Start` devuelve `0x80070005` y el servicio no carga la DLL. Si a ese archivo se le agrega lectura y ejecución para `LOCAL SERVICE`, el servicio sí la carga. `%ProgramFiles%\Camdesk` también la carga; crear esa carpeta pide administrador. `%ProgramData%\Camdesk` no.
3. Con la ACL de lectura, el servicio abrió `%ProgramData%\Camdesk\cellcam-vcam.bin` (`shm=ok`, error 0). El primer lector usaba `InterlockedCompareExchange` sobre un mapeo de solo lectura y Frame Server terminaba con `0xC0000005` en `cellcam_spike.dll`. El lector del spike ahora lee el seqlock con cargas volátiles y una barrera, sin escribir en la vista. Repetido el arranque, no hubo otro error de aplicación desde las 21:00. El cliente de Media Foundation recibió 7 muestras y la barra cambió de columna: 1584 y después 1800, y en otra corrida 1560 y después 1776.
4. Zoom no está instalado. En Chrome el selector lista `cellCam (Windows Virtual Camera)` junto a `OBS Virtual Camera`. `getUserMedia` sobre cellCam devolvió `NotReadableError`. En ese intento Frame Server igual inicializó el dispositivo con `hr` 0, una secuencia, 1920x1080 y 30 fps. La página no mostró la barra: siguió con OBS Virtual Camera. Meet no se abrió. La aplicación Cámara de Windows, abierta con la sesión ya cerrándose, respondió `0xA00F4244` (`NoCamerasAreAttached`).

El crate `vcam` sigue vacío. El CLSID de `HKLM` no quedó registrado. No quedaron la DLL ni el archivo mapeado en `%ProgramData%\Camdesk` ni en `%ProgramFiles%\Camdesk`. El lector del crate `vcam-shm` no puede usar una operación que escribe sobre el mapeo de solo lectura: es lo que tumbó a Frame Server.

El paso 2 usa ese resultado. La DLL del crate es `IMFMediaSource` en Rust y lee NV12 con `vcam_shm::Reader`. El origen del reloj es la posición de `Start` si viene en `VT_I8`, y si no `MFGetSystemTime()` de ese arranque; cada muestra suma `frame * 1/30 s` y lleva clean point. Se copia a `%ProgramData%\Camdesk\cellcam.dll`. El CLSID `{8F4E2A11-6C3D-4B9E-9170-2A5B8C4D6E11}` queda en `HKLM` después del alta: `Remove` no lo borra, y la siguiente vez no pide administrador. En esta PC, un lector de Media Foundation listó `cellCam (Windows Virtual Camera)` y vio la barra pasar de la columna 200 a la 900. Zoom no está instalado y Meet no se abrió.

El paso 3 cablea `app`. Conectar crea el decoder, el archivo mapeado y llama a `VirtualCamera::create()`. Cada frame sigue rotación, desenfoque, color, preview y lienzo NV12. Esa cadena corre en un hilo propio de `app` (`camdesk-video`), no en el de egui: la cámara virtual no depende de que la ventana se redibuje, y soltar la sesión no traba la ventana. Ese hilo es dueño de la `Session`, del decoder, del `Writer` y del `BackgroundBlur`, que se crea ahí al prender la casilla. La UI le manda órdenes (conectar, desconectar, color, desenfoque) y recibe los estados y el último cuadro ya con efectos, solo para la vista previa. Soltar la sesión espera a `link`, que puede estar en adb o en el handshake; eso pasa en ese hilo, y las órdenes van en orden, así que el `forward --remove` de una sesión termina antes de que arranque la siguiente. Antes de un keyframe, `app` pide uno cada 500 ms como mucho. Desconectar cierra la sesión y deja la cámara hasta cerrar la ventana. Abrir la ventana no hace ninguna de esas cargas. El pie dice «En vivo · ancho×alto». El epígrafe nombra USB o Wi-Fi según el camino que conectó, no el selector. Mientras busca el teléfono, el epígrafe dice que reintenta cada 2 segundos y el pie se queda en Conectando hasta que llega la imagen o se cancela. Al cortar, si la imagen sigue en pantalla, el epígrafe dice que Zoom se quedó en esa imagen y el aviso dice que la cámara virtual no pasa a negro. Si el teléfono habla otra versión, no reintenta: el pie queda en «Versión incompatible» y el epígrafe dice que no hay reintento útil. Si otro cliente ya tiene el stream, tampoco reintenta: el pie queda en «Ocupado», el botón vuelve a Conectar y el aviso pide cerrar ese cliente. Si la depuración USB no está aceptada, sigue reintentando: el pie queda en «Teléfono no autorizado», el botón en Cancelar y el aviso pide aceptarla en el teléfono. Sin GPU NVIDIA o sin el runtime, el aviso dice que no hay desenfoque y que la cámara sigue; la casilla queda apagada. Al llegar la imagen, el aviso dice que la vista previa y cellCam la reciben, y que la grabación no. Si el cuadro ya rotado es más alto que ancho, el aviso pasa a «Teléfono vertical» y dice que las barras las agrega Zoom. El pie sigue con el tamaño del stream. Si el desenfoque no sostiene 30 fps, pasa a modo rendimiento y el aviso lo dice; la cámara sigue. Si el efecto falla en medio de la sesión, se apaga y el aviso dice que la cámara sigue.

## Grabación

- Pantalla: `windows-capture` (Windows.Graphics.Capture), monitor elegido en la UI, resolución nativa del monitor.
- **30 fps constantes.** WGC solo entrega frames cuando cambia la pantalla: si no llega uno a tiempo se repite el último. Si el encode se atrasa, se descarta el frame de pantalla más viejo.
- Audio del sistema: WASAPI loopback de la salida elegida. Por defecto, «Salida predeterminada». **El loopback no entrega paquetes en silencio**: el hueco se rellena con ceros según el reloj, si no el audio se corre.
- Micrófono: WASAPI del dispositivo de captura elegido. Por defecto, el de comunicaciones. No se escucha por los parlantes. Mientras se graba, la UI muestra el nivel reciente de cada pista, de silencio a 0 dB. El nivel sube al instante y cae 24 dB por segundo; la barra se dibuja suavizada a unos 60 cuadros por segundo y el número en dB cambia cada 200 ms; al abrir la ventana esas barras quedan vacías y no se abre WASAPI.
- Las dos pistas pasan a 48 kHz estéreo float, se suman y se recortan. Cada una se puede apagar.
- Reloj común: `QueryPerformanceCounter`. Cada pista se alinea a ese reloj; si se corre más de 20 ms, se insertan o descartan muestras.
- Con «Salida predeterminada», si cambia el dispositivo por defecto en medio de la grabación, se reabre y el hueco va en silencio. Una salida elegida a mano no sigue a la predeterminada: si se desconecta, esa pista queda en silencio y se vuelve a buscar cada 500 ms. La grabación no se corta. Si al pulsar Grabar la elegida no está, no se graba y el aviso lo dice.
- La elección se guarda en la config (`system_device`, `system_device_name`). El nombre guardado se muestra sin abrir WASAPI; la lista se arma al desplegarla. No cambia lo que suena en la PC y no se puede cambiar durante una grabación.
- Los monitores se listan numerados, con su tamaño y «(principal)» en el principal. Se recuerdan por número y tamaño (`monitor_id`, `monitor`).
- Salida: carpeta Videos del usuario (`FOLDERID_Videos`) + `Camdesk\grabacion-YYYYMMDD-HHMMSS.mp4`.
- **MP4 fragmentado** (`frag_keyframe+empty_moov+default_base_moof`) para que un cuelgue no deje el archivo ilegible.
- Encoder de video, en este orden: NVENC, AMF, Quick Sync, `h264_mf`. `h264_mf` ya cae al encoder de software de Windows; no se usa `libx264`. Mientras se graba, el pie muestra ese encoder y «30 fps», al lado del tiempo REC. Mientras se graba, la vista previa muestra la pantalla grabada, unas 10 veces por segundo, con las marcas PANTALLA y REC y el monitor en el epígrafe. Sale del mismo lienzo que va al MP4. Si además hay imagen del teléfono, la cámara y la pantalla van en dos marcos lado a lado: la cámara no se compone encima de la grabación. Para ver antes qué se va a grabar está la casilla «Vista previa de la pantalla», debajo del monitor: muestra el monitor elegido, con las marcas PANTALLA y SIN GRABAR y «Se va a grabar …», y cambia al elegir otro. No abre audio ni encoder. La casilla no se recuerda entre sesiones: sin marcarla y sin pulsar Grabar no se captura la pantalla. Después de guardar, debajo de «Grabar» queda «Mostrar en la carpeta», que abre el Explorador con el último MP4 seleccionado; la ruta se recuerda entre sesiones (`last_recording`). Si se graba el monitor donde está Camdesk, la vista previa se ve repetida dentro de sí misma. Al detener, el aviso dice «Grabación guardada» y, al lado, la ruta del MP4. Mientras se graba sin teléfono, el aviso dice «Grabando sin cámara» y que la grabación es otro trabajo. Con el teléfono en vivo, dice «Grabando» con el tiempo y que graba el monitor.
- Audio AAC.

## FFmpeg

- Build **LGPL**, sin componentes GPL. Las DLL van junto al exe.
- Se enlazan con **carga diferida** (`/DELAYLOAD` para cada DLL de FFmpeg, o carga en tiempo de ejecución). Con import libs normales, Windows las carga al arrancar el proceso y se rompe la regla de arranque.
- Las usan `decode` y `record`. Ninguno las toca antes de `Decoder::new()` o `Recording::start()`.

## Arranque

La ventana egui abre sola. FFmpeg, la cámara virtual, el archivo mapeado, adb, WASAPI y la captura de pantalla se crean al conectar el teléfono o al pulsar Grabar. El runtime de NVIDIA se carga recién al activar el desenfoque. Objetivo: ventana usable en menos de un segundo. OBS tarda varios segundos porque carga Qt, plugins y la escena; Camdesk no hace eso.

En uso, el costo grande es el mismo que en OBS: el teléfono codifica y el PC decodifica H.264. Sin mezclador de escenas, el preview gasta un poco menos. La latencia hacia Zoom es la del protocolo más un frame de la cámara virtual.

Log de la app: `%LOCALAPPDATA%\Camdesk\camdesk.log`. La compilación release no abre consola (`windows_subsystem = "windows"`); la de depuración sí.

## UI

Español. Controles de la primera versión:

- Modo de conexión: Automática, Solo USB, Solo Wi-Fi. Campo de IP.
- Resolución 720p o 1080p. Cámara trasera o frontal. Botón Conectar.
- Preview de la imagen ya rotada.
- Selector Rotación. Automática aplica los metadatos del teléfono; 0°, 90°, 180° o 270° fijan el ángulo para la preview y la cámara virtual. Se puede cambiar en vivo y se guarda en la configuración.
- Estado: conectando, en vivo, desconectado, ocupado (otro cliente conectado al teléfono), teléfono no autorizado (aceptar la depuración USB), versión incompatible.
- Efectos: casilla "Desenfocar fondo" y su fuerza (deshabilitada con el motivo si no hay RTX o runtime). Sliders de brillo, contraste, saturación y temperatura, y botón Restablecer.
- Monitor, casillas de audio del sistema y de micrófono, desplegable de salida de audio y de micrófono.
- Grabar / Detener y el tiempo transcurrido.

Diseño vigente: «Camdesk — consola oscura, inspirada en OBS» (lienzo «Camdesk UI»); reemplaza a `docs/camdesk-ux.html`. De arriba abajo: barra con la marca; vista previa en un marco 16:9 con las marcas EN VIVO, REC y «Último cuadro», y el epígrafe debajo; cuatro docks (Conexión, Efectos, Grabación, Controles); pie con el estado, el formato de la cámara y, al grabar, el tiempo y el encoder. Los avisos van en el dock Controles, no arriba. Los tokens están en `app/src/theme.rs`: un solo acento azul para acciones; verde, ámbar y rojo solo para estado. Los controles pintados a mano están en `app/src/widgets.rs` y declaran nombre y estado para el lector de pantalla. Fuentes: Inter y JetBrains Mono si están instaladas en Windows; si no, Segoe UI y Cascadia Mono. No se empaquetan fuentes. La ventana abre en 1280×800 y no baja de 1060×640; en una ventana baja los docks se desplazan.

## Actualizaciones

El repo es público y las versiones se publican como releases de GitHub (`Nabil014/camDesk`). Código en `app/src/update.rs`; publicación en `installer/publish.ps1`.

- La versión sale solo de `desktop/crates/app/Cargo.toml`. La ventana la muestra al lado de la marca y el instalador la recibe del script.
- Cada release lleva `Camdesk-Setup.exe`, `camdesk-update.txt` (`version=` y `sha256=` del instalador) y `camdesk-update.txt.sig` (Ed25519 del manifiesto).
- La app lleva la clave pública. La privada está en `%USERPROFILE%\.camdesk\release-key.pem` de quien publica y nunca en el repo. Sin firma válida, o si el hash no coincide, no se ejecuta nada.
- La app consulta `releases/latest/download/` una vez por ejecución, en un hilo aparte, después de mostrar la ventana. No usa la API de GitHub ni manda datos del usuario. Si falla, queda en el log y no se muestra nada. Una compilación de depuración no consulta salvo con `CAMDESK_UPDATE_URL`.
- Solo ofrece una versión mayor que la propia. Aparece «Actualizar a vX.Y.Z» arriba a la derecha. Nada se descarga ni se instala sin ese clic.
- Mientras se graba o hay una sesión con el teléfono, el botón está deshabilitado y dice por qué. Si eso empieza durante la descarga, queda «Instalar vX.Y.Z» para después.
- Instala con `/SILENT /RELAUNCH=1`: la app se cierra, el instalador reemplaza los archivos y la vuelve a abrir. La config de `%LOCALAPPDATA%` se conserva.
- Descargas en `%LOCALAPPDATA%\Camdesk\updates`.
- La primera publicada es la 1.0.0. Las compilaciones anteriores no saben actualizarse: la 1.0.0 se instala a mano.
- `publish.ps1` compila aparte (`target/dist`) y reemplaza las rutas locales: el instalador público no lleva el usuario de Windows ni las carpetas de quien publica.

## Orden de trabajo

0. Esqueleto del workspace con todos los crates vacíos que compilan (dueño: Aplicación). Copiar `docs/PROTOCOL-v1.md`. Paso 0 de la cámara virtual, en Windows.
1. En paralelo, con las firmas de arriba: `protocol`, `vcam-shm`, `link`, `decode`, `fx`, `record`, ventana de `app`.
2. `vcam`, cuando el paso 0 tenga resultados y `vcam-shm` tenga el `Reader`.
3. `app` cablea todo.
4. Prueba de aceptación.

## Prueba de aceptación

1. cellCam en Iniciar stream. Conectar por Wi-Fi (misma red, sin aislamiento de clientes) y por USB con depuración. Con el plugin de OBS conectado, el estado es "ocupado".
2. La preview queda derecha al girar el teléfono.
3. Zoom, configuración de video, muestra cellCam y la imagen se mueve. Lo mismo en el selector de cámara de Meet en Chrome.
4. Con Zoom mostrando la cámara, girar el teléfono y cambiar 720p/1080p: Zoom no pierde la cámara, la imagen queda derecha con barras.
5. Abrir Zoom antes de conectar el teléfono: muestra negro y luego la imagen.
6. Grabar 15 segundos con un video sonando y hablando al micrófono. El MP4 muestra el monitor y se oyen las dos cosas. Repetir con Zoom usando la cámara al mismo tiempo.
7. Grabar 30 minutos con la pantalla quieta parte del tiempo y sin sonido parte del tiempo: el audio sigue sincronizado al final y el MP4 dura 30 minutos.
8. Cortar el Wi-Fi: estado desconectado, la cámara virtual se queda en el último cuadro y vuelve al reconectar.
9. Abrir la app: ventana usable en menos de 1 s y, en Process Explorer, ninguna DLL de FFmpeg ni de NVIDIA cargada hasta conectar o activar el desenfoque.
10. En una PC con RTX y el runtime: activar el desenfoque con Zoom abierto. El fondo se ve desenfocado en Zoom y en la preview, la cámara sigue a 30 fps y la imagen sigue bien al girar el teléfono.
11. En una PC sin RTX o sin runtime: la casilla está deshabilitada y dice por qué.
12. Mover los sliders de color: el cambio se ve en la preview y en Zoom. Restablecer deja la imagen como sin efectos. Al reabrir la app, los ajustes siguen.

Los crates `protocol`, `vcam-shm` y `link`, y la corrección de color de `fx`, se prueban sin teléfono y sin Zoom, con fixtures de bytes.

Resultados parciales en esta PC (25/11/2026). Zoom no está instalado. Meet lista cellCam; sin imagen del teléfono el cuadro queda negro.

1. USB: `adb` ve el teléfono `e5c31a82` en `device`. Conectar en modo automático pasa a **Ocupado**: otro cliente ya tiene el stream. Wi-Fi no se probó. Más tarde, con un cliente TCP sosteniendo el stream, Conectar pasó a Ocupado y se quedó ahí unos 6 s. En ese rato no había forward para ese teléfono. Al soltar el cliente, el estado volvió a En vivo 1280x720. Después de cerrar se dejó otra vez el forward `e5c31a82 tcp:4747`. Con ese forward quitado solo para esta prueba, un servidor en `127.0.0.1:4747` respondió el handshake con estado de cámara. Solo Wi-Fi a esa IP dejó el pie en «Ocupado», el botón en Conectar, el centro en «Sin imagen.», el epígrafe en «Zoom recibe cellCam a 1920×1080» y el aviso «Teléfono ocupado» / «Otro cliente ya está conectado. Cerrar OBS y volver a conectar.». Al cerrar el servidor siguió en Ocupado, sin volver a Conectando. Se cerró, se restauró la config automática sin IP y el forward `e5c31a82 tcp:4747 tcp:4747`. En Solo USB, con un `adb` que respondió `unauthorized` y sin tocar la autorización real del teléfono, a los 4 s el pie seguía en «Teléfono no autorizado», el botón en Cancelar, el centro en «Sin imagen.», el epígrafe en «Reintento cada 2 segundos» y el aviso «Teléfono no autorizado» / «Aceptar la depuración USB en el teléfono. Camdesk reintenta cada 2 segundos.». Se cerró. `adb devices` sigue en `device`, la config volvió a automático sin IP y el forward no se tocó.
2. Hay preview. En el cuadro vertical el lienzo sigue en 1920x1080, con barras negras a los costados y la imagen al centro. La persona se ve de costado, no derecha. El cuadro llega en 1280x720 con rotación de 90° y, al aplicarla, queda en 720x1280. El acelerómetro marca el lado izquierdo hacia arriba y el lienzo sigue vertical con barras. Forzar la rotación de la pantalla a 90° y a 270° no cambió las barras: cellCam manda el giro del acelerómetro. El teléfono tenía la pantalla apagada y el bloqueo puesto, así que no se recorrieron las cuatro posiciones. Se dejó otra vez la rotación automática. Con la pantalla despierta y sin mover el teléfono, el acelerómetro sigue en x=−9,55, y=0,52, z=2,0 y el lienzo no cambia: el contenido mide 608 px de ancho desde x=656, la secuencia pasa de 134 a 196 y el estado se queda en En vivo 1280x720. La orientación de la pantalla sigue en 0 y no aparece «Rotacion de salida». Despertar la pantalla no endereza la imagen. Se volvió a apagar, la rotación automática sigue en 1 y el forward quedó en `e5c31a82 tcp:4747`. Más tarde, con el teléfono en la mano en vertical, el pie dijo «En vivo · 1280×720» y el epígrafe «720×1280 · Trasera · Automática · Zoom recibe 1920×1080 con barras». La persona se ve derecha. Al desconectar, el epígrafe pasó a «Zoom se quedó en esta imagen», el aviso a «Último cuadro» y el cuadro se quedó. La config no cambió y el forward quedó otra vez en `e5c31a82 tcp:4747`. Después, en modo Automática, el epígrafe dijo «720×1280 · Trasera · USB · Zoom recibe 1920×1080 con barras» y el selector siguió en Automática. El pie se quedó en «En vivo · 1280×720». La persona volvió a verse de costado. La config no cambió y el forward quedó en `e5c31a82 tcp:4747`. Más tarde, al pasar a En vivo · 1280×720 por USB, el aviso quedó en verde: «En vivo» / «La vista previa y cellCam reciben la imagen. La grabación no.». El epígrafe dijo «720×1280 · Trasera · USB · Zoom recibe 1920×1080 con barras» y el botón, Desconectar. Se cerró. La config no cambió y el forward quedó otra vez en `e5c31a82 tcp:4747`. Después, con el cuadro en 720×1280, el aviso pasó a «Teléfono vertical» / «La vista previa muestra el teléfono. Las barras las agrega Zoom.». El pie siguió en «En vivo · 1280×720» y el epígrafe en «720×1280 · Trasera · USB · Zoom recibe 1920×1080 con barras». Se cerró. La config no cambió y el forward quedó en `e5c31a82 tcp:4747`. Con el forward quitado solo para esta prueba, un servidor en `127.0.0.1:4747` entregó el fixture rojo de 32×16 sin rotación. Solo Wi-Fi dejó el aviso en verde, «En vivo» / «La vista previa y cellCam reciben la imagen. La grabación no.», el epígrafe en «32×16 · Trasera · Wi-Fi · Zoom recibe 1920×1080» y el pie en «En vivo · 1280×720». Se cerró, se restauró la config automática sin IP y el forward `e5c31a82 tcp:4747`. En la misma sesión, el servidor pasó de 32×16 sin rotación a 16×32 con 90° y volvió a 32×16. El aviso fue verde, después «Teléfono vertical», y al volver el cuadro horizontal quedó otra vez en verde con el epígrafe «32×16 · Trasera · Wi-Fi · Zoom recibe 1920×1080». El pie no se movió de «En vivo · 1280×720». Se cerró, se restauró la config automática sin IP y el forward `e5c31a82 tcp:4747`.
3. Zoom no está instalado. Chrome, con Camdesk en Conectar, abre `cellCam (Windows Virtual Camera)` a 1920x1080. Antes el segundo `Start` encontraba la fuente todavía activa y `getUserMedia` devolvía `NotReadableError`; ahora ese arranque reinicia la fuente. En Meet (Chrome, reunión instantánea, solo este usuario) el selector muestra `cellCam (Windows Virtual Camera)` marcada, junto a `OBS Virtual Camera`. Meet abrió la fuente: el log llegó a la muestra 601. El cuadro era negro porque el teléfono estaba bloqueado y cellCam no transmitía; Meet avisó que la cámara parecía tapada y pasó al avatar. Se salió de la reunión sin admitir a nadie. El forward de OBS no se tocó.
4. Sin Zoom. Con la ventana abierta, 1280x720 pasó a En vivo y el lienzo siguió en 1920x1080, lleno, sin barras. La imagen era el techo y un monitor. Al elegir 1920x1080 y volver a conectar, el estado fue En vivo 1920x1080, el archivo mapeado siguió en 1920x1080 y la cámara virtual no se volvió a crear. Más tarde, con el teléfono en otra posición, el mismo lienzo mostró barras laterales y la imagen al centro. La persona se ve de costado. Girar la pantalla desde la PC no movió esas barras.
5. Sin Zoom. Al conectar, el lienzo nace negro (Y=16, secuencia 2) y se queda así unos 6 s, con la cámara virtual ya creada. Después el estado pasa a En vivo 1280x720, la secuencia sigue (1282) y el cuadro deja de ser negro uniforme.
6. Primera toma de 15,5 s (`Videos\Camdesk\grabacion-20261125-005430.mp4`): audio 15,5 s y video aplastado a 51 ms, porque el MP4 cambia la base de 1/30 a 1/15360 y los timestamps no se convertían. Corregido. Segunda toma (`grabacion-20261125-005941.mp4`): video 14,67 s a 30 fps, audio 14,76 s, encoder `h264_nvenc`, monitor 1360x768. `graba_un_segundo` mide video 1,000 s a 30 fps y audio 1,081 s. Tercera toma (`grabacion-20261125-110520.mp4`), sin Zoom: 15,03 s, 1360x768, 30 fps, `h264_nvenc`, audio estéreo 48 kHz. El cuadro muestra ese monitor. Sonó un tono de 1 kHz por los parlantes: el archivo queda en −23,1 dB y esa banda concentra el nivel (−23,5 dB; el resto, −34,5 dB). El micrófono estaba activado y nadie habló. Cuarta toma (`grabacion-20261125-113355.mp4`), sin Zoom y sin audio del sistema: 10,97 s, 1360x768, 30 fps, `h264_nvenc`. El cuadro es ese monitor. El micrófono quedó en −51,6 dB (pico −28,5 dB); un tono de 2 kHz por los parlantes no entra (esa banda, −79,9 dB). Nadie habló. Quinta toma (`grabacion-20261125-115931.mp4`), con el teléfono en En vivo 1280x720 y sin Zoom: 5,83 s, 1360x768, 30 fps, `h264_nvenc`, audio estéreo. Durante esos segundos la cámara siguió (secuencia 50 a 402) y el estado se quedó en En vivo. Sexta toma (`grabacion-20261125-120353.mp4`), sin conectar: 3,70 s, 1920x1080, 30 fps, `h264_nvenc`. El selector lista dos entradas y las dos se llaman «18.5 monitor». La segunda graba el monitor principal. Sin elegir, las tomas salen en 1360x768. El menú muestra el tamaño: «18.5 monitor 1360x768» y «18.5 monitor 1920x1080». La elección queda en la config (`monitor=1920x1080`). Al cerrar y abrir, el selector sigue en «18.5 monitor 1920x1080». Séptima toma (`grabacion-20261125-121129.mp4`), sin conectar: 3,73 s, 1920x1080, 30 fps. Después de la prueba se sacó esa clave para no cambiar el monitor por defecto. Octava toma (`grabacion-20261125-122605.mp4`), sin conectar: 7,10 s, 1360x768, 30 fps, `h264_nvenc`. Mientras sonaba un tono de 1 kHz, la salida de consola y la multimedia pasaron de Altavoces a «Realtek HD Audio 2nd output» y volvieron. El tono sigue en los tres tramos (cerca de −23 dB, −8 dB y −23 dB): el loopback acompañó el cambio y la grabación no se cortó. La config no se tocó y la salida quedó otra vez en Altavoces. Con la ventana nueva, novena toma (`grabacion-20261125-124936.mp4`), sin conectar: 2,73 s, 1360x768, 30 fps, `h264_nvenc`. El botón pasó a Detener y el pie mostró REC 00:02. La config no cambió. Décima toma (`grabacion-20261125-125618.mp4`), sin conectar: 54,80 s, 1360x768, 30 fps, `h264_nvenc`. Antes de Grabar no estaban AudioSes ni MMDevAPI. Con un tono de 1 kHz la barra del audio del sistema se llenó y la del micrófono quedó vacía. La config no cambió. Undécima toma (`grabacion-20261125-125959.mp4`), sin conectar: 18,00 s, 1360x768, 30 fps, `h264_nvenc`. El pie mostró «NVENC · 30 fps» junto a REC 00:03. La config no cambió. Duodécima toma (`grabacion-20261125-130244.mp4`), sin conectar: 14,97 s, 1360x768, 30 fps, `h264_nvenc`. La vista previa dijo «Grabando sin cámara. La vista previa sigue vacía.» y el epígrafe «cellCam permanece en negro». El pie siguió en NVENC · 30 fps. La config no cambió. Decimotercera toma (`grabacion-20261125-130543.mp4`), sin conectar: 1,03 s, 1360x768, 30 fps. Al detener, el aviso quedó en «Grabación guardada» y al lado la ruta del MP4. La vista previa volvió al texto de espera y el pie ya no mostró REC. La config no cambió. Decimocuarta toma (`grabacion-20261125-130748.mp4`), sin conectar: 17,57 s, 1360x768, 30 fps. El aviso dijo «Grabando sin cámara» y que la grabación es otro trabajo. El pie siguió en NVENC · 30 fps. La config no cambió. Decimoquinta toma (`grabacion-20261125-131323.mp4`), con el teléfono en En vivo · 1280×720: 23,33 s, 1360x768, 30 fps. El aviso pasó a «Grabando 00:08» y dijo que graba el monitor. La vista previa llevó la marca REC y el pie siguió en NVENC · 30 fps. La config no cambió y el forward quedó en `e5c31a82 tcp:4747`. Decimosexta toma (`grabacion-20261125-140645.mp4`), con el aviso en «Teléfono vertical» y el cuadro en 720×1280: 17,93 s, 1360x768, 30 fps, `h264_nvenc`. Al grabar el aviso pasó a «Grabando 00:07» y no volvió a «Teléfono vertical». La marca REC quedó sobre el cuadro vertical, el epígrafe siguió en «720×1280 · Trasera · USB · Zoom recibe 1920×1080 con barras» y el pie en «En vivo · 1280×720» con «NVENC · 30 fps». Al detener, el aviso quedó en «Grabación guardada» y la ruta del MP4; dos segundos después seguía ahí. La config no cambió y el forward quedó en `e5c31a82 tcp:4747`. Decimoséptima toma (`grabacion-20261125-142540.mp4`), sin conectar: 35,90 s, 1360×768, 30 fps, H.264 y AAC estéreo. Al abrir, las barras de nivel estaban vacías y no estaban `MMDevApi.dll` ni `AUDIOSES.DLL`. Al grabar, con un tono de 1 kHz por los parlantes, la barra del sistema se puso verde y la del micrófono siguió vacía. El pie mostró «NVENC · 30 fps» y el aviso «Grabando sin cámara». La config no cambió y el forward no se tocó.
7. Grabación de 30 min (`Videos\Camdesk\grabacion-20261125-010534.mp4`), encoder `h264_nvenc`, 1360x768. El MP4 dura 29:59,77. Video 1799,67 s a 30 fps y audio 1799,76 s (92 ms de diferencia). En el minuto 10 el cuadro está quieto y el audio está en silencio (−70 dB). Un tono de 1 kHz al final empieza en 1739,668 s y el cuadro pasa de rojo a verde en 1739,667 s. No se usó Zoom.
8. Por Wi-Fi a `192.168.1.37`, en vivo 1280x720. Al apagar el Wi-Fi del teléfono el estado pasó a Desconectado y el cuadro mapeado quedó quieto (secuencia 42). Al encenderlo, la IP volvió a ser `192.168.1.37` y unos 4 s después la secuencia siguió (56). Antes de ese arreglo, un corte que no cerraba el socket dejaba el estado en En vivo y no reintentaba. OBS estaba usando el único cliente y se lo pausó solo durante la prueba. No se miró en Zoom. Por USB, con la sesión ya abierta, `adb -s e5c31a82 forward --remove tcp:4747` deja la lista de forwards vacía y el socket sigue: el estado se queda en En vivo 1280x720 y la secuencia pasa de 46 a 170 en 2 s. Cerrar ese socket desde aquí no se pudo: Windows devolvió 317 al borrar la conexión de Camdesk y el teléfono no deja cerrarla. La secuencia siguió (de 62 a 744) con el estado en En vivo, y cellCam quedó escuchando en 4747. Después de cerrar se dejó otra vez ese forward. Más tarde, en Solo Wi-Fi a `192.168.1.37`, el epígrafe dijo «720×1280 · Trasera · Wi-Fi · Zoom recibe 1920×1080 con barras» y el pie «En vivo · 1280×720». Al cerrar se dejó otra vez el modo automático, sin IP, y el forward `e5c31a82 tcp:4747`. En Solo Wi-Fi a `192.0.2.1`, que no responde, a los 5 s el pie seguía en Conectando, el botón en Cancelar y el epígrafe en «Reintento cada 2 segundos». Se canceló. La config volvió a automático sin IP y ese forward no se tocó. Con el forward `e5c31a82 tcp:4747` quitado solo para esta prueba, un servidor en `127.0.0.1:4747` respondió el handshake con estado de versión. Solo Wi-Fi a esa IP dejó el pie en «Versión incompatible», el botón en Conectar, el centro en «Sin imagen.», el epígrafe en «No hay reintento útil» y el aviso «Versión incompatible» / «cellCam habla otra versión del protocolo. Actualizar el teléfono o Camdesk.». A los 4 s seguía ahí, sin volver a Conectando. Se cerró, se restauró la config automática sin IP y el forward `e5c31a82 tcp:4747 tcp:4747`.
9. El handle de la ventana apareció en cerca de 1,1 s. Antes de Conectar no estaban cargadas `avcodec`, `swscale` ni el runtime de NVIDIA. Después de Conectar sí se cargan `avcodec-62.dll`, `avutil-60.dll` y `swscale-9.dll`. Más tarde, ya con la lista de monitores armada al abrir y sin conectar: el handle aparece en 0,17 s y los botones Conectar y Grabar en 0,63 s y 0,66 s. No están FFmpeg, NVIDIA, WASAPI ni Windows.Graphics.Capture. `mfplat.dll` sí está cargada; el archivo mapeado y el log de la cámara no cambian de hora, y no se lanza adb.
10. RTX 4060 y runtime NVIDIA Video Effects 0.7.6. `BackgroundBlur::new` carga `GreenScreen` y `BackgroundBlur` en modo calidad. En un cuadro sintético de 1280x720 el efecto cambia la imagen (diferencia media 31) y tarda unos 5,5 ms; a 1920x1080, unos 7,7 ms. No hizo falta pasar a modo rendimiento. Con el teléfono en vivo a 1280x720, antes de marcar la casilla no estaban cargadas `NVVideoEffects.dll` ni `NVCVImage.dll`. Al marcarla se cargan y el detalle baja (de 2,10 a 1,87) sin mover el promedio del cuadro. La cadencia siguió en 29,3 fps con el efecto y 30,4 fps al apagarlo. No apareció el aviso de modo rendimiento. Más tarde, con una persona en el cuadro vertical, el techo pasa de una desviación de 11,1 a 2,2 y la persona se queda en 58 a 55. La cadencia fue 30,3 fps. Las DLL de NVIDIA se cargaron al marcar la casilla y no antes. Tampoco apareció el aviso de modo rendimiento. La persona sigue de costado. Zoom no está instalado. Para ver el aviso, una corrida forzó el presupuesto de cuadro a 0 ms (el real es 34 ms) y lo devolvió después de la captura. Con el teléfono en vivo, al marcar Desenfocar fondo el aviso pasó a «Modo rendimiento» y «El desenfoque pasó a modo rendimiento para sostener 30 fps.», en ámbar. La casilla quedó prendida, el pie siguió «En vivo · 1280×720» y la vista previa no se cortó. El presupuesto volvió a 34 ms y el ejecutable se recompiló. Abrir de nuevo, sin marcar la casilla, no muestra ese aviso ni carga `NVVideoEffects.dll`. Para ver un fallo a mitad de sesión, una corrida hizo fallar `apply` al cuadro 13 y lo quitó después de la captura. Con el teléfono en vivo, al marcar Desenfocar fondo el aviso pasó a «Sin desenfoque» y «El efecto falló y se apagó. La cámara sigue.», en ámbar. La casilla quedó apagada, el detalle dijo «GreenScreen falló», el pie siguió «En vivo · 1280×720» y la vista previa siguió. El forzado se revirtió y el ejecutable se recompiló.
11. En esta PC (RTX 4060 y runtime 0.7.6) la casilla «Desenfocar fondo» queda habilitada, sin motivo ni enlace, y al abrir no se cargan `NVVideoEffects.dll` ni FFmpeg. La rama sin runtime muestra la casilla y la fuerza deshabilitadas, el texto «sin el runtime de NVIDIA Video Effects» y el enlace «Descargar el runtime» hacia la página del redistribuible 0.7.6. Esa rama se vio forzando el resultado en una corrida local que después se revirtió: no hay otra PC sin RTX. Con el mismo forzado, ya en En vivo · 1280×720 por USB, el aviso dijo «Sin desenfoque» / «Sin GPU NVIDIA, o sin el runtime de NVIDIA Video Effects. La cámara sigue.», la casilla quedó apagada, el texto del runtime y el enlace siguieron, y la vista previa siguió llegando. Se revirtió el forzado: al abrir no aparece ese aviso y no se cargan `NVVideoEffects.dll` ni FFmpeg. La config no cambió y el forward quedó en `e5c31a82 tcp:4747`.
12. Sin conectar, los cuatro sliders se movieron a brillo 0,2, contraste −0,4, saturación 0,6 y temperatura −0,25. Al cerrar y reabrir, los cuatro volvieron en esos valores. Restablecer los dejó en 0 y lo escribió en la config; la fuerza del desenfoque no se tocó. Con el teléfono en vivo a 1280x720, el brillo 0,35 dejó el cuadro en un promedio de 198; subirlo a 1 lo llevó a 235, y Restablecer (brillo 0) lo bajó a 142. Después se restauró el brillo 0,35. Con el brillo en 0, el contraste al máximo sube la desviación de 78,8 a 96,1. La saturación al máximo lleva el color de |U| 4,2 y |V| 4,0 a 9,2 y 8,9. La temperatura al máximo deja U en 92,6 y V en 157,5 (el neutro estaba en 124 y 131). Se dejó otra vez el brillo 0,35 y el resto en 0. Zoom no está instalado. `cargo test -p fx` ya cubre que el neutro no cambia un píxel y que el brillo alto aclara un gris.

Causa de «la persona se ve de costado» en el punto 2, leída en el código de cellCam y no corregida desde aquí: cellCam manda `rotationDegrees` de CameraX, que sigue a `targetRotation`, y `targetRotation` solo cambia cuando dispara `OrientationEventListener` (`StreamService.kt`, `createOrientationListener`). Con la pantalla apagada o el teléfono quieto, el listener no dispara y la rotación queda en la del arranque. Camdesk aplica la que recibe: con el teléfono en la mano, la persona sale derecha, y `la_rotacion_es_horaria_y_no_espeja` en `decode` fija el sentido de las cuatro rotaciones. Se corrige en el repo de cellCam.

Hilo de video (`camdesk-video`), 25/11/2026, teléfono `e5c31a82` por USB en modo automático, con la pantalla bloqueada, release, sin Zoom ni Meet. Se midió la secuencia del archivo mapeado.
- Conectar pasa a En vivo · 1280×720 en 1 s. La cámara recibe 30,0 a 30,3 fps con la ventana visible, 30,2 minimizada y 30,0 al restaurarla.
- Desconectar vuelve a Conectar en 137 ms. La secuencia queda quieta (último cuadro) y el forward se quita.
- Desenfocar fondo carga `NVVideoEffects.dll` y `NVCVImage.dll` en ese hilo al marcar la casilla, no antes. La cámara sigue a 30,0 fps (29,0 minimizada). El teléfono miraba el techo, sin persona: el detalle del cuadro baja de 0,83 sin efecto a 0,20 con efecto.
- La versión anterior, con todo en el hilo de egui, también sostenía unos 30 fps minimizada y unos 27 fps con la ventana en el bucle de mover (`SC_MOVE`). Minimizar no la frenaba.
- Lo que sí cambió es Cancelar. Con Solo Wi-Fi a `192.0.2.1`, tres veces Conectar y Cancelar: la anterior dejó la ventana sin responder hasta 6,3 s (vueltas de 6286, 509 y 118 ms) y la nueva vuelve en 27 a 45 ms.
- Se usó la config de siempre y la DLL de cámara ya instalada (`CAMDESK_VCAM_DLL`). Al final se restauró la config automática sin IP y el forward `e5c31a82 tcp:4747`.

Salidas y monitores, 25/11/2026, dos salidas (Altavoces, predeterminada, y «Realtek HD Audio 2nd output») y dos monitores del mismo modelo. Con un tono de 1 kHz solo en «2nd output»: eligiendo esa salida el MP4 trae el tono (total −10,0 dB, banda de 1 kHz −10,1 dB); eligiendo Altavoces o la predeterminada no (banda de 1 kHz entre −47,7 y −45,9 dB, con otro audio sonando). Cada monitor grabó su tamaño: «1 · 18.5 monitor» 1360×768 y «2 · 18.5 monitor» 1920×1080. Una salida inexistente no arranca la grabación. Encoder `h264_nvenc`.

El selector Frontal no está en la lista numerada. Con ese valor el estado fue En vivo 1280x720 y el cuadro siguió siendo el de la trasera: la misma persona y el mismo techo, diferencia media 1 sobre 255. cellCam abre siempre la cámara trasera. El lienzo siguió en 1920x1080. Camdesk ahora usa la cámara de la respuesta: el aviso dice «El teléfono se quedó en la cámara trasera», el selector vuelve a Trasera y la config queda en `camera=back`.
