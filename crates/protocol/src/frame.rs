//! Wire frame for the gongga reliable-UDP transport.
//!
//! Every UDP datagram carries a fixed 26-byte header followed by a JSON payload
//! fragment. The header is intentionally small and fixed-size so the ARQ layer
//! can retransmit payloads verbatim without re-encoding.
//!
//! ```text
//!  0 ..  2  magic      u16   0x4747 ("GG")
//!  2 ..  3  version    u8    1
//!  3 ..  4  flags      u8    SYN | ACK | FIN | RST | DATA | PING
//!  4 ..  8  conn_id    u32   connection identifier assigned by the server
//!  8 .. 12  seq        u32   packet sequence number (ARQ unit)
//! 12 .. 16  ack        u32   cumulative ack: next seq the sender expects
//! 16 .. 20  msg_id     u32   logical message a DATA fragment belongs to
//! 20 .. 22  frag_idx   u16   index of this fragment within the message
//! 22 .. 24  frag_cnt   u16   total fragments in the message
//! 24 .. 26  window     u16   receive window, in packets
//! ```

use bytes::{Buf, BufMut, Bytes, BytesMut};

pub const MAGIC: u16 = 0x4747;
pub const VERSION: u8 = 1;
pub const HEADER_LEN: usize = 26;

/// Default maximum datagram size. 1200 keeps a full frame under the Internet
/// safe MTU so cross-machine links never rely on IP fragmentation.
pub const DEFAULT_MTU: usize = 1200;

