//! Several peer messages serialized back to back into the connection's one
//! write buffer, so that they leave in a single socket write.
//!
//! The buffer is the one each connection always had (`MAX_MSG_LEN`, about
//! 17 KiB: the largest single message); batching only uses its free space. A
//! message that doesn't fit behind what is already there goes first into the
//! next batch. A `piece` (16 KiB + 13) therefore leaves room for about 1 KiB of
//! small messages, and two pieces never share a batch.

use librqbit_core::lengths::ChunkInfo;
use peer_binary_protocol::{
    MAX_MSG_LEN, Message, SerializeError, extended::PeerExtendedMessageIds,
    serialize_piece_preamble,
};

/// Length prefix, message id, index and begin of a `piece` message.
const PIECE_PREAMBLE_LEN: usize = 13;

pub(crate) struct WriteBatch {
    buf: Box<[u8; MAX_MSG_LEN]>,
    len: usize,
}

impl WriteBatch {
    pub fn new(buf: Box<[u8; MAX_MSG_LEN]>) -> Self {
        Self { buf, len: 0 }
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn bytes(&self) -> &[u8] {
        &self.buf[..self.len]
    }

    pub fn clear(&mut self) {
        self.len = 0;
    }

    /// Appends what `serialize` writes into the free space. `Ok(false)`: it
    /// didn't fit behind the messages already in the batch (send them, then
    /// push again). Into an empty batch, failing is an error as it always was.
    pub fn push(
        &mut self,
        serialize: impl FnOnce(&mut [u8]) -> Result<usize, SerializeError>,
    ) -> Result<bool, SerializeError> {
        match serialize(&mut self.buf[self.len..]) {
            Ok(n) => {
                self.len += n;
                Ok(true)
            }
            Err(_) if self.len > 0 => Ok(false),
            Err(e) => Err(e),
        }
    }

    pub fn push_message(
        &mut self,
        msg: &Message<'_>,
        ext_msg_ids: &dyn Fn() -> PeerExtendedMessageIds,
    ) -> Result<bool, SerializeError> {
        self.push(|buf| msg.serialize(buf, ext_msg_ids))
    }

    pub fn fits_piece(&self, chunk: &ChunkInfo) -> bool {
        self.len + PIECE_PREAMBLE_LEN + chunk.size as usize <= self.buf.len()
    }

    /// Appends a `piece` message header for `chunk` and returns the space of
    /// its block, to be read into in place. Check [`Self::fits_piece`] first.
    pub fn piece(&mut self, chunk: &ChunkInfo) -> Result<&mut [u8], SerializeError> {
        if !self.fits_piece(chunk) {
            return Err(SerializeError::NoSpaceInBuffer);
        }
        let start = self.len;
        let preamble = serialize_piece_preamble(chunk, &mut self.buf[start..]);
        debug_assert_eq!(preamble, PIECE_PREAMBLE_LEN);
        self.len += preamble + chunk.size as usize;
        Ok(&mut self.buf[start + preamble..self.len])
    }
}

#[cfg(test)]
mod tests {
    use librqbit_core::lengths::{ChunkInfo, Lengths};
    use peer_binary_protocol::{MAX_MSG_LEN, Message, Request};

    use super::WriteBatch;

    fn batch() -> WriteBatch {
        WriteBatch::new(Box::new([0u8; MAX_MSG_LEN]))
    }

    fn alone(msg: &Message<'_>) -> Vec<u8> {
        let mut buf = vec![0u8; MAX_MSG_LEN];
        let n = msg.serialize(&mut buf, &Default::default).unwrap();
        buf.truncate(n);
        buf
    }

    fn chunk(piece: u32, size: u32) -> ChunkInfo {
        let lengths = Lengths::new(100 * 16384, 16384).unwrap();
        let mut c = lengths
            .iter_chunk_infos(lengths.validate_piece_index(piece).unwrap())
            .next()
            .unwrap();
        c.size = size;
        c
    }

    fn parse_all(mut bytes: &[u8]) -> Vec<String> {
        let mut out = vec![];
        while !bytes.is_empty() {
            let (msg, n) = Message::deserialize(bytes, &[]).unwrap();
            out.push(format!("{msg:?}"));
            bytes = &bytes[n..];
        }
        out
    }

    #[test]
    fn messages_keep_their_order() {
        let msgs = [
            Message::Unchoke,
            Message::Have(7),
            Message::Request(Request::new(1, 0, 16384)),
            Message::KeepAlive,
            Message::Choke,
            Message::Cancel(Request::new(1, 0, 16384)),
        ];
        let mut b = batch();
        for m in &msgs {
            assert!(b.push_message(m, &Default::default).unwrap());
        }
        let expected: Vec<u8> = msgs.iter().flat_map(alone).collect();
        assert_eq!(b.bytes(), &expected[..]);
        assert_eq!(
            parse_all(b.bytes()),
            msgs.iter().map(|m| format!("{m:?}")).collect::<Vec<_>>()
        );
        b.clear();
        assert!(b.is_empty());
    }

    #[test]
    fn stops_at_the_buffer_size() {
        let mut b = batch();
        let mut n = 0;
        while b
            .push_message(&Message::Have(n), &Default::default)
            .unwrap()
        {
            n += 1;
        }
        assert_eq!(n as usize, MAX_MSG_LEN / 9);
        assert_eq!(b.bytes().len(), n as usize * 9);
        // What didn't fit is not half written.
        assert_eq!(parse_all(b.bytes()).len(), n as usize);
        // A message too large even for an empty batch is still an error.
        let mut b = batch();
        let huge = vec![0u8; MAX_MSG_LEN];
        assert!(
            b.push_message(
                &Message::Bitfield(buffers::ByteBuf(&huge)),
                &Default::default
            )
            .is_err()
        );
        assert!(b.is_empty());
    }

    #[test]
    fn piece_boundary() {
        let c = chunk(3, 16384);
        // A piece fits an empty batch and leaves room for small messages.
        let mut b = batch();
        assert!(b.fits_piece(&c));
        b.piece(&c).unwrap().fill(0xab);
        assert!(!b.fits_piece(&c), "two pieces never share a batch");
        assert!(
            b.push_message(&Message::Have(1), &Default::default)
                .unwrap()
        );
        let parsed = parse_all(b.bytes());
        assert_eq!(parsed.len(), 2);
        assert!(parsed[0].starts_with("Piece"));
        assert_eq!(parsed[1], "Have(1)");

        // Behind enough small messages, the piece waits for the next batch.
        let mut b = batch();
        let mut n = 0;
        while b.fits_piece(&c) {
            assert!(
                b.push_message(&Message::Have(n), &Default::default)
                    .unwrap()
            );
            n += 1;
        }
        assert!(b.bytes().len() + 13 + 16384 > MAX_MSG_LEN);
        assert!(b.piece(&c).is_err());
        assert_eq!(parse_all(b.bytes()).len(), n as usize);

        // A short last block.
        let mut b = batch();
        assert_eq!(b.piece(&chunk(3, 100)).unwrap().len(), 100);
        assert_eq!(b.bytes().len(), 113);
    }
}
