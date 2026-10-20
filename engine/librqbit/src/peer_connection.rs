use std::{
    collections::VecDeque,
    net::SocketAddr,
    sync::Arc,
    time::{Duration, Instant},
};

use crate::{
    Error, Result, advertised_haves::AdvertisedHaves, session::CheckedIncomingConnection,
    stream_connect::ConnectionKind,
};
use buffers::{ByteBuf, ByteBufOwned};
use bytes::Bytes;
use futures::TryFutureExt;
use futures::future::{FusedFuture, FutureExt};
use librqbit_core::{
    hash_id::Id20,
    lengths::{ChunkInfo, ValidPieceIndex},
    peer_id::try_decode_peer_id,
};
use parking_lot::RwLock;
use peer_binary_protocol::{
    Handshake, HashRequest, MAX_MSG_LEN, Message,
    extended::{
        ExtendedMessage, PeerExtendedMessageIds, handshake::ExtendedHandshake,
        ut_metadata::UtMetadata, ut_pex::UtPex,
    },
    serialize_hashes, serialize_piece_preamble,
};
use serde::{Deserialize, Serialize};
use serde_with::serde_as;
use tokio::time::timeout;
use tracing::{Instrument, debug, trace, trace_span};

use crate::{
    read_buf::ReadBuf,
    spawn_utils::BlockingSpawner,
    stream_connect::StreamConnector,
    type_aliases::{BoxAsyncReadVectored, BoxAsyncWrite},
};

pub trait PeerConnectionHandler {
    fn on_connected(&self, _connection_time: Duration) {}
    /// Set the BEP 52 v2 bit in our handshake.
    fn handshake_v2(&self) -> bool {
        false
    }
    /// The peer's handshake may carry this info hash although the
    /// connection uses another one: the other hash of a hybrid torrent (a
    /// v1 connection upgraded to v2, or the reverse answer to an upgrade).
    fn accepts_other_info_hash(&self, _info_hash: Id20) -> bool {
        false
    }
    fn should_send_bitfield(&self) -> bool;
    /// Also records the pieces the BITFIELD announces in `advertised`, from
    /// the same snapshot.
    fn serialize_bitfield_message_to_buf(
        &self,
        buf: &mut [u8],
        advertised: &mut AdvertisedHaves,
    ) -> anyhow::Result<usize>;
    /// No BITFIELD was sent: start `advertised` empty, sized to the torrent.
    fn init_advertised_haves(&self, _advertised: &mut AdvertisedHaves) {}
    /// The HAVE broadcast dropped entries for this connection: hand the
    /// pieces we have that `advertised` lacks to `emit` (see
    /// [`AdvertisedHaves::resync_step`]), or cancel the resync if this peer
    /// gets no HAVEs.
    fn resync_haves(&self, advertised: &mut AdvertisedHaves, _emit: &mut dyn FnMut(u32) -> bool) {
        advertised.cancel_resync();
    }
    /// `encrypted`: the connection uses MSE/PE with RC4.
    fn on_handshake(
        &self,
        handshake: Handshake,
        ckind: ConnectionKind,
        encrypted: bool,
    ) -> anyhow::Result<()>;
    fn on_extended_handshake(
        &self,
        extended_handshake: &ExtendedHandshake<ByteBuf>,
    ) -> anyhow::Result<()>;
    async fn on_received_message(&self, msg: Message<'_>) -> anyhow::Result<()>;
    /// Called (and awaited) right after `on_extended_handshake`.
    async fn after_extended_handshake(&self) -> anyhow::Result<()> {
        Ok(())
    }
    fn should_transmit_have(&self, id: ValidPieceIndex) -> bool;
    fn on_uploaded_bytes(&self, bytes: u32);
    fn read_chunk(&self, chunk: &ChunkInfo, buf: &mut [u8]) -> anyhow::Result<()>;
    /// Called by the writer before an upload request is read and sent: waits
    /// for the upload rate limits. Throttling here, at the socket, means that
    /// a limit set while requests are queued applies to them too.
    async fn throttle_upload(&self, _chunk: &ChunkInfo) {}
    /// Called by the writer right before an upload request is read and sent:
    /// returns false if the peer cancelled it meanwhile (it is then skipped).
    fn take_upload_request(&self, _chunk: &ChunkInfo) -> bool {
        true
    }
    /// A queued `ut_metadata` data message is about to be sent.
    fn on_metadata_piece_dequeued(&self) {}
    /// A queued BEP 52 `hashes` / `hash reject` is about to be sent.
    fn on_hash_reply_dequeued(&self) {}
    fn update_my_extended_handshake(
        &self,
        _handshake: &mut ExtendedHandshake<ByteBuf>,
    ) -> anyhow::Result<()> {
        Ok(())
    }
    fn client_name_and_version(&self) -> &str {
        crate::client_name_and_version()
    }
    /// Bytes read from the peer at the BitTorrent protocol level (handshakes,
    /// message framing and payload).
    fn on_protocol_bytes_received(&self, _bytes: u64) {}
    /// Bytes written to the peer at the BitTorrent protocol level.
    fn on_protocol_bytes_sent(&self, _bytes: u64) {}
}

#[derive(Debug)]
pub enum WriterRequest {
    Message(Message<'static>),
    UtMetadata(UtMetadata<ByteBufOwned>),
    UtPex(UtPex<ByteBufOwned>),
    /// BEP 52 reply: `hashes` with this payload, or `hash reject` if `None`.
    V2Hashes(HashRequest, Option<Bytes>),
    ReadChunkRequest(ChunkInfo),
    Disconnect(anyhow::Result<()>),
}

/// How long an established peer may stay silent before it is dropped. Peers
/// only owe a keep-alive every two minutes (libtorrent's `peer_timeout` is 120 s
/// too), so the short handshake/write timeout must not apply here.
const PEER_IDLE_TIMEOUT: Duration = Duration::from_secs(120);

/// Sent well within the peer's idle timeout so that it never drops us.
const KEEP_ALIVE_INTERVAL: Duration = Duration::from_secs(60);

#[serde_as]
#[derive(Default, Debug, Copy, Clone, Serialize, Deserialize, Eq, PartialEq)]
pub struct PeerConnectionOptions {
    #[serde_as(as = "Option<serde_with::DurationSeconds>")]
    pub connect_timeout: Option<Duration>,

