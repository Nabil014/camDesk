//! Bytes del socket de cellCam. No abre la red, no enlaza FFmpeg y no pinta.

pub const PORT: u16 = 4747;
pub const HANDSHAKE_LEN: usize = 16;
pub const FRAME_HEADER_LEN: usize = 20;
pub const CONTROL_LEN: usize = 8;
pub const MAX_PAYLOAD: u32 = 4 * 1024 * 1024;
pub const VERSION: u8 = 1;

pub const MAGIC_HANDSHAKE: u32 = 0x4343_414D;
pub const MAGIC_FRAME: u32 = 0x4652_4D30;

pub const HS_FLAG_REQUEST_KEYFRAME: u8 = 1 << 0;
pub const HS_FLAG_AUDIO: u8 = 1 << 1;

pub const FRAME_FLAG_KEYFRAME: u8 = 1 << 0;
pub const FRAME_FLAG_CODEC_CONFIG: u8 = 1 << 1;

pub const CTRL_REQUEST_KEYFRAME: u8 = 1;

const CODEC_H264: u8 = 0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rotation {
    R0 = 0,
    R90 = 1,
    R180 = 2,
    R270 = 3,
}

impl Rotation {
    pub fn from_bits(bits: u16) -> Self {
        match bits & 0b11 {
            1 => Self::R90,
            2 => Self::R180,
            3 => Self::R270,
            _ => Self::R0,
        }
    }

