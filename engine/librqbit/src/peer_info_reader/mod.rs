use std::{net::SocketAddr, sync::Arc};

use anyhow::{Context, bail};
use bencode::from_bytes;
use buffers::{ByteBuf, ByteBufOwned};
use bytes::Bytes;
use librqbit_core::{
    constants::CHUNK_SIZE,
    hash_id::{Id20, Id32},
    lengths::{ChunkInfo, last_element_size},
    torrent_metainfo::TorrentMetaV1Info,
};
use parking_lot::{Mutex, RwLock};
use peer_binary_protocol::{
    Handshake, Message,
    extended::{
        ExtendedMessage,
        handshake::ExtendedHandshake,
        ut_metadata::{UtMetadata, UtMetadataData},
    },
};
use sha1w::{ISha1, ISha256, Sha1, Sha256};
use tokio::sync::mpsc::UnboundedSender;
use tracing::trace;

pub(crate) mod budget;
pub(crate) mod v2_layers;
use budget::{FetchBudget, FetchReservation};
use v2_layers::{LayerFetch, Wanted};

/// Peers of one torrent fetching piece layers at the same time; the others
/// wait (their connection stays open) until one of these is done.
pub(crate) const MAX_LAYER_FETCHES_PER_TORRENT: usize = 2;
/// Peers of one magnet link downloading its metadata at the same time; the
/// others wait (connection open, nothing requested or allocated).
pub(crate) const MAX_METADATA_FETCHES_PER_TORRENT: usize = 2;
/// Largest metadata (info dictionary) we download.
pub(crate) const MAX_METADATA_SIZE: u32 = 32 * 1024 * 1024;
/// Bytes charged per metadata piece on top of its data.
const METADATA_PIECE_OVERHEAD: u64 = 64;

/// Bounds on the fetches of a metadata resolution: a session-wide byte
/// budget and the per-torrent slots for metadata and piece layer
/// downloads.
#[derive(Clone, Debug)]
pub(crate) struct FetchLimits {
    pub budget: Arc<FetchBudget>,
    pub slots: Arc<tokio::sync::Semaphore>,
    pub metadata_slots: Arc<tokio::sync::Semaphore>,
}

impl FetchLimits {
    /// Fresh slots for one torrent, sharing the session's budget.
    pub fn for_torrent(budget: Arc<FetchBudget>) -> Self {
        Self {
            budget,
            slots: Arc::new(tokio::sync::Semaphore::new(MAX_LAYER_FETCHES_PER_TORRENT)),
            metadata_slots: Arc::new(tokio::sync::Semaphore::new(
                MAX_METADATA_FETCHES_PER_TORRENT,
            )),
        }
    }
}

use crate::{
    peer_connection::{
        PeerConnection, PeerConnectionHandler, PeerConnectionOptions, WriterRequest,
    },
    spawn_utils::BlockingSpawner,
    stream_connect::{ConnectionKind, StreamConnector},
};

/// Metadata fetched from a peer.
#[derive(Debug)]
pub struct FetchedMetadata {
    pub info: TorrentMetaV1Info<ByteBufOwned>,
    pub info_bytes: ByteBufOwned,
    /// Bencoded BEP 52 `piece layers` of a pure v2 torrent (fetched with
    /// hash requests and verified), when it has files longer than a piece.
    pub piece_layers: Option<Bytes>,
}