    #[serde_as(as = "Option<serde_with::DurationSeconds>")]
    pub read_write_timeout: Option<Duration>,

    #[serde_as(as = "Option<serde_with::DurationSeconds>")]
    pub keep_alive_interval: Option<Duration>,

    /// The most chunk requests to keep outstanding per peer (default 128, and
    /// never more than the peer's reqq). A newly needed piece, such as the
    /// one at a stream's read position after a seek, is requested only after
    /// those, so a smaller window makes streams react faster to seeks.
    pub max_request_window: Option<usize>,
}

pub(crate) struct PeerConnection<H> {
    handler: H,
    addr: SocketAddr,
    info_hash: Id20,
    peer_id: Id20,
    options: PeerConnectionOptions,
    spawner: BlockingSpawner,
    connector: Arc<StreamConnector>,
}

#[cfg(not(feature = "miri"))]
pub(crate) async fn with_timeout<T>(
    name: &'static str,
    timeout_value: Duration,
    fut: impl std::future::Future<Output = Result<T>>,
) -> crate::Result<T> {
    match timeout(timeout_value, fut).await {
        Ok(v) => v,
        Err(_) => Err(Error::Timeout(name)),
    }
}

#[cfg(feature = "miri")]
pub(crate) async fn with_timeout<T>(
    _name: &'static str,
    _timeout_value: Duration,
    fut: impl std::future::Future<Output = Result<T>>,
) -> crate::Result<T> {
    fut.await
}

struct ManagePeerArgs {
    handshake_supports_extended: bool,
    outgoing: bool,
    read_buf: ReadBuf,
    write_buf: Box<[u8; MAX_MSG_LEN]>,
    read: BoxAsyncReadVectored,
    write: BoxAsyncWrite,
    outgoing_chan: tokio::sync::mpsc::UnboundedReceiver<WriterRequest>,
    have_broadcast: tokio::sync::broadcast::Receiver<ValidPieceIndex>,
}

impl<H: PeerConnectionHandler> PeerConnection<H> {
    pub fn new(
        addr: SocketAddr,
        info_hash: Id20,
        peer_id: Id20,
        handler: H,
        options: Option<PeerConnectionOptions>,
        spawner: BlockingSpawner,
        connector: Arc<StreamConnector>,
    ) -> Self {
        PeerConnection {
            handler,
            addr,
            info_hash,
            peer_id,
            spawner,
            options: options.unwrap_or_default(),
            connector,
        }
    }

