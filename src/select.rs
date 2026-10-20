use rand::seq::SliceRandom;
use std::path::PathBuf;

/// Take at most `cells` distinct pool entries, optionally shuffled.
/// Uniqueness holds because a pool never contains a path twice.
pub fn select(pool: &[PathBuf], cells: usize, shuffle: bool) -> Vec<PathBuf> {
    let mut chosen = pool.to_vec();
    if shuffle {
        chosen.shuffle(&mut rand::rng());
    }
    chosen.truncate(cells.min(chosen.len()));
    chosen
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn pool(n: usize) -> Vec<PathBuf> {
        (0..n).map(|i| PathBuf::from(format!("/v/{i}.mp4"))).collect()
    }

    #[test]
    fn selection_is_unique_and_bounded() {
        let all = pool(10);
        let chosen = select(&all, 4, true);
        assert_eq!(chosen.len(), 4);
        let unique: HashSet<_> = chosen.iter().collect();
        assert_eq!(unique.len(), 4);

        let chosen = select(&all, 16, true);
        assert_eq!(chosen.len(), 10);
        let unique: HashSet<_> = chosen.iter().collect();
        assert_eq!(unique.len(), 10);
    }

    #[test]
    fn preserves_order_without_shuffle() {
        let all = pool(5);
        assert_eq!(select(&all, 3, false), all[..3].to_vec());
    }

}