/// `info_hash_v2`: set for a v2-only (`btmh`) magnet; `info_hash` is then
/// its truncated form. The metadata is checked with SHA-256 against it and
/// the piece layers are fetched too.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn read_metainfo_from_peer(
    addr: SocketAddr,
    peer_id: Id20,
    info_hash: Id20,
    info_hash_v2: Option<Id32>,
    limits: FetchLimits,
    peer_connection_options: Option<PeerConnectionOptions>,
    spawner: BlockingSpawner,
    connector: Arc<StreamConnector>,
    client_name_and_version: String,
) -> anyhow::Result<FetchedMetadata> {
    let (result_tx, result_rx) = tokio::sync::oneshot::channel::<anyhow::Result<FetchedMetadata>>();
    let (writer_tx, writer_rx) = tokio::sync::mpsc::unbounded_channel::<WriterRequest>();
    let handler = Handler {
        addr,
        info_hash,
        info_hash_v2,
        limits,
        metadata_size: Default::default(),
        layers: Mutex::new(None),
        writer_tx,
        result_tx: Mutex::new(Some(result_tx)),
        locked: RwLock::new(None),
        handshake_seen: Default::default(),
        client_name_and_version,
    };
    let connection = PeerConnection::new(
        addr,
        info_hash,
        peer_id,
        handler,
        peer_connection_options,
        spawner,
        connector,
    );

    let result_reader = result_rx;
    let (_, brx) = tokio::sync::broadcast::channel(1);
    let connection_runner = async move { connection.manage_peer_outgoing(writer_rx, brx).await };

    tokio::select! {
        result = result_reader => Ok(result??),
        whatever = connection_runner => match whatever {
            Ok(_) => anyhow::bail!("connection runner completed first"),
            Err(e) => Err(e.into())
        }
    }
}

/// A metadata download in progress: the pieces received so far, each
/// charged to the budget before it is kept; nothing is allocated for the
/// pieces still missing.
struct HandlerLocked {
    metadata_size: u32,
    total_pieces: usize,
    pieces: Vec<Option<Box<[u8]>>>,
    received: usize,
    reserved: FetchReservation,
    /// This peer's metadata slot, freed when the download ends.
    _slot: tokio::sync::OwnedSemaphorePermit,
}

impl HandlerLocked {
    fn new(
        metadata_size: u32,
        budget: Arc<FetchBudget>,
        slot: tokio::sync::OwnedSemaphorePermit,
    ) -> anyhow::Result<Self> {
        if metadata_size == 0 || metadata_size > MAX_METADATA_SIZE {
            anyhow::bail!("metadata size {} is too big", metadata_size);
        }
        let total_pieces: usize = (metadata_size as u64)
            .div_ceil(CHUNK_SIZE as u64)
            .try_into()?;
        let mut reserved = FetchReservation::new(budget);
        // The piece table itself (at most 2048 entries).
        reserved.grow((total_pieces * std::mem::size_of::<Option<Box<[u8]>>>()) as u64)?;
        Ok(Self {
            metadata_size,
            pieces: (0..total_pieces).map(|_| None).collect(),
            received: 0,
            reserved,
            total_pieces,
            _slot: slot,
        })
    }
    fn piece_size(&self, index: u32) -> usize {
        if index as usize == self.total_pieces - 1 {
            usize::try_from(last_element_size(
                self.metadata_size as u64,
                CHUNK_SIZE as u64,
            ))
            .unwrap_or(CHUNK_SIZE as usize)
        } else {
            CHUNK_SIZE as usize
        }
    }
    /// Keeps a piece (charged to the budget first); when the last one
    /// arrived, checks the whole metadata against the info hash and returns
    /// it.
    fn record_piece(
        &mut self,
        d: &UtMetadataData<ByteBuf>,
        info_hash: &Id20,
        info_hash_v2: Option<&Id32>,
    ) -> anyhow::Result<Option<Bytes>> {
        let piece = d.piece();
        if piece as usize >= self.total_pieces {
            anyhow::bail!("wrong index");
        }
        let size = self.piece_size(piece);
        if d.len() != size {
            anyhow::bail!(
                "expected length of piece {} to be {}, but got {}",
                piece,
                size,
                d.len()
            );
        }
        if self.pieces[piece as usize].is_some() {
            anyhow::bail!("already received piece {}", piece);
        }
        self.reserved.grow(size as u64 + METADATA_PIECE_OVERHEAD)?;
        let mut buf = vec![0u8; size].into_boxed_slice();
        d.copy_to_slice(&mut buf);
        self.pieces[piece as usize] = Some(buf);
        self.received += 1;
        if self.received < self.total_pieces {
            return Ok(None);
        }
        // Check the metadata piece by piece, then (only then) join it.
        let pieces = self.pieces.iter().flatten();
        let valid = match info_hash_v2 {
            Some(v2) => {
                let mut hash = Sha256::new();
                pieces.for_each(|p| hash.update(p));
                hash.finish() == v2.0
            }
            None => {
                let mut hash = Sha1::new();
                pieces.for_each(|p| hash.update(p));
                hash.finish() == info_hash.0
            }
        };
        if !valid {
            anyhow::bail!("info checksum invalid");
        }
        let mut out = Vec::with_capacity(self.metadata_size as usize);
        for p in self.pieces.iter().flatten() {
            out.extend_from_slice(p);
        }
        Ok(Some(out.into()))
    }
}