    // By the time this is called:
    // read_buf should start with valuable data. The handshake should be removed from it.
    pub async fn manage_peer_incoming(
        &self,
        outgoing_chan: tokio::sync::mpsc::UnboundedReceiver<WriterRequest>,
        mut incoming: CheckedIncomingConnection,
        have_broadcast: tokio::sync::broadcast::Receiver<ValidPieceIndex>,
    ) -> Result<()> {
        use tokio::io::AsyncWriteExt;

        let rwtimeout = self
            .options
            .read_write_timeout
            .unwrap_or_else(|| Duration::from_secs(10));

        if incoming.handshake.info_hash != self.info_hash
            && !self
                .handler
                .accepts_other_info_hash(incoming.handshake.info_hash)
        {
            return Err(Error::WrongInfoHash);
        }

        if incoming.handshake.peer_id == self.peer_id {
            return Err(Error::ConnectingToOurselves);
        }

        trace!(
            "incoming connection: id={:?}",
            try_decode_peer_id(incoming.handshake.peer_id)
        );

        let mut write_buf = Box::new([0u8; MAX_MSG_LEN]);
        let mut my_handshake = Handshake::new(self.info_hash, self.peer_id);
        if self.handler.handshake_v2() {
            my_handshake.set_supports_v2();
        }
        let hlen = my_handshake.serialize_unchecked_len(&mut *write_buf);
        with_timeout(
            "writing handshake",
            rwtimeout,
            incoming
                .writer
                .write_all(&write_buf[..hlen])
                .map_err(Error::WriteHandshake),
        )
        .await?;
        self.handler.on_protocol_bytes_sent(hlen as u64);
        self.handler
            .on_protocol_bytes_received(incoming.read_buf.take_consumed());

        // What the peer supports is in the peer's handshake, not in ours.
        let handshake_supports_extended = incoming.handshake.supports_extended();

        self.handler
            .on_handshake(incoming.handshake, incoming.kind, incoming.encrypted)
            .map_err(Error::Anyhow)?;

        self.manage_peer(ManagePeerArgs {
            handshake_supports_extended,
            outgoing: false,
            read_buf: incoming.read_buf,
            write_buf,
            read: incoming.reader,
            write: incoming.writer,
            outgoing_chan,
            have_broadcast,
        })
        .await
    }

    pub async fn manage_peer_outgoing(
        &self,
        outgoing_chan: tokio::sync::mpsc::UnboundedReceiver<WriterRequest>,
        have_broadcast: tokio::sync::broadcast::Receiver<ValidPieceIndex>,
    ) -> Result<()> {
        use tokio::io::AsyncWriteExt;
        let rwtimeout = self
            .options
            .read_write_timeout
            .unwrap_or_else(|| Duration::from_secs(10));

        let connect_timeout = self
            .options
            .connect_timeout
            .unwrap_or_else(|| Duration::from_secs(10));

        let now = Instant::now();
        let crate::stream_connect::Connected {
            kind: ckind,
            encrypted,
            mut read,
            mut write,
        } = self
            .connector
            .connect(self.addr, self.info_hash, connect_timeout, rwtimeout)
            .await?;

        async move {
            self.handler.on_connected(now.elapsed());

            let mut write_buf = Box::new([0u8; MAX_MSG_LEN]);
            let mut handshake = Handshake::new(self.info_hash, self.peer_id);
            if self.handler.handshake_v2() {
                handshake.set_supports_v2();
            }
            let hsz = handshake.serialize_unchecked_len(&mut *write_buf);
            with_timeout(
                "writing",
                rwtimeout,
                write
                    .write_all(&write_buf[..hsz])
                    .map_err(Error::WriteHandshake),
            )
            .await?;
            self.handler.on_protocol_bytes_sent(hsz as u64);

            let mut read_buf = ReadBuf::new();
            let h = read_buf.read_handshake(&mut read, rwtimeout).await?;
            self.handler
                .on_protocol_bytes_received(read_buf.take_consumed());
            let handshake_supports_extended = h.supports_extended();
            trace!(
                peer_id=?h.peer_id,
                decoded_id=?try_decode_peer_id(h.peer_id),
                "connected",
            );
            if h.info_hash != self.info_hash && !self.handler.accepts_other_info_hash(h.info_hash) {
                return Err(Error::WrongInfoHash);
            }

            if h.peer_id == self.peer_id {
                return Err(Error::ConnectingToOurselves);
            }

            self.handler
                .on_handshake(h, ckind, encrypted)
                .map_err(Error::Anyhow)?;

            self.manage_peer(ManagePeerArgs {
                handshake_supports_extended,
                outgoing: true,
                read_buf,
                write_buf,
                read,
                write,
                outgoing_chan,
                have_broadcast,
            })
            .await
        }
        .instrument(trace_span!("", kind=%ckind))
        .await
    }

