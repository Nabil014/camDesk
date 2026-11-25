# Protocolo cellcam — versión 1

Contrato entre la app Android (servidor) y el plugin de OBS (cliente).

**Este documento manda.** Si el código y el documento discrepan, el código está mal.
Cualquier cambio incompatible incrementa `VERSION`.

---

## 0. Reglas generales

| Regla | Valor |
|---|---|
| Transporte | TCP. **USB:** `adb forward` (puerto 4747). **Wi-Fi:** conexión directa a la IP del teléfono (misma red). |
| Puerto | 4747 |
| Orden de bytes | **Big-endian** en todos los campos multibyte |
| Formato de vídeo | **Annex-B** (NAL units prefijadas con `00 00 00 01`) |
| Base de tiempos | Microsegundos, reloj monótono |
| `TCP_NODELAY` | Activado en ambos extremos |
| Clientes simultáneos | **Uno**. Si ya hay un cliente, la conexión nueva se rechaza. |

Annex-B no es una elección estética: es lo que `MediaCodec` produce por defecto y lo
que `libavcodec` consume sin conversión. Elegir AVCC obligaría a reempaquetar en un
extremo sin ganar nada.

El *payload* de un frame nunca se altera: sale del encoder y entra al decoder byte a byte.

---

## 1. Handshake

Al conectar, el cliente envía 16 bytes y el servidor responde con otros 16 con la
misma estructura.

### 1.1 Petición (PC → teléfono)

| Offset | Campo | Tipo | Descripción |
|---:|---|---|---|
| 0 | `magic` | u32 | `0x4343414D` (`"CCAM"`) |
| 4 | `version` | u8 | Versión del protocolo. Actualmente 1. |
| 5 | `codec` | u8 | 0 = H.264, 1 = HEVC |
| 6 | `width` | u16 | Ancho deseado en píxeles |
| 8 | `height` | u16 | Alto deseado en píxeles |
| 10 | `fps` | u8 | Cadencia deseada (techo, ver §4.3) |
| 11 | `camera` | u8 | 0 = trasera, 1 = frontal |
| 12 | `flags` | u8 | Ver §3.1 |
| 13 | `status` | u8 | Sin usar en la petición. Debe ser 0. |
| 14 | `reserved` | u16 | Debe ser 0 |

### 1.2 Respuesta (teléfono → PC)

Misma estructura. Los campos `codec`, `width`, `height`, `fps` y `camera` contienen
**los valores realmente aplicados**, que pueden diferir de los pedidos.

`status` indica el resultado:

| Valor | Nombre | Significado |
|---:|---|---|
| 0 | `STATUS_OK` | Aplicado exactamente lo pedido |
| 1 | `STATUS_ADJUSTED` | Aplicado lo más cercano posible. El cliente debe usar los valores devueltos. |
| 2 | `STATUS_ERROR_VERSION` | Versión no soportada. El servidor cierra la conexión. |
| 3 | `STATUS_ERROR_CODEC` | Códec no disponible. El servidor cierra la conexión. |
| 4 | `STATUS_ERROR_CAMERA` | No se pudo abrir la cámara. El servidor cierra la conexión. |

En los estados de error el resto de campos carece de significado.

### 1.3 Tiempos

El cliente dispone de **3 segundos** para enviar la petición tras conectar; pasado ese
plazo el servidor cierra el socket. El servidor responde en menos de 5 segundos
(abrir la cámara y arrancar el encoder no es instantáneo).

---

## 2. Cabecera de frame

Tras el handshake, todo lo que viaja lleva esta cabecera de 20 bytes.

| Offset | Campo | Tipo | Descripción |
|---:|---|---|---|
| 0 | `magic` | u32 | `0x46524D30` (`"FRM0"`) |
| 4 | `type` | u8 | 0 = vídeo, 1 = audio, 2 = control |
| 5 | `flags` | u8 | Ver §3.2 |
| 6 | `reserved` | u16 | Debe ser 0 |
| 8 | `pts` | u64 | Microsegundos (ver §4.2) |
| 16 | `length` | u32 | Bytes del payload que sigue |

El `magic` por frame parece redundante sobre TCP, pero es lo que permite
**resincronizar** si el parser se desalinea por un bug (ver §5.2). Cuesta 4 bytes por
frame y ahorra tardes enteras.

`length` máximo: **4 MiB**. Un valor superior significa desincronización, no un frame
grande.

### 2.1 Payload de control (`type = 2`)

Viaja **del PC al teléfono**, 8 bytes:

| Offset | Campo | Tipo | Descripción |
|---:|---|---|---|
| 0 | `command` | u8 | Ver tabla |
| 1 | `reserved` | u8 ×3 | Debe ser 0 |
| 4 | `value` | u32 | Argumento, depende del comando |

| Valor | Comando | `value` | Estado |
|---:|---|---|---|
| 1 | `CTRL_REQUEST_KEYFRAME` | ignorado | v1 |
| 2 | `CTRL_SET_CAMERA` | 0 trasera / 1 frontal | reservado |
| 3 | `CTRL_SET_BITRATE` | bits por segundo | reservado |
| 4 | `CTRL_PING` | ignorado | reservado |

Los comandos marcados *reservado* están asignados pero no implementados en v1. Un
servidor v1 debe **ignorarlos silenciosamente**, nunca cerrar la conexión por recibirlos.

---

## 3. Flags

### 3.1 Flags de handshake

| Bit | Nombre | Significado |
|---:|---|---|
| 0 | `HS_FLAG_REQUEST_KEYFRAME` | Solicita IDR inmediato al arrancar. En la práctica siempre activo (ver §4.1). |
| 1 | `HS_FLAG_AUDIO` | Solicita flujo de audio. **Reservado**, sin efecto en v1. |
| 2-7 | — | Reservados, deben ser 0 |

