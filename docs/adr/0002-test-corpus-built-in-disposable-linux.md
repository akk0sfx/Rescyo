# Test corpus is built by real file-system tools inside disposable Linux only

Test images are created by the reference tools (`mkfs.fat`, `mtools`, `mkfs.exfat`, `mkntfs`, `ntfs-3g`) inside Docker or an ephemeral CI runner, driven by declarative TOML manifests; corruption and verification are done by `cargo xtask`. We do not write our own FAT/NTFS image builders (a shared misreading of the spec would make reader and writer agree on a wrong format), and scripts never call `hdiutil`, `diskutil`, `mount`, `losetup` or `dd` on the developer's host: one wrong `/dev/diskN` would destroy a real disk, which is the one thing a recovery tool must never do.

## Consequences

Physical-disk backends are integration-tested only on disposable CI runners, gated by `RESCYO_CI_DISPOSABLE=1`. Small compressed fixtures are committed so `cargo test` works offline on every OS.