    async fn manage_peer(&self, args: ManagePeerArgs) -> Result<()> {
        let ManagePeerArgs {
            handshake_supports_extended,
            outgoing,
            mut read_buf,
            mut write_buf,
            mut read,
            mut write,
            mut outgoing_chan,
            mut have_broadcast,
        } = args;

        use tokio::io::AsyncWriteExt;

        let rwtimeout = self
            .options
            .read_write_timeout
            .unwrap_or_else(|| Duration::from_secs(10));

        let extended_handshake: RwLock<Option<PeerExtendedMessageIds>> = RwLock::new(None);
        let extended_handshake_ref = &extended_handshake;
        let supports_extended = handshake_supports_extended;

        if supports_extended {
            let net = self.connector.settings();
            let mut my_extended = ExtendedHandshake::new();
            // Anonymous mode (libtorrent): neither the client name nor our
            // listen port. The port is only sent on connections we made
            // (the other side of an incoming one knows it already).
            // Our clearnet listen port never goes to I2P peers: it would link
            // our I2P destination to our clearnet identity.
            let i2p = crate::i2p::is_i2p_addr(self.addr);
            if !net.anonymous_mode {
                my_extended.v = Some(ByteBuf(self.handler.client_name_and_version().as_bytes()));
                if outgoing
                    && !i2p
                    && let Some(port) = self.connector.advertised_listen_port()
                {
                    my_extended.p = Some(u32::from(port));
                }
            }
            if !net.pex {
                my_extended.m.ut_pex = None;
            }
            if !crate::i2p::is_i2p_addr(self.addr) {
                my_extended.yourip = Some(self.addr.ip().into());
            }
            self.handler
                .update_my_extended_handshake(&mut my_extended)
                .map_err(Error::Anyhow)?;
            let my_extended = Message::Extended(ExtendedMessage::Handshake(my_extended));
            trace!("sending extended handshake: {:?}", &my_extended);
            let esz = my_extended.serialize(&mut *write_buf, &Default::default)?;
            with_timeout(
                "writing extended handshake",
                rwtimeout,
                write.write_all(&write_buf[..esz]).map_err(Error::Write),
            )
            .await?;
            self.handler.on_protocol_bytes_sent(esz as u64);
        }

        let writer = async move {
            let keep_alive_interval = self
                .options
                .keep_alive_interval
                .unwrap_or(KEEP_ALIVE_INTERVAL);

            let mut advertised = AdvertisedHaves::default();
            if self.handler.should_send_bitfield() {
                let len = self
                    .handler
                    .serialize_bitfield_message_to_buf(&mut *write_buf, &mut advertised)
                    .map_err(Error::Anyhow)?;
                with_timeout(
                    "writing bitfield",
                    rwtimeout,
                    write.write_all(&write_buf[..len]).map_err(Error::Write),
                )
                .await?;
                self.handler.on_protocol_bytes_sent(len as u64);
                trace!("sent bitfield");
            } else {
                self.handler.init_advertised_haves(&mut advertised);
            }

            // Every connection starts choked: the choker unchokes peers
            // (through the outgoing channel) once they are interested.

            let mut broadcast_closed = false;

            // Uploads wait for the rate limits here, one at a time and in
            // request order, while other messages (choke, have, our own
            // requests, disconnect) keep flowing.
            let mut pending_uploads: VecDeque<ChunkInfo> = VecDeque::new();
            let wait_for_upload = |chunk: ChunkInfo| async move {
                self.handler.throttle_upload(&chunk).await;
                chunk
            };
            let mut upload_wait = std::pin::pin!(futures::future::Fuse::terminated());

            loop {
                let req = loop {
                    if advertised.is_resyncing() {
                        let mut next = None;
                        self.handler.resync_haves(&mut advertised, &mut |index| {
                            next.is_none() && {
                                next = Some(index);
                                true
                            }
                        });
                        if let Some(index) = next {
                            break WriterRequest::Message(Message::Have(index));
                        }
                    }
                    if upload_wait.is_terminated()
                        && let Some(chunk) = pending_uploads.pop_front()
                    {
                        upload_wait.set(wait_for_upload(chunk).fuse());
                    }
                    break tokio::select! {
                        chunk = &mut upload_wait, if !upload_wait.is_terminated() => {
                            WriterRequest::ReadChunkRequest(chunk)
                        }
                        r = have_broadcast.recv(), if !broadcast_closed => match r {
                            Ok(id) => {
                                if self.handler.should_transmit_have(id)
                                    && advertised.mark(id.get())
                                {
                                    WriterRequest::Message(Message::Have(id.get()))
                                } else {
                                    continue
                                }
                            },
                            Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                                broadcast_closed = true;
                                debug!("broadcast channel closed, will not poll it anymore");
                                continue
                            },
                            Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                                // Never drop a HAVE: find what the peer missed.
                                debug!(missed = n, "have broadcast lagged, resyncing");
                                advertised.start_resync();
                                continue
                            }
                        },
                        r = timeout(keep_alive_interval, outgoing_chan.recv()) => match r {
                            Ok(Some(WriterRequest::ReadChunkRequest(chunk))) => {
                                pending_uploads.push_back(chunk);
                                continue
                            }
                            Ok(Some(msg)) => msg,
                            Ok(None) => {
                                return Err(Error::TorrentIsNotLive);
                            }
                            Err(_) => WriterRequest::Message(Message::KeepAlive),
                        }
                    };
                };

                tokio::task::yield_now().await;

                let mut uploaded_add = None;

                trace!("about to send: {:?}", &req);
                let ext_msg_ids = &|| {
                    extended_handshake_ref
                        .read()
                        .as_ref()
                        .map(|e| *e)
                        .unwrap_or_default()
                };

                let len = match req {
                    WriterRequest::Message(msg) => msg.serialize(&mut *write_buf, ext_msg_ids)?,
                    WriterRequest::UtMetadata(utm) => {
                        if matches!(utm, UtMetadata::Data(..)) {
                            self.handler.on_metadata_piece_dequeued();
                        }
                        Message::Extended(ExtendedMessage::UtMetadata(utm.as_borrowed()))
                            .serialize(&mut *write_buf, ext_msg_ids)?
                    }
                    WriterRequest::V2Hashes(req, reply) => {
                        self.handler.on_hash_reply_dequeued();
                        match reply {
                            Some(hashes) => serialize_hashes(&req, &hashes, &mut *write_buf)?,
                            None => {
                                Message::HashReject(req).serialize(&mut *write_buf, ext_msg_ids)?
                            }
                        }
                    }
                    WriterRequest::UtPex(ut_pex) => {
                        Message::Extended(ExtendedMessage::UtPex(ut_pex.as_borrowed()))
                            .serialize(&mut *write_buf, ext_msg_ids)?
                    }
                    WriterRequest::ReadChunkRequest(chunk) => {
                        if !self.handler.take_upload_request(&chunk) {
                            trace!(?chunk, "upload request was cancelled");
                            continue;
                        }
                        #[allow(unused_mut)]
                        let mut skip_reading_for_e2e_tests = false;

                        #[cfg(test)]
                        {
                            use tracing::warn;
                            // This is poor-mans fault injection for running e2e tests.
                            use crate::tests::test_util::TestPeerMetadata;
                            let tpm = TestPeerMetadata::from_peer_id(self.peer_id);
                            use rand::RngExt;
                            if rand::rng().random_bool(tpm.disconnect_probability()) {
                                return Err(Error::TestDisconnect);
                            }

                            #[allow(clippy::cast_possible_truncation)]
                            let sleep_ms = (rand::rng().random::<f64>()
                                * (tpm.max_random_sleep_ms as f64))
                                as u64;
                            if sleep_ms > 0 {
                                tokio::time::sleep(Duration::from_millis(sleep_ms)).await;
                            }

                            if rand::rng().random_bool(tpm.bad_data_probability()) {
                                warn!(
                                    "will NOT actually read the data to simulate a malicious peer that sends garbage"
                                );
                                write_buf.fill(0);
                                skip_reading_for_e2e_tests = true;
                            }
                        }

                        // this whole section is an optimization
                        let preamble_len = serialize_piece_preamble(&chunk, &mut *write_buf);
                        let full_len = preamble_len + chunk.size as usize;
                        if !skip_reading_for_e2e_tests {
                            self.spawner
                                .block_in_place_with_semaphore(|| {
                                    self.handler
                                        .read_chunk(&chunk, &mut write_buf[preamble_len..full_len])
                                })
                                .await
                                .map_err(Error::ReadChunk)?;
                        }

                        uploaded_add = Some(chunk.size);
                        full_len
                    }
                    WriterRequest::Disconnect(res) => {
                        trace!("disconnect requested, closing writer");
                        match res {
                            Ok(()) => return Err(Error::Disconnect),
                            Err(e) => return Err(Error::DisconnectWithSource(e)),
                        }
                    }
                };

                with_timeout(
                    "writing",
                    rwtimeout,
                    write.write_all(&write_buf[..len]).map_err(Error::Write),
                )
                .await?;
                self.handler.on_protocol_bytes_sent(len as u64);

                if let Some(uploaded_add) = uploaded_add {
                    self.handler.on_uploaded_bytes(uploaded_add)
                }
            }

