//! Physical-disk backend test. Runs ONLY on disposable CI runners (ADR-0002): the workflow
//! attaches an image read-only as a block device and passes both paths in the environment.
//! On a developer machine the test returns immediately.

use std::sync::Arc;

use rescyo_io::{BlockSource, GuardedReader, ImageSource, PhysicalDisk, ReadPolicy, list_disks};

#[test]
fn physical_disk_reads_match_the_backing_image() {
    if std::env::var("RESCYO_CI_DISPOSABLE").as_deref() != Ok("1") {
        eprintln!("skipped: not a disposable CI runner");
        return;
    }
    let device = std::env::var("RESCYO_CI_DEVICE").expect("RESCYO_CI_DEVICE");
    let image = std::env::var("RESCYO_CI_IMAGE").expect("RESCYO_CI_IMAGE");

    let disk = PhysicalDisk::open(&device).expect("open device read-only");
    let img = ImageSource::open(image.as_ref(), 512).expect("open image");
    let g = disk.geometry();
    assert!(g.size >= img.geometry().size.min(8 << 20), "device smaller than expected: {g:?}");

    let disk = GuardedReader::new(Arc::new(disk), ReadPolicy::default());
    let img = GuardedReader::new(Arc::new(img), ReadPolicy::default());
    let len = img.geometry().size.min(g.size);
    for offset in [0, 512, 4096 + 17, len / 2 + 3, len - 4096] {
        let mut a = vec![0; 4096];
        let mut b = vec![0; 4096];
        let n = (len - offset).min(4096) as usize;
        disk.read(offset, &mut a[..n]).expect("device read");
        img.read(offset, &mut b[..n]).expect("image read");
        assert_eq!(a[..n], b[..n], "mismatch at offset {offset}");
    }

    let disks = list_disks().expect("list disks");
    eprintln!("listed {} disks", disks.len());
}
