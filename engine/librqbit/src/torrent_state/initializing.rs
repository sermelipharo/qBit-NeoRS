use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Instant,
};

use anyhow::Context;

use rand::{RngExt, seq::IteratorRandom};
use size_format::SizeFormatterBinary as SF;
use tracing::{debug, info, trace, warn};

use crate::{
    api::TorrentIdOrHash,
    bitv::BitV,
    bitv_factory::BitVFactory,
    chunk_tracker::{ChunkTracker, compute_selected_pieces},
    file_ops::FileOps,
    storage::StorageError,
    type_aliases::{BF, FileStorage},
};

const MAX_FASTRESUME_CHECKS: usize = 64;

use super::{
    ManagedTorrentShared, TorrentMetadata, missing_layers,
    paused::TorrentStatePaused,
    resume_stats::{self, ResumeStats},
};

pub struct TorrentStateInitializing {
    pub(crate) files: FileStorage,
    pub(crate) shared: Arc<ManagedTorrentShared>,
    pub(crate) metadata: Arc<TorrentMetadata>,
    pub(crate) only_files: Option<Vec<usize>>,
    pub(crate) checked_bytes: AtomicU64,
    pause_requested: AtomicBool,
    pause_notify: tokio::sync::Notify,
    check_running: AtomicBool,
    previously_errored: bool,
}

impl TorrentStateInitializing {
    pub fn new(
        shared: Arc<ManagedTorrentShared>,
        metadata: Arc<TorrentMetadata>,
        only_files: Option<Vec<usize>>,
        files: FileStorage,
        previously_errored: bool,
    ) -> Self {
        Self {
            shared,
            metadata,
            only_files,
            files,
            checked_bytes: AtomicU64::new(0),
            pause_requested: AtomicBool::new(false),
            pause_notify: tokio::sync::Notify::new(),
            check_running: AtomicBool::new(false),
            previously_errored,
        }
    }