            // For type inference.
            #[allow(unreachable_code)]
            Ok::<_, Error>(())
        };

        let reader = async move {
            loop {
                let (message, consumed) = read_buf
                    .read_message(&mut read, PEER_IDLE_TIMEOUT)
                    .await?;
                // Account the wire bytes before handling the message, so that
                // anything the handler makes visible (e.g. a finished download)
                // is never ahead of the protocol counters.
                self.handler.on_protocol_bytes_received(consumed);
                trace!("received: {:?}", &message);

                tokio::task::yield_now().await;

                if let Message::Extended(ExtendedMessage::Handshake(h)) = &message {
                    *extended_handshake_ref.write() = Some(h.peer_extended_messages());
                    self.handler
                        .on_extended_handshake(h)
                        .map_err(Error::Anyhow)?;
                    self.handler
                        .after_extended_handshake()
                        .await
                        .map_err(Error::Anyhow)?;
                } else {
                    self.handler
                        .on_received_message(message)
                        .await
                        .map_err(Error::Anyhow)?;
                }
            }

            // For type inference.
            #[allow(unreachable_code)]
            Ok::<_, Error>(())
        };

        tokio::select! {
            r = reader => {
                if let Err(e) = r.as_ref() {
                    trace!("reader finished with error: {e:#}");
                } else {
                    trace!("reader finished without error");
                }
                r
            }
            r = writer => {
                if let Err(e) = r.as_ref() {
                    trace!("writer finished with error: {e:#}");
                } else {
                    trace!("writer finished without error");
                }
                r
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        net::SocketAddr,
        pin::Pin,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
        task::{Context, Poll},
        time::Duration,
    };

    use buffers::ByteBuf;
    use librqbit_core::{
        hash_id::Id20,
        lengths::{ChunkInfo, Lengths, ValidPieceIndex},
    };
    use parking_lot::Mutex;
    use peer_binary_protocol::{Handshake, Message, extended::handshake::ExtendedHandshake};
    use tokio::{
        io::{AsyncReadExt, AsyncWrite},
        sync::{broadcast, mpsc},
    };

    use super::{PeerConnection, PeerConnectionHandler, WriterRequest};
    use crate::{
        advertised_haves::AdvertisedHaves,
        read_buf::ReadBuf,
        session::CheckedIncomingConnection,
        spawn_utils::BlockingSpawner,
        stream_connect::{ConnectionKind, StreamConnector},
        type_aliases::BF,
        vectored_traits::AsyncReadVectoredIntoCompat,
    };

    const PIECES: u32 = 1000;

    /// A torrent that has `have` and completes `burst` right after the
    /// BITFIELD was taken, before the writer reads the broadcast.
    struct Mock {
        lengths: Lengths,
        have: Mutex<BF>,
        tx: broadcast::Sender<ValidPieceIndex>,
        burst: Vec<u32>,
    }

    impl Mock {
        fn complete(&self, piece: u32) {
            self.have.lock().set(piece as usize, true);
            let _ = self
                .tx
                .send(self.lengths.validate_piece_index(piece).unwrap());
        }
    }

    impl PeerConnectionHandler for &Mock {
        fn should_send_bitfield(&self) -> bool {
            true
        }
        fn serialize_bitfield_message_to_buf(
            &self,
            buf: &mut [u8],
            advertised: &mut AdvertisedHaves,
        ) -> anyhow::Result<usize> {
            let len = {
                let have = self.have.lock();
                advertised.init(&have, true);
                Message::Bitfield(ByteBuf(have.as_raw_slice())).serialize(buf, &Default::default)?
            };
            for &p in &self.burst {
                self.complete(p);
            }
            Ok(len)
        }
        fn resync_haves(
            &self,
            advertised: &mut AdvertisedHaves,
            emit: &mut dyn FnMut(u32) -> bool,
        ) {
            advertised.resync_step(&self.have.lock(), emit);
        }
        fn on_handshake(
            &self,
            _handshake: Handshake,
            _ckind: ConnectionKind,
            _encrypted: bool,
        ) -> anyhow::Result<()> {
            Ok(())
        }
        fn on_extended_handshake(&self, _: &ExtendedHandshake<ByteBuf>) -> anyhow::Result<()> {
            Ok(())
        }
        async fn on_received_message(&self, _msg: Message<'_>) -> anyhow::Result<()> {
            Ok(())
        }
        fn should_transmit_have(&self, _id: ValidPieceIndex) -> bool {
            true
        }
        fn on_uploaded_bytes(&self, _bytes: u32) {}
        fn read_chunk(&self, chunk: &ChunkInfo, buf: &mut [u8]) -> anyhow::Result<()> {
            #[allow(clippy::cast_possible_truncation)]
            buf.fill(chunk.piece_index.get() as u8);
            Ok(())
        }
    }

    /// Counts the writes that reach the socket.
    struct CountingWriter<W> {
        inner: W,
        writes: Arc<AtomicUsize>,
    }

    impl<W: AsyncWrite + Unpin> AsyncWrite for CountingWriter<W> {
        fn poll_write(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &[u8],
        ) -> Poll<std::io::Result<usize>> {
            let r = Pin::new(&mut self.inner).poll_write(cx, buf);
            if matches!(r, Poll::Ready(Ok(_))) {
                self.writes.fetch_add(1, Ordering::Relaxed);
            }
            r
        }
        fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            Pin::new(&mut self.inner).poll_flush(cx)
        }
        fn poll_shutdown(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
        ) -> Poll<std::io::Result<()>> {
            Pin::new(&mut self.inner).poll_shutdown(cx)
        }
    }