/// Metadata waiting for its piece layers.
struct PendingLayers {
    info: TorrentMetaV1Info<ByteBufOwned>,
    info_bytes: ByteBufOwned,
    fetch: LayerFetch,
    /// This peer's fetch slot, freed when the fetch ends.
    _slot: tokio::sync::OwnedSemaphorePermit,
}

struct Handler {
    addr: SocketAddr,
    info_hash: Id20,
    info_hash_v2: Option<Id32>,
    limits: FetchLimits,
    /// `metadata_size` from the peer's extended handshake (0: none yet).
    metadata_size: std::sync::atomic::AtomicU32,
    layers: Mutex<Option<PendingLayers>>,
    writer_tx: UnboundedSender<WriterRequest>,
    result_tx: Mutex<Option<tokio::sync::oneshot::Sender<anyhow::Result<FetchedMetadata>>>>,
    locked: RwLock<Option<HandlerLocked>>,
    handshake_seen: std::sync::atomic::AtomicBool,
    client_name_and_version: String,
}

impl Handler {
    fn send_result(&self, r: anyhow::Result<FetchedMetadata>) -> anyhow::Result<()> {
        self.result_tx
            .lock()
            .take()
            .ok_or_else(|| anyhow::anyhow!("oneshot is consumed"))?
            .send(r)
            .map_err(|_| anyhow::anyhow!("torrent info deserialized, but consumer closed"))
    }

    /// The metadata is complete and verified: done, or (pure v2 via a
    /// `btmh` magnet) start fetching the piece layers.
    async fn on_metadata(&self, buf: Bytes) -> anyhow::Result<()> {
        let info = match from_bytes::<TorrentMetaV1Info<ByteBuf>>(&buf) {
            Ok(i) => {
                use clone_to_owned::CloneToOwned;
                i.clone_to_owned(Some(&buf))
            }
            Err(e) => {
                trace!("error deserializing TorrentMetaV1Info: {e:#}");
                return self.send_result(Err(e.into_kind().into()));
            }
        };
        let info_bytes = ByteBufOwned(buf);
        // A hybrid torrent (verified against its v2 hash) needs no piece
        // layers: it runs on its v1 piece hashes.
        if self.info_hash_v2.is_some() && info.is_pure_v2() {
            let fetch = match info.clone().validate() {
                Ok(v) => v.v2_layout_arc().and_then(|layout| {
                    LayerFetch::new(layout, Wanted::All, self.limits.budget.clone())
                }),
                Err(e) => return self.send_result(Err(e.into())),
            };
            if let Some(mut fetch) = fetch {
                // At most a couple of peers per torrent fetch at once: wait
                // for a slot (no reads meanwhile, nothing allocated).
                let slot = self.limits.slots.clone().acquire_owned().await?;
                for r in fetch.next_requests()? {
                    self.writer_tx
                        .send(WriterRequest::Message(Message::HashRequest(r)))?;
                }
                *self.layers.lock() = Some(PendingLayers {
                    info,
                    info_bytes,
                    fetch,
                    _slot: slot,
                });
                return Ok(());
            }
        }
        self.send_result(Ok(FetchedMetadata {
            info,
            info_bytes,
            piece_layers: None,
        }))
    }