    pub fn degrees(self) -> u16 {
        (self as u16) * 90
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HandshakeStatus {
    Ok,
    Adjusted,
    Version,
    Codec,
    Camera,
    Other(u8),
}

impl HandshakeStatus {
    fn from_byte(value: u8) -> Self {
        match value {
            0 => Self::Ok,
            1 => Self::Adjusted,
            2 => Self::Version,
            3 => Self::Codec,
            4 => Self::Camera,
            other => Self::Other(other),
        }
    }

    fn as_byte(self) -> u8 {
        match self {
            Self::Ok => 0,
            Self::Adjusted => 1,
            Self::Version => 2,
            Self::Codec => 3,
            Self::Camera => 4,
            Self::Other(value) => value,
        }
    }

    pub fn is_error(self) -> bool {
        !matches!(self, Self::Ok | Self::Adjusted)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProtocolError {
    BadMagic,
    Desync,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HandshakeRequest {
    pub version: u8,
    pub codec: u8,
    pub width: u16,
    pub height: u16,
    pub fps: u8,
    pub camera: u8,
    pub flags: u8,
}

impl HandshakeRequest {
    pub fn video(width: u16, height: u16, fps: u8, front: bool) -> Self {
        Self {
            version: VERSION,
            codec: CODEC_H264,
            width,
            height,
            fps,
            camera: if front { 1 } else { 0 },
            flags: HS_FLAG_REQUEST_KEYFRAME,
        }
    }

    pub fn to_bytes(self) -> [u8; HANDSHAKE_LEN] {
        let mut out = [0u8; HANDSHAKE_LEN];
        out[0..4].copy_from_slice(&MAGIC_HANDSHAKE.to_be_bytes());
        out[4] = self.version;
        out[5] = self.codec;
        out[6..8].copy_from_slice(&self.width.to_be_bytes());
        out[8..10].copy_from_slice(&self.height.to_be_bytes());
        out[10] = self.fps;
        out[11] = self.camera;
        out[12] = self.flags;
        out
    }

    pub fn parse(bytes: &[u8; HANDSHAKE_LEN]) -> Result<Self, ProtocolError> {
        let magic = u32::from_be_bytes(bytes[0..4].try_into().unwrap());
        if magic != MAGIC_HANDSHAKE {
            return Err(ProtocolError::BadMagic);
        }
        Ok(Self {
            version: bytes[4],
            codec: bytes[5],
            width: u16::from_be_bytes(bytes[6..8].try_into().unwrap()),
            height: u16::from_be_bytes(bytes[8..10].try_into().unwrap()),
            fps: bytes[10],
            camera: bytes[11],
            flags: bytes[12],
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HandshakeResponse {
    pub status: HandshakeStatus,
    pub version: u8,
    pub codec: u8,
    pub width: u32,
    pub height: u32,
    pub fps: u8,
    pub camera: u8,
    pub flags: u8,
}

impl HandshakeResponse {
    pub fn parse(bytes: &[u8; HANDSHAKE_LEN]) -> Result<Self, ProtocolError> {
        let magic = u32::from_be_bytes(bytes[0..4].try_into().unwrap());
        if magic != MAGIC_HANDSHAKE {
            return Err(ProtocolError::BadMagic);
        }
        Ok(Self {
            status: HandshakeStatus::from_byte(bytes[13]),
            version: bytes[4],
            codec: bytes[5],
            width: u16::from_be_bytes(bytes[6..8].try_into().unwrap()) as u32,
            height: u16::from_be_bytes(bytes[8..10].try_into().unwrap()) as u32,
            fps: bytes[10],
            camera: bytes[11],
            flags: bytes[12],
        })
    }

    pub fn to_bytes(self) -> [u8; HANDSHAKE_LEN] {
        let mut out = [0u8; HANDSHAKE_LEN];
        out[0..4].copy_from_slice(&MAGIC_HANDSHAKE.to_be_bytes());
        out[4] = self.version;
        out[5] = self.codec;
        out[6..8].copy_from_slice(&(self.width as u16).to_be_bytes());
        out[8..10].copy_from_slice(&(self.height as u16).to_be_bytes());
        out[10] = self.fps;
        out[11] = self.camera;
        out[12] = self.flags;
        out[13] = self.status.as_byte();
        out
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameKind {
    Video,
    CodecConfig,
    Audio,
    Control,
    Other(u8),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FrameHeader {
    pub kind: FrameKind,
    pub keyframe: bool,
    pub rotation: Rotation,
    pub pts_us: i64,
    pub len: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    pub header: FrameHeader,
    pub payload: Vec<u8>,
}

pub struct FrameParser {
    buf: Vec<u8>,
}

impl FrameParser {
    pub fn new() -> Self {
        Self { buf: Vec::new() }
    }

    pub fn push(&mut self, data: &[u8]) {
        self.buf.extend_from_slice(data);
    }

    pub fn next(&mut self) -> Option<Result<Frame, ProtocolError>> {
        loop {
            if self.buf.len() < 4 {
                return None;
            }
            let magic = u32::from_be_bytes(self.buf[0..4].try_into().unwrap());
            if magic != MAGIC_FRAME {
                return Some(Err(self.skip_to_magic()));
            }
            if self.buf.len() < FRAME_HEADER_LEN {
                return None;
            }
            let len = u32::from_be_bytes(self.buf[16..20].try_into().unwrap());
            if len > MAX_PAYLOAD {
                return Some(Err(self.skip_to_magic()));
            }
            let total = FRAME_HEADER_LEN + len as usize;
            if self.buf.len() < total {
                return None;
            }
            let header = parse_header(&self.buf[..FRAME_HEADER_LEN], len);
            let payload = self.buf[FRAME_HEADER_LEN..total].to_vec();
            self.buf.drain(..total);
            return Some(Ok(Frame { header, payload }));
        }
    }

    fn skip_to_magic(&mut self) -> ProtocolError {
        let start = if self.buf.len() >= 4 { 1 } else { 0 };
        if let Some(found) = find_magic(&self.buf[start..]) {
            self.buf.drain(..start + found);
        } else {
            let keep = self.buf.len().min(3);
            self.buf.drain(..self.buf.len() - keep);
        }
        ProtocolError::Desync
    }
}

impl Default for FrameParser {
    fn default() -> Self {
        Self::new()
    }
}

fn parse_header(bytes: &[u8], len: u32) -> FrameHeader {
    let kind_byte = bytes[4];
    let flags = bytes[5];
    let reserved = u16::from_be_bytes(bytes[6..8].try_into().unwrap());
    let pts_us = i64::from_be_bytes(bytes[8..16].try_into().unwrap());
    let codec_config = flags & FRAME_FLAG_CODEC_CONFIG != 0;
    let kind = if codec_config && kind_byte == 0 {
        FrameKind::CodecConfig
    } else {
        match kind_byte {
            0 => FrameKind::Video,
            1 => FrameKind::Audio,
            2 => FrameKind::Control,
            other => FrameKind::Other(other),
        }
    };
    FrameHeader {
        kind,
        keyframe: flags & FRAME_FLAG_KEYFRAME != 0,
        rotation: Rotation::from_bits(reserved),
        pts_us,
        len,
    }
}

fn find_magic(bytes: &[u8]) -> Option<usize> {
    let needle = MAGIC_FRAME.to_be_bytes();
    bytes.windows(4).position(|window| window == needle)
}

pub fn request_keyframe() -> Vec<u8> {
    let mut out = Vec::with_capacity(FRAME_HEADER_LEN + CONTROL_LEN);
    out.extend_from_slice(&MAGIC_FRAME.to_be_bytes());
    out.push(2);
    out.push(0);
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&0i64.to_be_bytes());
    out.extend_from_slice(&(CONTROL_LEN as u32).to_be_bytes());
    out.push(CTRL_REQUEST_KEYFRAME);
    out.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handshake_de_pedido_coincide_con_el_vector() {
        let req = HandshakeRequest::video(1280, 720, 30, false);
        let bytes = req.to_bytes();
        let expected = [
            0x43, 0x43, 0x41, 0x4D, 0x01, 0x00, 0x05, 0x00, 0x02, 0xD0, 0x1E, 0x00, 0x01, 0x00, 0x00, 0x00,
        ];
        assert_eq!(bytes, expected);
        assert_eq!(HandshakeRequest::parse(&bytes).unwrap(), req);
    }

    #[test]
    fn respuesta_ajustada_conserva_el_tamano_real() {
        let bytes = [
            0x43, 0x43, 0x41, 0x4D, 0x01, 0x00, 0x02, 0x80, 0x01, 0xE0, 0x1E, 0x01, 0x01, 0x01, 0xAB, 0xCD,
        ];
        let parsed = HandshakeResponse::parse(&bytes).unwrap();
        assert_eq!(parsed.status, HandshakeStatus::Adjusted);
        assert_eq!(parsed.width, 640);
        assert_eq!(parsed.height, 480);
        assert_eq!(parsed.camera, 1);
        assert!(!parsed.status.is_error());
    }

    #[test]
    fn magic_de_handshake_roto_no_se_interpreta() {
        let mut bytes = HandshakeRequest::video(1280, 720, 30, false).to_bytes();
        bytes[0] = 0;
        assert_eq!(HandshakeResponse::parse(&bytes), Err(ProtocolError::BadMagic));
    }

    #[test]
    fn frame_partido_en_dos_push_sale_entero() {
        let mut parser = FrameParser::new();
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&MAGIC_FRAME.to_be_bytes());
        bytes.push(0);
        bytes.push(FRAME_FLAG_KEYFRAME);
        bytes.extend_from_slice(&1u16.to_be_bytes());
        bytes.extend_from_slice(&1_500_000i64.to_be_bytes());
        bytes.extend_from_slice(&4u32.to_be_bytes());
        bytes.extend_from_slice(&[9, 8, 7, 6]);
        parser.push(&bytes[..10]);
        assert!(parser.next().is_none());
        parser.push(&bytes[10..]);
        let frame = parser.next().unwrap().unwrap();
        assert_eq!(frame.header.kind, FrameKind::Video);
        assert!(frame.header.keyframe);
        assert_eq!(frame.header.rotation, Rotation::R90);
        assert_eq!(frame.header.pts_us, 1_500_000);
        assert_eq!(frame.payload, vec![9, 8, 7, 6]);
        assert!(parser.next().is_none());
    }

    #[test]
    fn magic_roto_reengancha_en_el_siguiente_frm0() {
        let mut parser = FrameParser::new();
        let mut bytes = vec![0, 1, 2, 3, 4];
        bytes.extend(request_keyframe());
        parser.push(&bytes);
        assert_eq!(parser.next(), Some(Err(ProtocolError::Desync)));
        let frame = parser.next().unwrap().unwrap();
        assert_eq!(frame.header.kind, FrameKind::Control);
        assert_eq!(frame.payload[0], CTRL_REQUEST_KEYFRAME);
    }

    #[test]
    fn largo_mayor_a_4_mib_no_reserva_el_payload() {
        let mut parser = FrameParser::new();
        let mut header = [0u8; FRAME_HEADER_LEN];
        header[0..4].copy_from_slice(&MAGIC_FRAME.to_be_bytes());
        header[16..20].copy_from_slice(&(MAX_PAYLOAD + 1).to_be_bytes());
        parser.push(&header);
        parser.push(&request_keyframe());
        assert_eq!(parser.next(), Some(Err(ProtocolError::Desync)));
        let frame = parser.next().unwrap().unwrap();
        assert_eq!(frame.header.len, CONTROL_LEN as u32);
        assert!(parser.buf.len() < 64);
    }

    #[test]
    fn config_de_codec_no_se_marca_como_imagen() {
        let mut parser = FrameParser::new();
        let mut bytes = vec![0u8; FRAME_HEADER_LEN];
        bytes[0..4].copy_from_slice(&MAGIC_FRAME.to_be_bytes());
        bytes[5] = FRAME_FLAG_CODEC_CONFIG;
        bytes.extend_from_slice(&[1, 2]);
        bytes[16..20].copy_from_slice(&2u32.to_be_bytes());
        parser.push(&bytes);
        let frame = parser.next().unwrap().unwrap();
        assert_eq!(frame.header.kind, FrameKind::CodecConfig);
        assert!(!frame.header.keyframe);
    }
}
