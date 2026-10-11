# Two tiers of file-system support for v1.0

FAT12/16/32, exFAT and NTFS are Tier A: lost-volume search, tree reconstruction, deleted files, Gate 1 byte-for-byte corpus, fuzzing. ext4, HFS+, APFS and Btrfs ship in v1.0 as Tier B, labelled experimental: intact read plus deleted files where metadata still remembers them, tested on generated images and fuzzed. Spreading equal depth across seven file systems would leave none of them trustworthy; the origin case and most removable media are Tier A.
