//! Speed graph history in two fixed-size ring buffers: one sample per second
//! for the last 30 minutes, and one mean per 36 s for the last 24 hours
//! (20 bytes per sample, ~35 KiB + ~48 KiB in total).

use std::collections::VecDeque;

use crate::model::SpeedSample;

pub const HISTORY_SECS: usize = 30 * 60;
/// Width of a coarse bucket; every graph resolution of 36 s or more is a
/// multiple of it.
pub const COARSE_SECS: i64 = 36;
const COARSE_LEN: usize = 24 * 3600 / COARSE_SECS as usize;

#[derive(Clone, Copy)]
struct Sample {
    ts: i64,
    up: u32,
    down: u32,
    payload_up: u32,
    payload_down: u32,
}

#[derive(Default)]
pub struct SpeedHistory {
    samples: VecDeque<Sample>,
    /// Means of whole coarse buckets; `ts` is the last second of the bucket.
    coarse: VecDeque<Sample>,
    open: Open,
}

/// The coarse bucket being filled.
#[derive(Default)]
struct Open {
    key: i64,
    n: u64,
    sums: [u64; 4],
}

fn to_sample(s: &Sample) -> SpeedSample {
    SpeedSample {
        timestamp_ms: s.ts * 1000,
        upload: s.up as i64,
        download: s.down as i64,
        payload_upload: s.payload_up as i64,
        payload_download: s.payload_down as i64,
        overhead_upload: s.up.saturating_sub(s.payload_up) as i64,
        overhead_download: s.down.saturating_sub(s.payload_down) as i64,
        // ENGINE: DHT and tracker traffic are not counted separately.
        ..Default::default()
    }
}

impl SpeedHistory {
    /// Records the current rates (total incl. overhead, and payload) for
    /// every whole second since the last sample (the session timer may tick
    /// slower than once per second).
    pub fn record(&mut self, now_secs: i64, up: i64, down: i64, payload_up: i64, payload_down: i64) {
        let clamp = |v: i64| v.clamp(0, u32::MAX as i64) as u32;
        let first = match self.samples.back() {
            Some(last) if now_secs <= last.ts => return,
            Some(last) => (last.ts + 1).max(now_secs - HISTORY_SECS as i64 + 1),
            None => now_secs,
        };
        for ts in first..=now_secs {
            if self.samples.len() == HISTORY_SECS {
                self.samples.pop_front();
            }
            let sample = Sample { ts, up: clamp(up), down: clamp(down), payload_up: clamp(payload_up), payload_down: clamp(payload_down) };
            self.samples.push_back(sample);
            self.add_coarse(&sample);
        }
    }

    fn add_coarse(&mut self, s: &Sample) {
        let key = s.ts.div_euclid(COARSE_SECS);
        if self.open.n > 0 && self.open.key != key {
            let n = self.open.n;
            let [up, down, pu, pd] = self.open.sums.map(|v| (v / n) as u32);
            if self.coarse.len() == COARSE_LEN {
                self.coarse.pop_front();
            }
            self.coarse.push_back(Sample { ts: (self.open.key + 1) * COARSE_SECS - 1, up, down, payload_up: pu, payload_down: pd });
            self.open = Open::default();
        }
        self.open.key = key;
        self.open.n += 1;
        for (sum, v) in self.open.sums.iter_mut().zip([s.up, s.down, s.payload_up, s.payload_down]) {
            *sum += u64::from(v);
        }
    }