    fn on_layer_message(&self, msg: &Message<'_>) -> anyhow::Result<()> {
        let mut g = self.layers.lock();
        let Some(p) = g.as_mut() else {
            bail!("unexpected BEP 52 hashes message");
        };
        match msg {
            Message::Hashes(req, data) => p.fetch.on_hashes(req, data.as_ref())?,
            Message::HashReject(req) => p.fetch.on_reject(req)?,
            _ => return Ok(()),
        }
        for r in p.fetch.next_requests()? {
            self.writer_tx
                .send(WriterRequest::Message(Message::HashRequest(r)))?;
        }
        if p.fetch.is_done() {
            let PendingLayers {
                info,
                info_bytes,
                fetch,
                ..
            } = g.take().context("bug: pending layers vanished")?;
            drop(g);
            return self.send_result(Ok(FetchedMetadata {
                info,
                info_bytes,
                piece_layers: Some(fetch.into_piece_layers()),
            }));
        }
        Ok(())
    }
}

impl PeerConnectionHandler for Handler {
    fn handshake_v2(&self) -> bool {
        self.info_hash_v2.is_some()
    }

    fn should_send_bitfield(&self) -> bool {
        false
    }

    fn serialize_bitfield_message_to_buf(
        &self,
        _buf: &mut [u8],
        _advertised: &mut crate::advertised_haves::AdvertisedHaves,
    ) -> anyhow::Result<usize> {
        Ok(0)
    }

    fn on_handshake(
        &self,
        handshake: Handshake,
        _kind: ConnectionKind,
        _encrypted: bool,
    ) -> anyhow::Result<()> {
        if !handshake.supports_extended() {
            anyhow::bail!(
                "this peer does not support extended handshaking, which is a prerequisite to download metadata"
            )
        }
        Ok(())
    }

    async fn on_received_message(&self, msg: Message<'_>) -> anyhow::Result<()> {
        trace!("{}: received message: {:?}", self.addr, msg);

        if matches!(msg, Message::Hashes(..) | Message::HashReject(..)) {
            return self.on_layer_message(&msg);
        }
        if let Message::Extended(ExtendedMessage::UtMetadata(UtMetadata::Data(utdata))) = msg {
            // Data before the extended handshake (or after completion) is a
            // protocol violation, never a panic.
            let finished = {
                let mut locked = self.locked.write();
                let Some(l) = locked.as_mut() else {
                    anyhow::bail!("ut_metadata data we didn't ask for");
                };
                let done = l.record_piece(&utdata, &self.info_hash, self.info_hash_v2.as_ref())?;
                if done.is_some() {
                    // Frees the pieces, their budget and the slot.
                    locked.take();
                }
                done
            };
            if let Some(done) = finished {
                self.on_metadata(done).await?;
            }
        }
        Ok(())
    }

    async fn after_extended_handshake(&self) -> anyhow::Result<()> {
        let metadata_size = self
            .metadata_size
            .load(std::sync::atomic::Ordering::Relaxed);
        if metadata_size == 0 || self.locked.read().is_some() {
            return Ok(());
        }
        // At most a couple of peers per magnet download the metadata at
        // once; this one waits for a slot, with nothing requested or
        // allocated.
        let slot = self.limits.metadata_slots.clone().acquire_owned().await?;
        let inner = HandlerLocked::new(metadata_size, self.limits.budget.clone(), slot)?;
        let total_pieces = inner.total_pieces;
        self.locked.write().replace(inner);
        for i in 0..total_pieces {
            self.writer_tx
                .send(WriterRequest::Message(Message::Extended(
                    ExtendedMessage::UtMetadata(UtMetadata::Request(i.try_into()?)),
                )))?;
        }
        Ok(())
    }

    fn on_uploaded_bytes(&self, _bytes: u32) {}

    fn read_chunk(&self, _chunk: &ChunkInfo, _buf: &mut [u8]) -> anyhow::Result<()> {
        anyhow::bail!("the peer is not supposed to be requesting chunks")
    }

