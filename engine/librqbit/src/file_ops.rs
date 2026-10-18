use std::{
    marker::PhantomData,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
};

use anyhow::{Context, bail};
use buffers::{ByteBuf, ByteBufOwned};
use librqbit_core::{
    lengths::{ChunkInfo, ValidPieceIndex},
    torrent_metainfo::{PieceCheck, ValidatedTorrentMetaV1Info},
    v2::{Hash32, MerkleHasher, V2Layout, V2PieceCheck, V2PieceHashes},
};
use peer_binary_protocol::{DoubleBufHelper, Piece};
use sha1w::{ISha1, Sha1};
use tracing::{debug, trace, warn};

use crate::{
    file_info::FileInfo,
    storage::TorrentStorage,
    type_aliases::{BF, BS, FileInfos, PeerHandle},
};

/// Hashes one piece: SHA-1 over the whole piece for v1 and hybrid
/// torrents, the BEP 52 merkle root over the real file data (padding is
/// never hashed) for pure v2 torrents.
pub(crate) enum PieceHasher {
    V1 {
        hash: Sha1,
        expected: [u8; 20],
    },
    V2 {
        hash: MerkleHasher,
        check: V2PieceCheck,
        fed: u64,
    },
    /// A pure v2 piece whose file's layer isn't known yet: it can't be
    /// verified on its own. Its merkle root is computed (all the pieces of
    /// a complete file hash up to the file's `pieces root`, which yields
    /// the layer, as libtorrent finds it), and whether any of its data is
    /// there (not all zeros).
    Unknown {
        hash: MerkleHasher,
        leaves: u64,
        data: bool,
    },
}

impl PieceHasher {
    pub fn new(
        torrent: &ValidatedTorrentMetaV1Info<ByteBufOwned>,
        piece: ValidPieceIndex,
    ) -> anyhow::Result<Self> {
        match torrent.piece_check(piece.get()) {
            Some(PieceCheck::V1(expected)) => Ok(Self::V1 {
                hash: Sha1::new(),
                expected,
            }),
            Some(PieceCheck::V2(check)) => Ok(Self::V2 {
                hash: MerkleHasher::new(),
                check,
                fed: 0,
            }),
            None => bail!("no hash known for piece {piece}"),
        }
    }

    /// Like [`Self::new`], but a pure v2 piece whose file's layer isn't
    /// known yet gets [`Self::Unknown`].
    fn new_or_unknown(
        torrent: &ValidatedTorrentMetaV1Info<ByteBufOwned>,
        piece: ValidPieceIndex,
    ) -> anyhow::Result<Self> {
        if let Some(layout) = torrent.v2_layout()
            && !torrent.piece_hash_known(piece.get())
        {
            return Ok(Self::Unknown {
                hash: MerkleHasher::new(),
                // Files with a layer are longer than a piece: full pieces.
                leaves: layout.blocks_per_piece(),
                data: false,
            });
        }
        Self::new(torrent, piece)
    }

    /// [`Self::Unknown`]: the piece's merkle root.
    fn unknown_root(self) -> Option<Hash32> {
        match self {
            Self::Unknown { hash, leaves, .. } => hash.finish(leaves),
            _ => None,
        }
    }

    fn hashes_padding(&self) -> bool {
        matches!(self, Self::V1 { .. })
    }

    fn update(&mut self, data: &[u8]) {
        match self {
            Self::V1 { hash, .. } => hash.update(data),
            Self::V2 { hash, fed, .. } => {
                *fed += data.len() as u64;
                hash.update(data);
            }
            Self::Unknown { hash, data: d, .. } => {
                *d = *d || data.iter().any(|&b| b != 0);
                hash.update(data);
            }
        }
    }

    fn matches(self) -> bool {
        match self {
            Self::V1 { hash, expected } => hash.finish() == expected,
            Self::V2 { hash, check, fed } => {
                fed == check.data_len && hash.finish(check.leaves) == Some(check.expected)
            }
            Self::Unknown { .. } => false,
        }
    }
}