    /// What the remote peer received after the handshake.
    #[derive(Debug, PartialEq)]
    enum Got {
        Have(u32),
        Piece(u32, u32, usize),
        Other(String),
    }

    async fn read_peer_side(
        mut stream: tokio::io::DuplexStream,
        got: mpsc::UnboundedSender<Got>,
    ) -> anyhow::Result<()> {
        let mut buf = Vec::new();
        let mut chunk = vec![0u8; 65536];
        let mut handshake_done = false;
        loop {
            if !handshake_done && buf.len() >= 68 {
                buf.drain(..68);
                handshake_done = true;
            }
            while handshake_done && let Ok((msg, size)) = Message::deserialize(&buf, &[]) {
                let item = match msg {
                    Message::Have(i) => Got::Have(i),
                    Message::Piece(p) => Got::Piece(p.index, p.begin, p.len()),
                    Message::Extended(_) => Got::Other("extended".into()),
                    Message::Bitfield(_) => Got::Other("bitfield".into()),
                    other => Got::Other(format!("{other:?}")),
                };
                let _ = got.send(item);
                buf.drain(..size);
            }
            let n = stream.read(&mut chunk).await?;
            if n == 0 {
                return Ok(());
            }
            buf.extend_from_slice(&chunk[..n]);
        }
    }