    fn on_extended_handshake(
        &self,
        extended_handshake: &ExtendedHandshake<ByteBuf>,
    ) -> anyhow::Result<()> {
        let metadata_size = match extended_handshake.metadata_size {
            Some(metadata_size) => metadata_size,
            None => anyhow::bail!("peer does not have metadata_size"),
        };

        if extended_handshake.m.ut_metadata.is_none() {
            anyhow::bail!("peer does not support ut_metadata");
        }
        // Every handshake would queue another round of requests on the
        // unbounded writer channel: accept only the first one.
        if self
            .handshake_seen
            .swap(true, std::sync::atomic::Ordering::Relaxed)
        {
            anyhow::bail!("repeated extended handshake");
        }

        if metadata_size == 0 || metadata_size > MAX_METADATA_SIZE {
            anyhow::bail!("metadata size {} is too big", metadata_size);
        }

        self.writer_tx
            .send(WriterRequest::Message(Message::Unchoke))?;
        self.writer_tx
            .send(WriterRequest::Message(Message::Interested))?;
        // The pieces are requested once a slot is free
        // (`after_extended_handshake`).
        self.metadata_size
            .store(metadata_size, std::sync::atomic::Ordering::Relaxed);
        Ok(())
    }

    fn should_transmit_have(&self, _id: librqbit_core::lengths::ValidPieceIndex) -> bool {
        false
    }

    fn client_name_and_version(&self) -> &str {
        &self.client_name_and_version
    }
}

#[cfg(test)]
#[allow(clippy::cast_possible_truncation)]
mod tests {
    use super::*;
    use peer_binary_protocol::extended::PeerExtendedMessageIds;

    fn handler() -> (Handler, tokio::sync::mpsc::UnboundedReceiver<WriterRequest>) {
        let (writer_tx, writer_rx) = tokio::sync::mpsc::unbounded_channel();
        let (result_tx, _result_rx) = tokio::sync::oneshot::channel();
        let h = Handler {
            addr: "127.0.0.1:1".parse().unwrap(),
            info_hash: Id20::new([0; 20]),
            info_hash_v2: None,
            limits: FetchLimits::for_torrent(FetchBudget::new(u64::MAX)),
            metadata_size: Default::default(),
            layers: Mutex::new(None),
            writer_tx,
            result_tx: Mutex::new(Some(result_tx)),
            locked: RwLock::new(None),
            handshake_seen: Default::default(),
            client_name_and_version: String::new(),
        };
        (h, writer_rx)
    }

    /// Layer fetches of one torrent: at most MAX_LAYER_FETCHES_PER_TORRENT
    /// peers fetch at once, the next one waits (without requesting or
    /// allocating) until one of them is done.
    #[tokio::test]
    async fn layer_fetches_per_torrent_are_limited() {
        let (info, _, _) = v2_layers::tests::torrent(1500);
        let info = Bytes::from(info);
        let budget = FetchBudget::new(u64::MAX);
        let limits = FetchLimits::for_torrent(budget.clone());
        let mk = || {
            let (mut h, rx) = handler();
            h.info_hash_v2 = Some(Id32::new([1; 32]));
            h.limits = limits.clone();
            (h, rx)
        };
        // The metadata of a `btmh` magnet arrived and was verified: each
        // peer then fetches the piece layers.
        let mut started = Vec::new();
        for _ in 0..MAX_LAYER_FETCHES_PER_TORRENT {
            let (h, mut rx) = mk();
            h.on_metadata(info.clone()).await.unwrap();
            assert!(rx.try_recv().is_ok(), "hash requests sent");
            started.push((h, rx));
        }
        let (waiting, mut wrx) = mk();
        let used;
        {
            let fut = waiting.on_metadata(info.clone());
            tokio::pin!(fut);
            assert!(
                tokio::time::timeout(std::time::Duration::from_millis(100), &mut fut)
                    .await
                    .is_err(),
                "a third fetch waits for a slot"
            );
            assert!(wrx.try_recv().is_err(), "nothing requested while waiting");
            used = budget.used();
            // One fetch ends (its peer is gone): the waiting one starts.
            drop(started.pop());
            tokio::time::timeout(std::time::Duration::from_secs(5), &mut fut)
                .await
                .expect("slot freed")
                .unwrap();
        }
        assert!(wrx.try_recv().is_ok());
        assert_eq!(
            budget.used(),
            used,
            "one fetch's bookkeeping freed, one taken"
        );
        drop(started);
        drop(waiting);
        assert_eq!(budget.used(), 0);
    }