    /// Graph points of the last `seconds`, averaged into buckets of
    /// `resolution` seconds aligned to the clock, newer than `after_ms`.
    /// Buckets still being filled are left out. Resolutions of 36 s and more
    /// (rounded down to a multiple of 36 s) come from the 24 h tier, the rest
    /// from the 1 s tier. The timestamp of a point is its last second.
    pub fn points(&self, seconds: u32, resolution: u32, after_ms: i64) -> Vec<SpeedSample> {
        let (source, unit) = if resolution >= COARSE_SECS as u32 { (&self.coarse, COARSE_SECS) } else { (&self.samples, 1) };
        let step = (i64::from(resolution) / unit).max(1);
        let width = step * unit;
        let Some(newest) = source.back().map(|s| s.ts) else { return Vec::new() };
        let oldest = newest - i64::from(seconds);
        let newest_key = newest.div_euclid(width);
        let mut out = Vec::new();
        let mut group: Vec<&Sample> = Vec::new();
        let flush = |group: &mut Vec<&Sample>, out: &mut Vec<SpeedSample>| {
            let Some(last) = group.last() else { return };
            let complete = group.len() as i64 == step || last.ts.div_euclid(width) < newest_key;
            if complete && last.ts * 1000 > after_ms {
                let n = group.len() as u64;
                let mean = |f: fn(&Sample) -> u32| (group.iter().map(|s| u64::from(f(s))).sum::<u64>() / n) as u32;
                out.push(to_sample(&Sample {
                    ts: last.ts,
                    up: mean(|s| s.up),
                    down: mean(|s| s.down),
                    payload_up: mean(|s| s.payload_up),
                    payload_down: mean(|s| s.payload_down),
                }));
            }
            group.clear();
        };
        for s in source.iter().rev().take_while(|s| s.ts > oldest).collect::<Vec<_>>().into_iter().rev() {
            if group.last().is_some_and(|l| l.ts.div_euclid(width) != s.ts.div_euclid(width)) {
                flush(&mut group, &mut out);
            }
            group.push(s);
        }
        flush(&mut group, &mut out);
        out
    }

    /// Up to the last `seconds` samples, oldest first.
    pub fn last(&self, seconds: u32) -> Vec<SpeedSample> {
        let n = (seconds as usize).min(self.samples.len());
        self.samples
            .iter()
            .skip(self.samples.len() - n)
            .map(to_sample)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fills_gaps_and_caps() {
        let mut h = SpeedHistory::default();
        h.record(100, 1, 2, 1, 1);
        h.record(102, 3, 4, 2, 3);
        h.record(102, 9, 9, 9, 9);
        let s = h.last(10);
        assert_eq!(s.len(), 3);
        assert_eq!(s[1].timestamp_ms, 101_000);
        assert_eq!(s[1].upload, 3);
        assert_eq!(s[1].overhead_upload, 1);
        h.record(100_000, 5, 5, 5, 5);
        assert_eq!(h.last(u32::MAX).len(), HISTORY_SECS);
        assert_eq!(h.last(1)[0].timestamp_ms, 100_000_000);
    }

    #[test]
    fn points_are_aligned_complete_and_incremental() {
        let mut h = SpeedHistory::default();
        // Seconds 0..=99, down = ts.
        for ts in 0..100 {
            h.record(ts, 0, ts, 0, ts);
        }
        // 10 s buckets 0-9 ... 90-99, all full.
        let p = h.points(100, 10, 0);
        assert_eq!(p.len(), 10);
        assert_eq!(p[0].timestamp_ms, 9_000);
        assert_eq!(p[0].download, 4); // mean of 0..=9
        assert_eq!(p[9].timestamp_ms, 99_000);
        // Only newer than the last known point; a bucket in progress waits.
        h.record(100, 0, 100, 0, 100);
        assert!(h.points(100, 10, 99_000).is_empty());
        for ts in 101..110 {
            h.record(ts, 0, ts, 0, ts);
        }
        let q = h.points(100, 10, 99_000);
        assert_eq!(q.len(), 1);
        assert_eq!(q[0].timestamp_ms, 109_000);
        // Resolution 1 returns every second.
        assert_eq!(h.points(30, 1, 0).len(), 30);
    }

    #[test]
    fn coarse_tier_keeps_a_day_in_fixed_memory() {
        let mut h = SpeedHistory::default();
        let day = 24 * 3600;
        for ts in 0..(day + 7200) {
            h.record(ts, 0, 100, 0, 100);
        }
        assert_eq!(h.samples.len(), HISTORY_SECS);
        assert_eq!(h.coarse.len(), COARSE_LEN);
        // 24 h at 144 s resolution is 600 points, all with the mean rate.
        let p = h.points(day as u32, 144, 0);
        assert!((599..=600).contains(&p.len()), "{}", p.len());
        assert!(p.iter().all(|s| s.download == 100));
        // Short periods still come from the 1 s tier.
        assert_eq!(h.points(60, 1, 0).len(), 60);
    }
}