    struct Rig {
        mock: &'static Mock,
        #[allow(dead_code)]
        writes: Arc<AtomicUsize>,
        out_tx: mpsc::UnboundedSender<WriterRequest>,
        got: mpsc::UnboundedReceiver<Got>,
        conn: tokio::task::JoinHandle<crate::Result<()>>,
    }

    fn start(have: impl IntoIterator<Item = u32>, burst: Vec<u32>) -> Rig {
        let lengths = Lengths::new(u64::from(PIECES) * 16384, 16384).unwrap();
        let mut bits =
            bitvec::bitvec![u8, bitvec::order::Msb0; 0; PIECES as usize].into_boxed_bitslice();
        for p in have {
            bits.set(p as usize, true);
        }
        let (tx, rx) = broadcast::channel(128);
        let mock: &'static Mock = Box::leak(Box::new(Mock {
            lengths,
            have: Mutex::new(bits),
            tx,
            burst,
        }));

        let (ours, theirs) = tokio::io::duplex(1 << 20);
        let (r, w) = tokio::io::split(ours);
        let writes = Arc::new(AtomicUsize::new(0));
        let writer = CountingWriter {
            inner: w,
            writes: writes.clone(),
        };
        let info_hash = Id20::new([1; 20]);
        let addr: SocketAddr = "127.0.0.1:6881".parse().unwrap();
        let incoming = CheckedIncomingConnection {
            kind: ConnectionKind::Tcp,
            addr,
            reader: Box::new(r.into_vectored_compat()),
            writer: Box::new(writer),
            read_buf: ReadBuf::new(),
            handshake: Handshake::new(info_hash, Id20::new([2; 20])),
            encrypted: false,
        };
        let (out_tx, out_rx) = mpsc::unbounded_channel();
        let conn = tokio::spawn(async move {
            let conn = PeerConnection::new(
                addr,
                info_hash,
                Id20::new([3; 20]),
                mock,
                None,
                BlockingSpawner::new(1),
                Arc::new(StreamConnector::new(Default::default()).unwrap()),
            );
            conn.manage_peer_incoming(out_rx, incoming, rx).await
        });
        let (got_tx, got) = mpsc::unbounded_channel();
        tokio::spawn(read_peer_side(theirs, got_tx));
        Rig {
            mock,
            writes,
            out_tx,
            got,
            conn,
        }
    }

    async fn next_haves(got: &mut mpsc::UnboundedReceiver<Got>, n: usize) -> Vec<u32> {
        let mut haves = Vec::new();
        while haves.len() < n {
            let item = tokio::time::timeout(Duration::from_secs(5), got.recv())
                .await
                .expect("timed out waiting for HAVEs")
                .expect("connection closed");
            if let Got::Have(i) = item {
                haves.push(i);
            }
        }
        haves
    }

    /// Everything received after `Disconnect`, until the socket closes.
    async fn finish(mut rig: Rig) -> Vec<Got> {
        rig.out_tx.send(WriterRequest::Disconnect(Ok(()))).unwrap();
        let _ = rig.conn.await.unwrap();
        let mut rest = Vec::new();
        while let Some(item) = rig.got.recv().await {
            rest.push(item);
        }
        rest
    }

    /// 300 pieces complete before the writer reads the 128-slot broadcast:
    /// every one is announced exactly once, then later pieces as usual, and
    /// a piece already announced is not announced again.
    #[tokio::test]
    async fn lagging_writer_sends_every_have_once() {
        let mut rig = start(0..100, (100..400).collect());
        let mut haves = next_haves(&mut rig.got, 300).await;
        haves.sort_unstable();
        assert_eq!(haves, (100..400).collect::<Vec<_>>());

        for p in 400..410 {
            rig.mock.complete(p);
        }
        rig.mock.complete(150);
        let haves = next_haves(&mut rig.got, 10).await;
        assert_eq!(haves, (400..410).collect::<Vec<_>>());

        let rest = finish(rig).await;
        assert!(
            !rest.iter().any(|g| matches!(g, Got::Have(_))),
            "duplicate HAVEs: {rest:?}"
        );
    }
}