    #[test]
    fn metadata_before_extended_handshake_is_an_error_not_a_panic() {
        let (h, _rx) = handler();
        let msg = Message::Extended(ExtendedMessage::UtMetadata(UtMetadata::Data(
            UtMetadataData::from_bytes(0, 1, ByteBuf(b"X")),
        )));
        assert!(pollster::block_on(h.on_received_message(msg)).is_err());
    }

    fn metadata_handshake(size: u32) -> ExtendedHandshake<ByteBuf<'static>> {
        ExtendedHandshake::<ByteBuf> {
            m: PeerExtendedMessageIds {
                ut_metadata: Some(3),
                ut_pex: None,
            },
            metadata_size: Some(size),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn repeated_extended_handshakes_do_not_queue_more_requests() {
        let (h, mut rx) = handler();
        let hs = metadata_handshake(MAX_METADATA_SIZE);
        h.on_extended_handshake(&hs).unwrap();
        h.after_extended_handshake().await.unwrap();
        let mut queued = 0;
        while rx.try_recv().is_ok() {
            queued += 1;
        }
        assert_eq!(queued, 2 + 2048);
        assert!(h.on_extended_handshake(&hs).is_err());
        h.after_extended_handshake().await.unwrap();
        assert!(rx.try_recv().is_err());

        for size in [0, MAX_METADATA_SIZE + 1, u32::MAX] {
            let (h, _rx) = handler();
            assert!(h.on_extended_handshake(&metadata_handshake(size)).is_err());
        }
    }

    /// Metadata of `len` bytes and its SHA-1.
    fn metadata(len: usize) -> (Vec<u8>, Id20) {
        let data: Vec<u8> = (0..len).map(|i| (i * 7 % 251) as u8).collect();
        let mut h = Sha1::new();
        h.update(&data);
        (data, Id20::new(h.finish()))
    }

    fn piece(data: &[u8], i: u32) -> UtMetadataData<ByteBuf<'_>> {
        let start = i as usize * CHUNK_SIZE as usize;
        let end = (start + CHUNK_SIZE as usize).min(data.len());
        UtMetadataData::from_bytes(i, data.len() as u32, ByteBuf(&data[start..end]))
    }

    fn slot() -> tokio::sync::OwnedSemaphorePermit {
        Arc::new(tokio::sync::Semaphore::new(1))
            .try_acquire_owned()
            .unwrap()
    }

    /// A hostile peer announcing the largest size gets nothing allocated
    /// for it; pieces are charged as they arrive, malformed ones never; the
    /// metadata is checked at the end and everything is returned.
    #[test]
    fn metadata_pieces_are_charged_as_they_arrive() {
        let budget = FetchBudget::new(u64::MAX);
        let big = HandlerLocked::new(MAX_METADATA_SIZE, budget.clone(), slot()).unwrap();
        assert!(
            budget.used() < 64 * 1024,
            "only the piece table: {}",
            budget.used()
        );
        drop(big);
        assert_eq!(budget.used(), 0);

        let (data, ih) = metadata(40_000);
        let mut l = HandlerLocked::new(data.len() as u32, budget.clone(), slot()).unwrap();
        let table = budget.used();
        assert!(
            l.record_piece(&piece(&data, 2), &ih, None)
                .unwrap()
                .is_none()
        );
        let after_one = budget.used();
        assert_eq!(
            after_one,
            table + (40_000 - 2 * 16384) + METADATA_PIECE_OVERHEAD
        );
        // Garbage is refused before anything is charged: a wrong index, a
        // wrong size, a duplicate.
        let bad_index = UtMetadataData::from_bytes(3, 40_000, ByteBuf(&data[..100]));
        assert!(l.record_piece(&bad_index, &ih, None).is_err());
        let short = UtMetadataData::from_bytes(0, 40_000, ByteBuf(&data[..100]));
        assert!(l.record_piece(&short, &ih, None).is_err());
        assert!(l.record_piece(&piece(&data, 2), &ih, None).is_err());
        assert_eq!(budget.used(), after_one);
        assert!(
            l.record_piece(&piece(&data, 0), &ih, None)
                .unwrap()
                .is_none()
        );
        let done = l
            .record_piece(&piece(&data, 1), &ih, None)
            .unwrap()
            .unwrap();
        assert_eq!(&done[..], &data[..]);
        drop(l);
        assert_eq!(budget.used(), 0);

        // Wrong content is caught by the hash at the end.
        let mut l = HandlerLocked::new(data.len() as u32, budget.clone(), slot()).unwrap();
        let mut forged = data.clone();
        forged[5] ^= 1;
        for i in 0..2 {
            assert!(
                l.record_piece(&piece(&forged, i), &ih, None)
                    .unwrap()
                    .is_none()
            );
        }
        assert!(l.record_piece(&piece(&forged, 2), &ih, None).is_err());
        drop(l);
        assert_eq!(budget.used(), 0);
    }