    pub fn get_checked_bytes(&self) -> u64 {
        self.checked_bytes
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    pub(crate) fn request_pause(&self) {
        self.pause_requested.store(true, Ordering::Relaxed);
        self.pause_notify.notify_waiters();
    }

    pub(crate) fn clear_pause_request(&self) {
        self.pause_requested.store(false, Ordering::Relaxed);
    }

    pub(crate) fn is_pause_requested(&self) -> bool {
        self.pause_requested.load(Ordering::Relaxed)
    }

    pub(crate) fn try_start_check(&self) -> bool {
        self.check_running
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    pub(crate) fn finish_check(&self) {
        self.check_running.store(false, Ordering::Release);
    }

    /// Resolves once a pause is requested.
    pub(crate) async fn wait_pause_requested(&self) {
        loop {
            let notified = self.pause_notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.is_pause_requested() {
                return;
            }
            notified.await;
        }
    }

    pub(crate) fn is_check_running(&self) -> bool {
        self.check_running.load(Ordering::Acquire)
    }

    async fn validate_fastresume(
        &self,
        bitv_factory: &dyn BitVFactory,
        have_pieces: Option<Box<dyn BitV>>,
    ) -> Option<Box<dyn BitV>> {
        let hp = have_pieces?;
        let actual = hp.as_bytes().len();
        let expected = self.metadata.lengths().piece_bitfield_bytes();
        if actual != expected {
            warn!(
                actual,
                expected,
                "the bitfield loaded isn't of correct length, ignoring it, will do full check"
            );
            return None;
        }

        let is_broken = self
            .shared
            .spawner
            .block_in_place_with_semaphore(|| {
                let fo = crate::file_ops::FileOps::new(
                    &self.metadata.info,
                    &self.files,
                    &self.metadata.file_infos,
                );

                let mut to_validate = BF::from_boxed_slice(
                    vec![0u8; self.metadata.lengths().piece_bitfield_bytes()].into_boxed_slice(),
                );
                let mut queue = hp.as_slice().to_owned();

                // Validate at least one piece from each file, if we claim we have it.
                for fi in self.metadata.file_infos.iter() {
                    let prange = fi.piece_range_usize();
                    let offset = prange.start;
                    for piece_id in hp
                        .as_slice()
                        .get(fi.piece_range_usize())
                        .into_iter()
                        .flat_map(|s| s.iter_ones())
                        .map(|pid| pid + offset)
                        .take(1)
                    {
                        to_validate.set(piece_id, true);
                        queue.set(piece_id, false);
                    }
                }

                // For all the remaining pieces we claim we have, validate them with decreasing probability.
                let queue = queue
                    .iter_ones()
                    .sample(&mut rand::rng(), MAX_FASTRESUME_CHECKS);

                for (tmp_id, piece_id) in queue.into_iter().enumerate() {
                    let denom: u32 = (tmp_id + 1).min(50).try_into().unwrap();
                    if rand::rng().random_ratio(1, denom) {
                        to_validate.set(piece_id, true);
                    }
                }

                let to_validate_count = to_validate.count_ones();
                for (id, piece_id) in to_validate
                    .iter_ones()
                    .filter_map(|id| {
                        self.metadata
                            .lengths()
                            .validate_piece_index(id.try_into().ok()?)
                    })
                    .enumerate()
                {
                    // `check_piece` reports a hash mismatch as `Ok(false)`; both a
                    // mismatch and a read error make the fastresume data untrustworthy.
                    match fo.check_piece(piece_id) {
                        Ok(true) => {}
                        Ok(false) => return true,
                        Err(e) => {
                            warn!(id = ?self.shared.id, ?piece_id, "error checking piece: {e:#}");
                            return true;
                        }
                    }

                    #[allow(clippy::cast_possible_truncation)]
                    let progress = (self.metadata.lengths().total_length() as f64
                        / to_validate_count as f64
                        * (id + 1) as f64) as u64;
                    let progress = progress.min(self.metadata.lengths().total_length());
                    self.checked_bytes.store(progress, Ordering::Relaxed);
                }

                false
            })
            .await;

        if is_broken {
            warn!(
                id = ?self.shared.id,
                info_hash = ?self.shared.info_hash,
                "data corrupted, ignoring fastresume data"
            );
            if let Err(e) = bitv_factory.clear(self.shared.id.into()).await {
                warn!(id=?self.shared.id, info_hash = ?self.shared.info_hash, "error clearing bitfield: {e:#}");
            }
            self.checked_bytes.store(0, Ordering::Relaxed);
            return None;
        }

        Some(hp)
    }

    /// The file sizes and mtimes recorded with the loaded bitfield, if there are
    /// usable ones.
    async fn load_resume_stats(
        &self,
        bitv_factory: &dyn BitVFactory,
        have_pieces: Option<&dyn BitV>,
    ) -> Option<ResumeStats> {
        let hp = have_pieces?;
        if hp.as_bytes().len() != self.metadata.lengths().piece_bitfield_bytes()
            || !self.shared.layout.is_disk_backed()
        {
            return None;
        }
        let data = match bitv_factory
            .load_resume_stats(self.shared.info_hash.into())
            .await
        {
            Ok(d) => d?,
            Err(e) => {
                warn!(id = ?self.shared.id, "error loading file stats: {e:#}");
                return None;
            }
        };
        match ResumeStats::decode(&data) {
            Some(s) if s.fits(self.metadata.lengths(), &self.metadata.file_infos) => Some(s),
            _ => {
                warn!(id = ?self.shared.id, "ignoring invalid file stats");
                None
            }
        }
    }

    /// Validates the loaded bitfield with the recorded file sizes and mtimes:
    /// pieces of unchanged files are kept without reading, pieces touching a
    /// changed file are hashed, pieces touching a missing file are dropped. A
    /// corrected bitfield is stored. Also returns whether nothing changed since
    /// the record (so it needn't be written again).
    async fn validate_with_resume_stats(
        &self,
        bitv_factory: &dyn BitVFactory,
        hp: Box<dyn BitV>,
        saved: ResumeStats,
    ) -> anyhow::Result<(Box<dyn BitV>, bool)> {
        let layout = &self.shared.layout;
        let file_infos = &self.metadata.file_infos;
        let (have, files_unchanged, reads) = self
            .shared
            .spawner
            .block_in_place_with_semaphore(|| {
                let current =
                    resume_stats::current_stats(file_infos, |idx| layout.full_actual_path(idx));
                let plan = resume_stats::plan(file_infos, hp.as_slice(), &saved, &current);
                let reads = plan.verify.count_ones();
                let fo = FileOps::new(&self.metadata.info, &self.files, file_infos);
                let have = resume_stats::run_plan(
                    plan,
                    self.metadata.lengths(),
                    &self.checked_bytes,
                    &self.pause_requested,
                    |piece| match fo.check_piece(piece) {
                        Ok(ok) => ok,
                        Err(e) => {
                            debug!(?piece, "error checking piece: {e:#}");
                            false
                        }
                    },
                )?;
                anyhow::Ok((have, current == saved.files, reads))
            })
            .await?;
        if have.as_raw_slice() == hp.as_bytes() {
            debug!(id = ?self.shared.id, reads, "fastresume data validated with file stats");
            return Ok((hp, files_unchanged));
        }
        let dropped = hp.as_slice().count_ones() - have.count_ones();
        info!(
            id = ?self.shared.id,
            reads,
            dropped,
            "fastresume: checked pieces of changed files"
        );
        let hp = bitv_factory
            .store_initial_check(self.shared.info_hash.into(), have)
            .await
            .context("error storing checked bitfield")?;
        Ok((hp, false))
    }

    /// Records the files' sizes and mtimes with the bitfield (see
    /// `resume_stats`), or removes the record when it can't be kept.
    async fn save_resume_stats(
        &self,
        bitv_factory: &dyn BitVFactory,
        chunk_tracker: &ChunkTracker,
    ) {
        let stats = self
            .shared
            .spawner
            .block_in_place_with_semaphore(|| {
                resume_stats::record_for(&self.shared.layout, &self.metadata.file_infos, || {
                    Some(resume_stats::copy_bits(chunk_tracker.get_have_pieces()))
                })
            })
            .await;
        if let Err(e) = bitv_factory
            .store_resume_stats(self.shared.info_hash.into(), stats.map(|s| s.encode()))
            .await
        {
            warn!(id = ?self.shared.id, "error storing file stats: {e:#}");
        }
    }

    pub async fn check(&self) -> anyhow::Result<TorrentStatePaused> {
        // The check may run again after being paused.
        self.checked_bytes.store(0, Ordering::Relaxed);
        let id: TorrentIdOrHash = self.shared.info_hash.into();
        let bitv_factory = self
            .shared
            .session
            .upgrade()
            .context("session is dead")?
            .bitv_factory
            .clone();
        let have_pieces = if self.previously_errored {
            if let Err(e) = bitv_factory.clear(id).await {
                warn!(id=?self.shared.id, info_hash = ?self.shared.info_hash, error=?e, "error clearing bitfield");
            }
            None
        } else {
            bitv_factory
                .load(id)
                .await
                .context("error loading have_pieces")?
        };

        let layout = &self.shared.layout;
        let expected_bitfield_len = self.metadata.lengths().piece_bitfield_bytes();

        // Missing files: fastresume says we have data, but the files are gone (moved,
        // deleted, drive not mounted). Don't recheck (that would throw the progress
        // away), report it instead.
        if let Some(hp) = have_pieces.as_ref()
            && hp.as_bytes().len() == expected_bitfield_len
            && hp.as_slice().any()
        {
            let missing = self.shared.spawner.block_in_place(|| {
                layout.missing_files(|idx| layout.file_has_any_piece(idx, hp.as_slice()))
            });
            if !missing.is_empty() {
                warn!(
                    id = ?self.shared.id,
                    info_hash = ?self.shared.info_hash,
                    count = missing.len(),
                    first = ?missing.first(),
                    "files with downloaded data are missing"
                );
                return Err(StorageError::MissingFiles { files: missing }.into());
            }
        }

        // Skip hash check: assume we have everything, verify pieces when first read.
        let skip_hash_check = have_pieces.is_none()
            && !self.previously_errored
            && self.shared.options.skip_hash_check;
        let mut unverified = None;
        // A pure v2 torrent missing piece layers: pieces of files without
        // one can't be verified, so they never count as had; data claimed
        // for them is checked once the layer arrives.
        let unknown = missing_layers::unknown_pieces(&self.metadata.info);
        let mut unverified_v2 = None;
        let mut layers_found = false;
        let mut have_pieces = have_pieces;
        let mut resume_stats_current = false;
        if let (Some(unknown), Some(hp)) = (unknown.as_ref(), have_pieces.as_mut())
            && hp.as_bytes().len() == expected_bitfield_len
        {
            let mut taken = missing_layers::take_unknown(hp.as_slice_mut(), unknown);
            // Whole files said to be there: hashed up to their root, their
            // layer is found and they are had after all (libtorrent's check
            // does the same).
            let found = self
                .shared
                .spawner
                .block_in_place_with_semaphore(|| {
                    FileOps::new(&self.metadata.info, &self.files, &self.metadata.file_infos)
                        .find_layers(&taken)
                })
                .await;
            for r in found {
                if let Some(s) = taken.get_mut(r.clone()) {
                    s.fill(false);
                }
                if let Some(s) = hp.as_slice_mut().get_mut(r) {
                    s.fill(true);
                }
                layers_found = true;
            }
            unverified_v2 = Some(taken);
        }

        let have_pieces = if skip_hash_check {
            info!(torrent=?self.shared.id, "skipping hash check, assuming all data is present");
            let mut all = BF::from_boxed_slice(vec![0u8; expected_bitfield_len].into_boxed_slice());
            all[..self.metadata.lengths().total_pieces() as usize].fill(true);
            if let Some(unknown) = unknown.as_ref() {
                unverified_v2 = Some(missing_layers::take_unknown(&mut all, unknown));
            }
            unverified = Some(all.clone());
            self.checked_bytes
                .store(self.metadata.lengths().total_length(), Ordering::Relaxed);
            Some(
                bitv_factory
                    .store_initial_check(id, all)
                    .await
                    .context("error storing initial check bitfield")?,
            )
        } else {
            match self
                .load_resume_stats(&*bitv_factory, have_pieces.as_deref())
                .await
            {
                Some(saved) => {
                    let hp = have_pieces.context("bug: file stats without a bitfield")?;
                    let (hp, unchanged) = self
                        .validate_with_resume_stats(&*bitv_factory, hp, saved)
                        .await?;
                    resume_stats_current = unchanged;
                    Some(hp)
                }
                // No record (older data, or data imported from elsewhere): sample.
                None => self.validate_fastresume(&*bitv_factory, have_pieces).await,
            }
        };

        let have_pieces = match have_pieces {
            Some(h) => h,
            None => {
                info!("Doing initial checksum validation, this might take a while...");
                let check = self
                    .shared
                    .spawner
                    .block_in_place_with_semaphore(|| {
                        FileOps::new(&self.metadata.info, &self.files, &self.metadata.file_infos)
                            .initial_check(&self.checked_bytes, &self.pause_requested)
                    })
                    .await?;
                unverified_v2 = check.unverified_v2;
                layers_found |= check.derived_layers > 0;
                bitv_factory
                    .store_initial_check(id, check.have)
                    .await
                    .context("error storing initial check bitfield")?
            }
        };
        *self.shared.v2_unverified.lock() = unverified_v2.filter(|u| u.any());
        if layers_found {
            // Handed out for the owner to save (`take_fetched_piece_layers`).
            self.shared
                .v2_layers_unsaved
                .store(true, Ordering::Release);
        }

        let selected_pieces = compute_selected_pieces(
            self.metadata.lengths(),
            |idx| {
                self.only_files
                    .as_ref()
                    .map(|o| o.contains(&idx))
                    .unwrap_or(true)
            },
            &self.metadata.file_infos,
        );

        let chunk_tracker = ChunkTracker::new(
            have_pieces.into_dyn(),
            selected_pieces,
            *self.metadata.lengths(),
            &self.metadata.file_infos,
        )
        .context("error creating chunk tracker")?;

        // On-disk names: completed files lose the incomplete suffix, unwanted files go
        // to the unwanted folder (when those options are on).
        layout.set_completion(chunk_tracker.get_have_pieces().as_slice());
        layout.set_wanted(|idx| {
            self.only_files
                .as_ref()
                .map(|o| o.contains(&idx))
                .unwrap_or(true)
        });
        self.shared
            .spawner
            .block_in_place_with_semaphore(|| layout.reconcile_all())
            .await;
        layout.set_unverified(unverified);

        let hns = chunk_tracker.get_hns();

        info!(
            torrent=?self.shared.id,
            "Initial check results: have {}, needed {}, total selected {}",
            SF::new(hns.have_bytes),
            SF::new(hns.needed_bytes),
            SF::new(hns.selected_bytes)
        );

        // Ensure file lengths are correct, and reopen read-only.
        self.shared
            .spawner
            .block_in_place_with_semaphore(|| {
                for (idx, fi) in self.metadata.file_infos.iter().enumerate() {
                    if self
                        .only_files
                        .as_ref()
                        .map(|v| v.contains(&idx))
                        .unwrap_or(true)
                    {
                        let now = Instant::now();
                        if fi.attrs.padding {
                            continue;
                        }
                        if let Err(err) = self.files.ensure_file_length(idx, fi.len) {
                            warn!(
                                id=?self.shared.id, info_hash = ?self.shared.info_hash,
                                "Error setting length for file {:?} to {}: {:#?}",
                                fi.relative_filename, fi.len, err
                            );
                        } else {
                            trace!(
                                "Set length for file {:?} to {} in {:?}",
                                fi.relative_filename,
                                SF::new(fi.len),
                                now.elapsed()
                            );
                        }
                    }
                }
                Ok::<_, anyhow::Error>(())
            })
            .await?;

        // After the lengths are settled, so the record sees the final sizes.
        if !resume_stats_current {
            self.save_resume_stats(&*bitv_factory, &chunk_tracker).await;
        }

        let paused = TorrentStatePaused {
            shared: self.shared.clone(),
            metadata: self.metadata.clone(),
            files: self.files.take()?,
            chunk_tracker,
            streams: Arc::new(Default::default()),
        };
        Ok(paused)
    }
}
