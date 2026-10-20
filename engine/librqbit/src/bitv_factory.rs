use crate::{api::TorrentIdOrHash, bitv::BitV, type_aliases::BF};

#[async_trait::async_trait]
pub trait BitVFactory: Send + Sync {
    async fn load(&self, id: TorrentIdOrHash) -> anyhow::Result<Option<Box<dyn BitV>>>;
    async fn clear(&self, id: TorrentIdOrHash) -> anyhow::Result<()>;
    async fn store_initial_check(
        &self,
        id: TorrentIdOrHash,
        b: BF,
    ) -> anyhow::Result<Box<dyn BitV>>;

    /// File sizes and mtimes recorded with the bitfield (opaque bytes, see
    /// `torrent_state::resume_stats`). Stores that don't keep them return `None`,
    /// and the bitfield is then validated by sampling pieces.
    async fn load_resume_stats(&self, _id: TorrentIdOrHash) -> anyhow::Result<Option<Vec<u8>>> {
        Ok(None)
    }

    /// Replaces the recorded file stats; `None` removes them. `clear` removes
    /// them too.
    async fn store_resume_stats(
        &self,
        _id: TorrentIdOrHash,
        _data: Option<Vec<u8>>,
    ) -> anyhow::Result<()> {
        Ok(())
    }
}

pub struct NonPersistentBitVFactory {}

#[async_trait::async_trait]
impl BitVFactory for NonPersistentBitVFactory {
    async fn load(&self, _: TorrentIdOrHash) -> anyhow::Result<Option<Box<dyn BitV>>> {
        Ok(None)
    }

    async fn clear(&self, _id: TorrentIdOrHash) -> anyhow::Result<()> {
        Ok(())
    }

    async fn store_initial_check(
        &self,
        _id: TorrentIdOrHash,
        b: BF,
    ) -> anyhow::Result<Box<dyn BitV>> {
        Ok(Box::new(b))
    }
}
