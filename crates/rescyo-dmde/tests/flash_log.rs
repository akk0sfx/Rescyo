//! The sanitized log of the flash drive that started Rescyo.
#![allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test")]

#[test]
fn reconstructed_flash_log_parses_with_its_known_facts() {
    let s = rescyo_dmde::parse(include_bytes!("fixtures/devscan-flash.log")).unwrap();
    assert_eq!((s.sector_size, s.sectors), (512, 61_440_000));
    assert_eq!(s.device_size(), 0x7_5300_0000);
    assert_eq!(s.fat_volumes.len(), 12);
    let best = s.best_volume().unwrap();
    assert_eq!(
        (best.index, best.cluster_size, best.data_offset, best.folders),
        (1, 0x4000, 0x100_0000, 476)
    );
    assert_eq!(s.dir_starts.len(), 478);
    assert_eq!(
        s.dir_frags,
        vec![0x100_1000, 0x1c4_4000, 0x1_0613_c000, 0x1_6dae_c400]
    );
    assert!(s.warnings.is_empty(), "{:?}", s.warnings);
    // Two dirclus entries do not lie on the cluster grid and are dropped.
    let (seeds, off_grid) = s.seeds_for(best);
    assert_eq!(off_grid, 2);
    assert_eq!(seeds.iter().filter(|x| x.parent.is_some()).count(), 476);
    assert!(seeds.iter().any(|x| x.cluster == 788 && x.parent == Some(0)));
    // Folders whose parents are not in the list: their first cluster was damaged.
    let known: std::collections::HashSet<u64> = s.dir_starts.iter().map(|d| d.cluster).collect();
    let orphans: std::collections::BTreeSet<u64> = s
        .dir_starts
        .iter()
        .map(|d| d.parent)
        .filter(|p| *p != 0 && !known.contains(p))
        .collect();
    assert_eq!(
        orphans.into_iter().collect::<Vec<_>>(),
        vec![6, 423, 427, 9708, 22289, 23851]
    );
}
