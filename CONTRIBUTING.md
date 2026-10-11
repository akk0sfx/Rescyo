# Contributing to Rescyo

Thank you for helping. Rescyo handles people's last copy of their data, so the bar for safety is high. Please read this whole page before opening a pull request.

## Developer Certificate of Origin

Every commit must carry a `Signed-off-by` line certifying the [Developer Certificate of Origin 1.1](https://developercertificate.org/):

```sh
git commit -s
```

By signing off you state that you wrote the change or otherwise have the right to submit it under GPL-3.0-or-later. Note that DCO does not transfer copyright or grant relicensing rights (see [ADR-0001](docs/adr/0001-gpl-everywhere-with-dco.md)).

## Clean-room rule

Work only from public specifications and your own testing: Microsoft's FAT and exFAT specifications, Apple's APFS Reference, the Linux kernel ext4 documentation, the Linux-NTFS documentation, and similar. **Never disassemble, decompile or copy code from DMDE, R-Studio, UFS Explorer or any other proprietary tool.** Reading another tool's *data files* for interoperability (as with DMDE's `devscan.log`) is fine.

## Safety rules

- **Never run tests or tools against a real disk on your machine.** Use image files. Physical-disk backends are tested only on disposable CI runners (`RESCYO_CI_DISPOSABLE=1`). Scripts must not call `hdiutil`, `diskutil`, `mount`, `losetup`, `dd` or `mkfs*` on the host; the test corpus is built inside Docker ([ADR-0002](docs/adr/0002-test-corpus-built-in-disposable-linux.md)).
- **No `unsafe` outside `rescyo-io`'s platform module.** Parser crates use `#![forbid(unsafe_code)]`.
- **Treat every byte from a disk as hostile.** Bound every loop, recursion and allocation by a value you have validated; never index or allocate with an unchecked on-disk number.
- **Nothing writes to a Source.** Writes happen only in Recovery and Session storage and must pass the device-disjointness check ([ADR-0003](docs/adr/0003-write-isolation-fail-closed.md)).

## Workflow

- Branches: `main` holds releases only; `develop` is the integration branch; work happens on `feat/…`, `fix/…`, `docs/…`, `ci/…`, `chore/…` branches opened as pull requests into `develop`.
- Commits follow [Conventional Commits](https://www.conventionalcommits.org/): `feat(fat): follow mirror FAT when primary chain is broken`.
- Test first. A parser change comes with a test on an image or a byte fixture that fails without it.
- Before pushing: `cargo fmt --all`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`.
- Use the vocabulary in [CONTEXT.md](CONTEXT.md). If you need a new term, add it there in the same PR.

## Windows 7 builds

Windows 7 targets (`x86_64-win7-windows-msvc`, `i686-win7-windows-msvc`) are Tier 3 and need a pinned nightly with `-Z build-std`. CI builds them on every pull request. Before adding a dependency, check that it does not call Windows 8+ APIs unconditionally.

## Reporting security issues

See [SECURITY.md](SECURITY.md). Do not open public issues for vulnerabilities.