    /// Many peers sending honest metadata at once: together they never hold
    /// more than the budget; those that would go over fail, and everything
    /// is returned.
    #[test]
    fn many_metadata_downloads_stay_within_the_budget() {
        let (data, ih) = metadata(1024 * 1024);
        let limit = 1024 * 1024 * 3 / 2;
        let budget = FetchBudget::new(limit);
        let mut downloads: Vec<Option<(HandlerLocked, u32)>> = (0..20)
            .map(|_| {
                HandlerLocked::new(data.len() as u32, budget.clone(), slot())
                    .ok()
                    .map(|l| (l, 0))
            })
            .collect();
        let (mut completed, mut failed) = (0, 0);
        loop {
            let mut active = false;
            for d in downloads.iter_mut() {
                let Some((l, next)) = d.as_mut() else {
                    continue;
                };
                active = true;
                match l.record_piece(&piece(&data, *next), &ih, None) {
                    Ok(Some(done)) => {
                        assert_eq!(done.len(), data.len());
                        completed += 1;
                        *d = None;
                    }
                    Ok(None) => *next += 1,
                    Err(_) => {
                        failed += 1;
                        *d = None;
                    }
                }
                assert!(budget.used() <= limit);
            }
            if !active {
                break;
            }
        }
        assert!(completed >= 1, "at least one download completes");
        assert!(failed > 0, "the others hit the budget");
        assert_eq!(completed + failed, 20);
        assert_eq!(budget.used(), 0);
    }

    /// At most MAX_METADATA_FETCHES_PER_TORRENT peers of a magnet download
    /// its metadata at once; the next waits with nothing requested.
    #[tokio::test]
    async fn metadata_downloads_per_torrent_are_limited() {
        let limits = FetchLimits::for_torrent(FetchBudget::new(u64::MAX));
        let mk = || {
            let (mut h, rx) = handler();
            h.limits = limits.clone();
            (h, rx)
        };
        let hs = metadata_handshake(40_000);
        let mut started = Vec::new();
        for _ in 0..MAX_METADATA_FETCHES_PER_TORRENT {
            let (h, rx) = mk();
            h.on_extended_handshake(&hs).unwrap();
            h.after_extended_handshake().await.unwrap();
            assert!(h.locked.read().is_some());
            started.push((h, rx));
        }
        let (waiting, mut wrx) = mk();
        waiting.on_extended_handshake(&hs).unwrap();
        while wrx.try_recv().is_ok() {} // unchoke, interested
        {
            let fut = waiting.after_extended_handshake();
            tokio::pin!(fut);
            assert!(
                tokio::time::timeout(std::time::Duration::from_millis(100), &mut fut)
                    .await
                    .is_err(),
                "a third download waits for a slot"
            );
            assert!(wrx.try_recv().is_err(), "nothing requested while waiting");
            assert!(
                waiting.locked.read().is_none(),
                "nothing allocated while waiting"
            );
            drop(started.pop());
            tokio::time::timeout(std::time::Duration::from_secs(5), &mut fut)
                .await
                .expect("slot freed")
                .unwrap();
        }
        assert!(wrx.try_recv().is_ok(), "requests sent once started");
    }
}
