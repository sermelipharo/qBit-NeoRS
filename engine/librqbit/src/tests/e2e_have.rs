use std::{
    collections::HashSet,
    time::{Duration, Instant},
};

use super::fake_peer::{
    ClientOpts, FakePeer, Recv, SeederOpts, Wire, peer_id, run, start_client, start_seeder,
};

// A completed piece is announced to every connected peer, also to one whose bitfield already
// claims it. Leaving such a HAVE out looks like a peer that doesn't report its progress, which
// anti-leech tools flag.
#[tokio::test(flavor = "multi_thread")]
async fn test_have_is_announced_even_when_peer_bitfield_claims_the_piece() -> anyhow::Result<()> {
    run(async {
        let seed = start_seeder(SeederOpts::default()).await?;
        let client = start_client(
            &seed,
            ClientOpts {
                listen: true,
                ..Default::default()
            },
        )
        .await?;

        // The peer claims pieces 0..=2 and serves them, so the client completes exactly those.
        let mut peer =
            FakePeer::connect(client.addr(), seed.info_hash, &peer_id(b"-FK0001-")).await?;
        peer.bitfield(&[0b1110_0000]).await?;
        peer.unchoke().await?;

        let mut haves = HashSet::new();
        let deadline = Instant::now() + Duration::from_secs(20);
        while haves.len() < 3 {
            let left = deadline.saturating_duration_since(Instant::now());
            match peer.recv(left).await {
                Recv::Msg(Wire::Request(piece, begin, len)) => {
                    peer.piece(piece, begin, seed.block(piece, begin, len))
                        .await?;
                }
                Recv::Msg(Wire::Have(piece)) => {
                    haves.insert(piece);
                }
                Recv::Msg(_) => {}
                Recv::Timeout | Recv::Closed => break,
            }
        }
        assert_eq!(
            haves,
            HashSet::from([0, 1, 2]),
            "HAVE for every piece completed"
        );
        Ok(())
    })
    .await
}

// A seed is told about our progress too: anti-leech tools run on the uploading side (often a
// seed) and compare what they sent with the progress we report. The peer serves only pieces
// 0..=2, so the client never completes and never drops the connection as seed to seed.
#[tokio::test(flavor = "multi_thread")]
async fn test_have_is_announced_to_a_seed() -> anyhow::Result<()> {
    run(async {
        let seed = start_seeder(SeederOpts::default()).await?;
        let client = start_client(
            &seed,
            ClientOpts {
                listen: true,
                ..Default::default()
            },
        )
        .await?;

        let mut peer =
            FakePeer::connect(client.addr(), seed.info_hash, &peer_id(b"-FK0002-")).await?;
        peer.bitfield(&[0b1111_0000]).await?;
        peer.unchoke().await?;

        let mut haves = HashSet::new();
        let deadline = Instant::now() + Duration::from_secs(20);
        while haves.len() < 3 {
            let left = deadline.saturating_duration_since(Instant::now());
            match peer.recv(left).await {
                Recv::Msg(Wire::Request(piece, begin, len)) if piece < 3 => {
                    peer.piece(piece, begin, seed.block(piece, begin, len))
                        .await?;
                }
                Recv::Msg(Wire::Have(piece)) => {
                    haves.insert(piece);
                }
                Recv::Msg(_) => {}
                Recv::Timeout | Recv::Closed => break,
            }
        }
        assert_eq!(haves, HashSet::from([0, 1, 2]), "HAVE for every piece completed");
        Ok(())
    })
    .await
}