/// Payload budget for a single fragment.
pub const fn payload_capacity(mtu: usize) -> usize {
    mtu.saturating_sub(HEADER_LEN)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Flags(u8);

impl Flags {
    pub const SYN: u8 = 0x01;
    pub const ACK: u8 = 0x02;
    pub const FIN: u8 = 0x04;
    pub const RST: u8 = 0x08;
    pub const DATA: u8 = 0x10;
    pub const PING: u8 = 0x20;

    pub const fn empty() -> Self {
        Flags(0)
    }

    pub const fn from_bits(bits: u8) -> Self {
        Flags(bits)
    }

    pub const fn bits(self) -> u8 {
        self.0
    }

    pub const fn union(self, other: u8) -> Self {
        Flags(self.0 | other)
    }

    pub const fn contains(self, bit: u8) -> bool {
        self.0 & bit != 0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub flags: Flags,
    pub conn_id: u32,
    pub seq: u32,
    /// Cumulative ack. Only meaningful when [`Flags::ACK`] is set.
    pub ack: u32,
    pub msg_id: u32,
    pub frag_idx: u16,
    pub frag_cnt: u16,
    pub window: u16,
    pub payload: Bytes,
}

impl Frame {
    pub fn ack(conn_id: u32, ack: u32, window: u16) -> Self {
        Self {
            flags: Flags::from_bits(Flags::ACK),
            conn_id,
            seq: 0,
            ack,
            msg_id: 0,
            frag_idx: 0,
            frag_cnt: 0,
            window,
            payload: Bytes::new(),
        }
    }

    pub fn syn(conn_id: u32, window: u16) -> Self {
        Self {
            flags: Flags::from_bits(Flags::SYN),
            conn_id,
            seq: 0,
            ack: 0,
            msg_id: 0,
            frag_idx: 0,
            frag_cnt: 0,
            window,
            payload: Bytes::new(),
        }
    }

    pub fn syn_ack(conn_id: u32, window: u16) -> Self {
        Self {
            flags: Flags::from_bits(Flags::SYN | Flags::ACK),
            ..Self::syn(conn_id, window)
        }
    }

    pub fn fin(conn_id: u32, seq: u32, ack: u32, window: u16) -> Self {
        Self {
            flags: Flags::from_bits(Flags::FIN | Flags::ACK),
            conn_id,
            seq,
            ack,
            msg_id: 0,
            frag_idx: 0,
            frag_cnt: 0,
            window,
            payload: Bytes::new(),
        }
    }

    pub fn reset(conn_id: u32) -> Self {
        Self {
            flags: Flags::from_bits(Flags::RST),
            conn_id,
            seq: 0,
            ack: 0,
            msg_id: 0,
            frag_idx: 0,
            frag_cnt: 0,
            window: 0,
            payload: Bytes::new(),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn data(
        conn_id: u32,
        seq: u32,
        ack: u32,
        msg_id: u32,
        frag_idx: u16,
        frag_cnt: u16,
        window: u16,
        payload: Bytes,
    ) -> Self {
        Self {
            flags: Flags::from_bits(Flags::DATA | Flags::ACK),
            conn_id,
            seq,
            ack,
            msg_id,
            frag_idx,
            frag_cnt,
            window,
            payload,
        }
    }

    pub fn is_data(&self) -> bool {
        self.flags.contains(Flags::DATA)
    }

    pub fn encode(&self, out: &mut BytesMut) {
        out.reserve(HEADER_LEN + self.payload.len());
        out.put_u16(MAGIC);
        out.put_u8(VERSION);
        out.put_u8(self.flags.bits());
        out.put_u32(self.conn_id);
        out.put_u32(self.seq);
        out.put_u32(self.ack);
        out.put_u32(self.msg_id);
        out.put_u16(self.frag_idx);
        out.put_u16(self.frag_cnt);
        out.put_u16(self.window);
        out.put_slice(&self.payload);
    }

    pub fn encode_owned(&self) -> BytesMut {
        let mut out = BytesMut::new();
        self.encode(&mut out);
        out
    }

    pub fn decode(mut buf: Bytes) -> Result<Self, FrameError> {
        if buf.len() < HEADER_LEN {
            return Err(FrameError::TooShort {
                len: buf.len(),
                need: HEADER_LEN,
            });
        }
        let magic = buf.get_u16();
        if magic != MAGIC {
            return Err(FrameError::BadMagic(magic));
        }
        let version = buf.get_u8();
        if version != VERSION {
            return Err(FrameError::BadVersion(version));
        }
        let flags = Flags::from_bits(buf.get_u8());
        let conn_id = buf.get_u32();
        let seq = buf.get_u32();
        let ack = buf.get_u32();
        let msg_id = buf.get_u32();
        let frag_idx = buf.get_u16();
        let frag_cnt = buf.get_u16();
        let window = buf.get_u16();
        let payload = buf;

        if flags.contains(Flags::DATA) && frag_cnt == 0 {
            return Err(FrameError::ZeroFragments);
        }
        if flags.contains(Flags::DATA) && frag_idx >= frag_cnt {
            return Err(FrameError::BadFragment { frag_idx, frag_cnt });
        }

        Ok(Self {
            flags,
            conn_id,
            seq,
            ack,
            msg_id,
            frag_idx,
            frag_cnt,
            window,
            payload,
        })
    }
}

#[derive(Debug, thiserror::Error)]
pub enum FrameError {
    #[error("datagram too short: {len} bytes, need at least {need}")]
    TooShort { len: usize, need: usize },
    #[error("bad magic 0x{0:04x}")]
    BadMagic(u16),
    #[error("unsupported protocol version {0}")]
    BadVersion(u8),
    #[error("DATA frame declares zero fragments")]
    ZeroFragments,
    #[error("fragment index {frag_idx} out of range (count {frag_cnt})")]
    BadFragment { frag_idx: u16, frag_cnt: u16 },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_a_data_frame() {
        let frame = Frame::data(7, 42, 9, 3, 1, 4, 64, Bytes::from_static(b"hello"));
        let encoded = frame.encode_owned().freeze();
        let decoded = Frame::decode(encoded).unwrap();
        assert_eq!(decoded, frame);
        assert!(decoded.is_data());
    }

    #[test]
    fn rejects_foreign_datagrams() {
        let mut buf = BytesMut::new();
        buf.put_u16(0x1234);
        buf.put_u8(VERSION);
        buf.put_u8(0);
        buf.put_bytes(0, HEADER_LEN - 4);
        assert!(matches!(
            Frame::decode(buf.freeze()),
            Err(FrameError::BadMagic(_))
        ));
    }

    #[test]
    fn payload_capacity_respects_header() {
        assert_eq!(payload_capacity(DEFAULT_MTU), DEFAULT_MTU - HEADER_LEN);
        // Never underflows on absurdly small MTUs.
        assert_eq!(payload_capacity(8), 0);
    }
}