/// Pure v2 files whose piece layer is unknown, read piece by piece in
/// order: the merkle roots of a file's pieces are collected, and when the
/// whole file hashes up to its `pieces root` they are its layer (libtorrent
/// verifies such a file the same way). Holds one file's roots at a time.
struct LayerFinder<'a> {
    layout: &'a V2Layout,
    hashes: &'a V2PieceHashes,
    span: Option<usize>,
    roots: Vec<Hash32>,
    failed: bool,
}

impl<'a> LayerFinder<'a> {
    fn new(torrent: &'a ValidatedTorrentMetaV1Info<ByteBufOwned>) -> Option<Self> {
        let hashes = torrent.v2_piece_hashes()?;
        let layout = torrent.v2_layout()?;
        (!hashes.is_complete()).then(|| Self {
            layout,
            hashes,
            span: None,
            roots: Vec::new(),
            failed: false,
        })
    }

    /// The root of `piece` (`None`: unreadable). Returns the file's piece
    /// range once its layer was found and added.
    fn piece(&mut self, piece: u32, root: Option<Hash32>) -> Option<std::ops::Range<usize>> {
        let span = self.layout.span_index_of_piece(piece)?;
        if self.span != Some(span) {
            self.span = Some(span);
            self.roots = Vec::new();
            self.failed = false;
        }
        let s = &self.layout.spans[span];
        match root {
            Some(r)
                if !self.failed
                    && u64::from(piece) == u64::from(s.first_piece) + self.roots.len() as u64 =>
            {
                self.roots.push(r)
            }
            _ => {
                self.failed = true;
                self.roots = Vec::new();
            }
        }
        if self.failed || self.roots.len() < s.num_pieces as usize {
            return None;
        }
        let layer = std::mem::take(&mut self.roots);
        // Checked against the pieces root; a mismatch leaves it unknown.
        match self.hashes.set_layer(self.layout, &s.pieces_root, &layer) {
            Ok(spans) if spans.contains(&span) => {
                let first = s.first_piece as usize;
                Some(first..first + s.num_pieces as usize)
            }
            _ => None,
        }
    }
}

/// What the initial check found.
pub(crate) struct InitialCheck {
    /// Pieces whose hash matched.
    pub have: BF,
    /// Pure v2 torrents with missing piece layers: pieces that couldn't be
    /// verified yet (their file's layer is unknown) but have data on disk.
    /// They are checked once the layer arrives instead of downloaded.
    pub unverified_v2: Option<BF>,
    /// Layers found on disk: files whose data hashed up to their `pieces
    /// root` (their pieces are in `have`).
    pub derived_layers: usize,
}

pub(crate) fn update_hash_from_file(
    file_id: usize,
    file_info: &FileInfo,
    mut pos: u64,
    files: &dyn TorrentStorage,
    hash: &mut PieceHasher,
    buf: &mut [u8],
    mut bytes_to_read: usize,
) -> anyhow::Result<()> {
    if file_info.attrs.padding && !hash.hashes_padding() {
        return Ok(());
    }
    let mut read = 0;
    while bytes_to_read > 0 {
        let chunk = std::cmp::min(buf.len(), bytes_to_read);
        if file_info.attrs.padding {
            buf[..chunk].fill(0);
        } else {
            files
                .pread_exact(file_id, pos, &mut buf[..chunk])
                .with_context(|| {
                    format!("failed reading chunk of size {chunk}, read so far {read}")
                })?;
        }
        bytes_to_read -= chunk;
        read += chunk;
        pos += chunk as u64;
        hash.update(&buf[..chunk]);
    }
    Ok(())
}

pub(crate) struct FileOps<'a> {
    torrent: &'a ValidatedTorrentMetaV1Info<ByteBufOwned>,
    files: &'a dyn TorrentStorage,
    file_infos: &'a FileInfos,
    phantom_data: PhantomData<Sha1>,
}

impl<'a> FileOps<'a> {
    pub fn new(
        torrent: &'a ValidatedTorrentMetaV1Info<ByteBufOwned>,
        files: &'a dyn TorrentStorage,
        file_infos: &'a FileInfos,
    ) -> Self {
        Self {
            torrent,
            files,
            file_infos,
            phantom_data: PhantomData,
        }
    }

