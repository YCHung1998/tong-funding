//! Frame-interval statistics for the benchmark page. Nearest-rank percentiles:
//! the p-th percentile of n samples is the ceil(p/100 * n)-th smallest.

#[derive(Debug, Clone, PartialEq)]
pub struct FrameStats {
    pub frames: usize,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub max_ms: f64,
}

fn nearest_rank(sorted: &[f64], p: f64) -> f64 {
    let rank = (p / 100.0 * sorted.len() as f64).ceil() as usize;
    sorted[rank.clamp(1, sorted.len()) - 1]
}

pub fn summarize(intervals_ms: &[f64]) -> Option<FrameStats> {
    if intervals_ms.is_empty() {
        return None;
    }
    let mut sorted = intervals_ms.to_vec();
    sorted.sort_by(|a, b| a.total_cmp(b));
    Some(FrameStats {
        frames: sorted.len(),
        p50_ms: nearest_rank(&sorted, 50.0),
        p95_ms: nearest_rank(&sorted, 95.0),
        max_ms: *sorted.last().unwrap(),
    })
}

/// Frames longer than this are "the window was hidden or paused" (macOS stops redrawing
/// occluded windows), not dropped frames; they are reported separately and excluded
/// from the percentile statistics.
pub const PAUSE_THRESHOLD_MS: f64 = 250.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DropReport {
    /// Intervals longer than 1.5 vsync periods (but not pauses): a missed vsync.
    pub dropped: usize,
    /// Intervals longer than `PAUSE_THRESHOLD_MS`.
    pub paused: usize,
}

pub fn drops(intervals_ms: &[f64], vsync_ms: f64) -> DropReport {
    let paused = intervals_ms.iter().filter(|&&x| x > PAUSE_THRESHOLD_MS).count();
    let dropped = intervals_ms
        .iter()
        .filter(|&&x| x > 1.5 * vsync_ms && x <= PAUSE_THRESHOLD_MS)
        .count();
    DropReport { dropped, paused }
}

/// Splits raw intervals into (running frames, report). Percentiles should be computed
/// from the running frames only.
pub fn split_paused(intervals_ms: &[f64]) -> Vec<f64> {
    intervals_ms.iter().copied().filter(|&x| x <= PAUSE_THRESHOLD_MS).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_input_has_no_stats() {
        assert_eq!(summarize(&[]), None);
    }

    #[test]
    fn nearest_rank_percentiles_on_1_to_100() {
        let data: Vec<f64> = (1..=100).map(|n| n as f64).collect();
        let s = summarize(&data).unwrap();
        assert_eq!(s.frames, 100);
        assert_eq!(s.p50_ms, 50.0); // ceil(0.50 * 100) = 50th value
        assert_eq!(s.p95_ms, 95.0); // ceil(0.95 * 100) = 95th value
        assert_eq!(s.max_ms, 100.0);
    }

    #[test]
    fn input_order_does_not_matter() {
        let s = summarize(&[30.0, 10.0, 20.0, 40.0]).unwrap();
        assert_eq!(s.p50_ms, 20.0); // ceil(0.5*4)=2nd of sorted [10,20,30,40]
        assert_eq!(s.p95_ms, 40.0); // ceil(0.95*4)=4th
        assert_eq!(s.max_ms, 40.0);
    }

    #[test]
    fn single_sample_is_every_percentile() {
        let s = summarize(&[16.7]).unwrap();
        assert_eq!((s.p50_ms, s.p95_ms, s.max_ms), (16.7, 16.7, 16.7));
    }

    #[test]
    fn drops_counts_missed_vsyncs_and_separates_pauses() {
        // vsync 16.7 ms -> a drop is > 25.05 ms; a pause is > 250 ms.
        let r = drops(&[16.7, 16.7, 25.0, 25.1, 33.4, 300.0], 16.7);
        assert_eq!(r, DropReport { dropped: 2, paused: 1 });
    }

    #[test]
    fn no_drops_on_a_steady_run() {
        assert_eq!(drops(&[16.6, 16.8, 17.5, 18.0], 16.67), DropReport { dropped: 0, paused: 0 });
    }

    #[test]
    fn split_paused_removes_only_pauses() {
        assert_eq!(split_paused(&[16.7, 300.0, 33.4, 251.0, 250.0]), vec![16.7, 33.4, 250.0]);
    }
}
