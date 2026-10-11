use rescyo_testkit::require_corpus;

#[test]
fn every_scenario_is_consistent() {
    for s in require_corpus!() {
        let e = &s.expected;
        let img = std::fs::metadata(s.image_path()).expect("image exists").len();
        assert_eq!(img, e.image_size, "{}", e.scenario);
        assert!(e.partition.offset + e.partition.size <= img, "{}", e.scenario);
        assert!(
            e.files.iter().any(|f| f.fragmented == Some(true)) || !e.fs.starts_with("fat"),
            "{}: FAT scenarios must contain a fragmented file",
            e.scenario
        );
        for c in &e.corruptions {
            let ranges = s.damage_ranges(c).expect("known corruption");
            assert!(ranges.iter().all(|r| r.end <= img), "{}: {c}", e.scenario);
        }
    }
}

#[test]
fn damaged_source_zeroes_only_the_damage() {
    for s in require_corpus!() {
        let Some(c) = s.expected.corruptions.iter().find(|c| *c != "none") else {
            continue;
        };
        let ranges = s.damage_ranges(c).unwrap();
        let src = s.open(c).unwrap();
        let r = &ranges[0];
        let start = r.start - r.start % 512;
        let mut buf = vec![0xAAu8; 4096];
        src.read_at(start, &mut buf).unwrap();
        let lo = usize::try_from(r.start - start).unwrap();
        let hi = usize::try_from(r.end - start).unwrap().min(buf.len());
        assert!(buf[lo..hi].iter().all(|&b| b == 0), "{} {c}", s.expected.scenario);
        let clean = s.open("none").unwrap();
        let mut orig = vec![0u8; 4096];
        clean.read_at(start, &mut orig).unwrap();
        assert_eq!(buf[..lo], orig[..lo]);
        assert_eq!(buf[hi..], orig[hi..]);
    }
}