    // Returns the bitvector with pieces we have.
    pub fn initial_check(
        &self,
        progress: &AtomicU64,
        pause_requested: &AtomicBool,
    ) -> anyhow::Result<InitialCheck> {
        let new_bf =
            || BF::from_boxed_slice(vec![0u8; self.torrent.lengths().piece_bitfield_bytes()].into());
        let mut have_pieces = new_bf();
        let mut unverified_v2 = (!self.torrent.has_piece_hashes()).then(new_bf);
        let mut finder = LayerFinder::new(self.torrent);
        let mut derived_layers = 0;
        let mut piece_files = Vec::<usize>::new();

        #[derive(Debug)]
        struct CurrentFile<'a> {
            index: usize,
            fi: &'a FileInfo,
            processed_bytes: u64,
            is_broken: bool,
        }
        impl CurrentFile<'_> {
            fn remaining(&self) -> u64 {
                self.fi.len - self.processed_bytes
            }
            fn mark_processed_bytes(&mut self, bytes: u64) {
                self.processed_bytes += bytes
            }
        }
        let mut file_iterator = self
            .file_infos
            .iter()
            .enumerate()
            .map(|(idx, fi)| CurrentFile {
                index: idx,
                fi,
                processed_bytes: 0,
                is_broken: false,
            });

        let mut current_file = file_iterator.next().context("empty input file list")?;

        let mut read_buffer = vec![0u8; 65536];

        for piece_info in self.torrent.lengths().iter_piece_infos() {
            if pause_requested.load(Ordering::Relaxed) {
                bail!("initial check paused");
            }

            piece_files.clear();
            let mut computed_hash =
                PieceHasher::new_or_unknown(self.torrent, piece_info.piece_index)?;
            let mut piece_remaining = piece_info.len as usize;
            let mut some_files_broken = false;
            progress.fetch_add(piece_info.len as u64, Ordering::Relaxed);

            while piece_remaining > 0 {
                let mut to_read_in_file: usize =
                    std::cmp::min(current_file.remaining(), piece_remaining as u64).try_into()?;

                // Keep changing the current file to next until we find a file that has greater than 0 length.
                while to_read_in_file == 0 {
                    current_file = file_iterator.next().context("broken torrent metadata")?;

                    to_read_in_file =
                        std::cmp::min(current_file.remaining(), piece_remaining as u64)
                            .try_into()?;
                }

                piece_files.push(current_file.index);

                let pos = current_file.processed_bytes;
                piece_remaining -= to_read_in_file;
                current_file.mark_processed_bytes(to_read_in_file as u64);

                if current_file.is_broken {
                    // no need to read.
                    continue;
                }

                if let Err(err) = update_hash_from_file(
                    current_file.index,
                    current_file.fi,
                    pos,
                    self.files,
                    &mut computed_hash,
                    &mut read_buffer,
                    to_read_in_file,
                ) {
                    debug!(
                        "error reading from file {} ({:?}) at {}: {:#}",
                        current_file.index, current_file.fi.relative_filename, pos, &err
                    );
                    current_file.is_broken = true;
                    some_files_broken = true;
                }
            }

            let piece = piece_info.piece_index.get();
            if some_files_broken {
                trace!("piece {} had errors, marking as needed", piece);
                if let (PieceHasher::Unknown { .. }, Some(f)) = (&computed_hash, finder.as_mut()) {
                    f.piece(piece, None);
                }
                continue;
            }

            if let PieceHasher::Unknown { data, .. } = &computed_hash {
                let data = *data;
                if data && let Some(u) = unverified_v2.as_mut() {
                    u.set(piece as usize, true);
                }
                let root = computed_hash.unknown_root();
                if let Some(range) = finder.as_mut().and_then(|f| f.piece(piece, root)) {
                    // The whole file hashed up to its root: verified.
                    derived_layers += 1;
                    if let Some(u) = unverified_v2.as_mut()
                        && let Some(r) = u.get_mut(range.clone())
                    {
                        r.fill(false);
                    }
                    if let Some(r) = have_pieces.get_mut(range) {
                        r.fill(true);
                    }
                }
            } else if computed_hash.matches() {
                have_pieces.set(piece as usize, true);
            }
        }

        Ok(InitialCheck {
            have: have_pieces,
            unverified_v2,
            derived_layers,
        })
    }

    /// Pure v2: hashes the files whose layer is unknown and whose pieces
    /// are all in `claimed` (data said to be on disk, e.g. by fast-resume
    /// data) from disk; those that hash up to their `pieces root` get their
    /// layer. Returns the piece ranges of those files (verified now).
    pub fn find_layers(&self, claimed: &BS) -> Vec<std::ops::Range<usize>> {
        let (Some(layout), Some(mut finder)) = (self.torrent.v2_layout(), LayerFinder::new(self.torrent))
        else {
            return Vec::new();
        };
        let mut found = Vec::new();
        for (i, s) in layout.spans.iter().enumerate() {
            let range = s.first_piece as usize..(s.first_piece + s.num_pieces) as usize;
            if finder.hashes.span_known(i)
                || !claimed.get(range.clone()).is_some_and(|r| r.all())
            {
                continue;
            }
            for p in s.first_piece..s.first_piece + s.num_pieces {
                let root = self
                    .torrent
                    .lengths()
                    .validate_piece_index(p)
                    .and_then(|p| {
                        let h = PieceHasher::new_or_unknown(self.torrent, p).ok()?;
                        self.hash_piece(p, h).ok()
                    })
                    .and_then(PieceHasher::unknown_root);
                if let Some(r) = finder.piece(p, root) {
                    found.push(r);
                }
                if finder.failed {
                    break;
                }
            }
        }
        found
    }

    pub fn check_piece(&self, piece_index: ValidPieceIndex) -> anyhow::Result<bool> {
        if cfg!(feature = "_disable_disk_write_net_benchmark") {
            return Ok(true);
        }

        let h = PieceHasher::new(self.torrent, piece_index)?;
        let h = self.hash_piece(piece_index, h)?;
        if h.matches() {
            trace!("piece={} hash matches", piece_index);
            Ok(true)
        } else {
            let piece_length = self.torrent.lengths().piece_length(piece_index);
            let absolute_offset = self.torrent.lengths().piece_offset(piece_index);
            warn!(
                piece_length,
                absolute_offset, "the piece={} hash does not match", piece_index
            );
            Ok(false)
        }
    }

    /// Feeds the piece's data from disk to `h`.
    fn hash_piece(
        &self,
        piece_index: ValidPieceIndex,
        mut h: PieceHasher,
    ) -> anyhow::Result<PieceHasher> {
        let piece_length = self.torrent.lengths().piece_length(piece_index);
        let mut absolute_offset = self.torrent.lengths().piece_offset(piece_index);
        let mut buf = vec![0u8; std::cmp::min(65536, piece_length as usize)];

        let mut piece_remaining_bytes = piece_length as usize;

        for (file_idx, fi) in self.file_infos.iter().enumerate() {
            let file_len = fi.len;
            if absolute_offset > file_len {
                absolute_offset -= file_len;
                continue;
            }
            let file_remaining_len = file_len - absolute_offset;

            let to_read_in_file: usize =
                std::cmp::min(file_remaining_len, piece_remaining_bytes as u64).try_into()?;
            trace!(
                "piece={}, file_idx={}, seeking to {}",
                piece_index, file_idx, absolute_offset,
            );
            update_hash_from_file(
                file_idx,
                fi,
                absolute_offset,
                self.files,
                &mut h,
                &mut buf,
                to_read_in_file,
            )
            .with_context(|| {
                format!(
                    "error reading {to_read_in_file} bytes, file_id: {file_idx} (\"{:?}\")",
                    fi.relative_filename
                )
            })?;

            piece_remaining_bytes -= to_read_in_file;

            if piece_remaining_bytes == 0 {
                break;
            }

            absolute_offset = 0;
        }
        Ok(h)
    }

    /// Pieces assumed present without hashing ("skip hash check") are verified the
    /// first time they are read. A piece that fails is marked missing (to be
    /// downloaded again) and the read fails.
    fn verify_if_unverified(&self, piece: ValidPieceIndex) -> anyhow::Result<()> {
        let Some(layout) = self.files.layout() else {
            return Ok(());
        };
        if !layout.is_piece_unverified(piece) {
            return Ok(());
        }
        match self.check_piece(piece) {
            Ok(true) => {
                layout.mark_piece_verified(piece);
                Ok(())
            }
            Ok(false) => {
                layout.on_unverified_piece_failed(piece);
                bail!(
                    "piece {piece} was assumed present (skip hash check) but its hash doesn't match"
                )
            }
            Err(e) => {
                layout.on_unverified_piece_failed(piece);
                Err(e.context(format!(
                    "piece {piece} was assumed present (skip hash check) but can't be read"
                )))
            }
        }
    }

    pub fn read_chunk(
        &self,
        who_sent: PeerHandle,
        chunk_info: &ChunkInfo,
        result_buf: &mut [u8],
    ) -> anyhow::Result<()> {
        if result_buf.len() < chunk_info.size as usize {
            anyhow::bail!("read_chunk(): not enough capacity in the provided buffer")
        }
        self.verify_if_unverified(chunk_info.piece_index)?;
        let mut absolute_offset = self.torrent.lengths().chunk_absolute_offset(chunk_info);
        let mut buf = result_buf;

        for (file_idx, file_info) in self.file_infos.iter().enumerate() {
            let file_len = file_info.len;
            if absolute_offset > file_len {
                absolute_offset -= file_len;
                continue;
            }
            let file_remaining_len = file_len - absolute_offset;
            let to_read_in_file = std::cmp::min(file_remaining_len, buf.len() as u64).try_into()?;

            trace!(
                "piece={}, handle={}, file_idx={}, seeking to {}. To read chunk: {:?}",
                chunk_info.piece_index, who_sent, file_idx, absolute_offset, &chunk_info
            );
            if file_info.attrs.padding {
                buf[..to_read_in_file].fill(0);
            } else {
                self.files
                    .pread_exact(file_idx, absolute_offset, &mut buf[..to_read_in_file])
                    .with_context(|| {
                        format!("error reading {file_idx} bytes, file_id: {to_read_in_file}")
                    })?;
            }

            buf = &mut buf[to_read_in_file..];

            if buf.is_empty() {
                break;
            }

            absolute_offset = 0;
        }

        Ok(())
    }

    pub fn write_chunk(
        &self,
        who_sent: PeerHandle,
        data: &Piece<ByteBuf<'a>>,
        chunk_info: &ChunkInfo,
    ) -> anyhow::Result<()> {
        let mut absolute_offset = self.torrent.lengths().chunk_absolute_offset(chunk_info);
        let mut data = DoubleBufHelper::new(data.data().0, data.data().1);

        for (file_idx, file_info) in self.file_infos.iter().enumerate() {
            let file_len = file_info.len;
            if absolute_offset > file_len {
                absolute_offset -= file_len;
                continue;
            }

            let remaining_len = file_len - absolute_offset;
            let to_write = std::cmp::min(data.len() as u64, remaining_len).try_into()?;

            trace!(
                "piece={}, chunk={:?}, handle={}, begin={}, file={}, writing {} bytes at {}",
                chunk_info.piece_index,
                chunk_info,
                who_sent,
                chunk_info.offset,
                file_idx,
                to_write,
                absolute_offset
            );
            let slices = data.as_ioslices(to_write);
            debug_assert_eq!(slices[0].len() + slices[1].len(), to_write);
            if !file_info.attrs.padding {
                let written = self
                    .files
                    .pwrite_all_vectored(file_idx, absolute_offset, slices)
                    .with_context(|| {
                        format!(
                            "error writing to file {file_idx} (\"{:?}\")",
                            file_info.relative_filename
                        )
                    })?;
                debug_assert_eq!(written, to_write);
            }
            data.advance(to_write);
            if data.is_empty() {
                break;
            }

            absolute_offset = 0;
        }

        Ok(())
    }
}