### 3.2 Flags de frame

| Bit | Nombre | Significado |
|---:|---|---|
| 0 | `FRAME_FLAG_KEYFRAME` | El payload contiene un IDR. Punto de entrada válido para el decoder. |
| 1 | `FRAME_FLAG_CODEC_CONFIG` | El payload son parámetros, no imagen: SPS+PPS en H.264, VPS+SPS+PPS en HEVC. |
| 2-7 | — | Reservados, deben ser 0 |

Un frame con `CODEC_CONFIG` **no se muestra**: se alimenta al decoder como
configuración. Los dos bits no aparecen juntos: la configuración va en su propio frame,
inmediatamente antes del IDR al que corresponde.

---

## 4. Semántica

### 4.1 Conexión y reconexión

Este es el punto donde más implementaciones fallan, y siempre con el mismo síntoma:
pantalla negra indefinida.

Al aceptar un cliente y completar el handshake, el servidor debe, **en este orden**:

1. Enviar un frame con `FRAME_FLAG_CODEC_CONFIG` y el SPS/PPS guardado.
2. Pedir un IDR al encoder (`PARAMETER_KEY_REQUEST_SYNC_FRAME`).
3. **Descartar todo frame que produzca el encoder hasta que llegue el IDR.**

El paso 3 es tan importante como los otros dos. Un frame P referencia frames que este
cliente nunca vio; enviarlo produce basura visual hasta el siguiente IDR. Más vale
esperar 100 ms a que llegue el keyframe.

El servidor guarda el SPS/PPS desde el primer buffer con `BUFFER_FLAG_CODEC_CONFIG`
que emite el encoder, y lo conserva mientras el encoder viva. Un cliente que conecta
en el minuto 20 lo necesita igual que el primero.

Al desconectarse un cliente, el servidor mantiene cámara y encoder vivos y sigue
descartando frames. Volver a arrancar la cámara en cada reconexión añadiría segundos.

### 4.2 Marcas de tiempo

El `pts` procede de un reloj monótono del teléfono (`System.nanoTime() / 1000`).

- Es **monótonamente creciente** dentro de una sesión.
- Su **origen es arbitrario**: el primer frame no vale 0.
- El cliente debe rebasar contra el primer frame recibido y trabajar con deltas.

No se sincronizan relojes entre máquinas. Cuando llegue el audio (fase 2), ambos
flujos usarán este mismo reloj, y la sincronización A/V se reduce a respetar los pts.

### 4.3 El fps es un techo, no una promesa

El `fps` negociado es la cadencia **máxima**. La cadencia real varía por debajo, sobre
todo por el auto-exposure: con poca luz la cámara alarga la exposición y puede caer a
la mitad. Medido en un Mi 9T: 30 fps con buena luz, 14 fps en interior sin luz directa.

**El cliente nunca debe asumir cadencia constante.** La reproducción se rige por los
pts, no por el fps negociado.

### 4.4 Resolución no soportada

El servidor aplica la resolución soportada más cercana y responde con
`STATUS_ADJUSTED` y las dimensiones reales. Nunca falla por este motivo. El cliente
está obligado a leer los valores devueltos: si dimensiona sus buffers con lo que pidió,
el resultado son imágenes deformadas o un desbordamiento.

### 4.5 Congestión

Con MJPEG descartar el frame viejo era gratis; **con H.264 no lo es**, porque romper la
cadena de referencias corrompe la imagen hasta el siguiente IDR.

Política del servidor:

- Cola acotada de salida (5–10 frames).
- Si se llena: **vaciarla entera** y pedir un keyframe nuevo.

Es decir, ante congestión se corta limpio y se reengancha, en lugar de degradar. El
cliente verá un salto en los pts: es el comportamiento esperado, no un error.

### 4.6 Orientación

El sensor se transmite **sin rotar**; la rotación necesaria viaja como metadato en
el campo `reserved` de la cabecera de frame (§2), y es el cliente quien la aplica.

| Bits | Valor | Significado |
|---:|---|---|
| 0-1 | 0 | 0° |
| | 1 | 90° en sentido horario |
| | 2 | 180° |
| | 3 | 270° |
| 2-15 | — | Reservados, deben ser 0 |

Es la rotación **horaria** que el cliente debe aplicar para mostrar la imagen
derecha. Cambia si el usuario gira el teléfono durante la sesión.

Rotar en el teléfono costaría GPU en cada frame; el cliente ya está componiendo
la imagen y puede hacerlo sin coste adicional apreciable.

### 4.7 Keepalive

No hay. Sobre USB, TCP detecta la caída del enlace con suficiente rapidez. En Wi-Fi conviene
desactivar el ahorro de batería agresivo del teléfono durante la sesión.

---

## 5. Manejo de errores

### 5.1 Versión incompatible

Si `version` no coincide, el servidor responde con `STATUS_ERROR_VERSION` y cierra.
No hay negociación a la baja: cambiar de versión implica cambios incompatibles.

### 5.2 Resincronización

Si el cliente lee una cabecera cuyo `magic` no es `FRM0`, o cuyo `length` supera 4 MiB,
está desalineado. Procedimiento: avanzar byte a byte buscando el siguiente `FRM0`,
descartar frames hasta encontrar uno con `FRAME_FLAG_KEYFRAME`, y enviar
`CTRL_REQUEST_KEYFRAME` para acelerar el reenganche.

Que esto ocurra indica un bug. El mecanismo existe para que el bug se manifieste como
un parpadeo y no como una sesión perdida.

### 5.3 Campos reservados

Todo campo marcado *reservado* se escribe a 0 y **se ignora al leer**. Así v1 puede
convivir con extensiones futuras que los usen.
