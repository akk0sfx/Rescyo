# GPL-3.0-or-later for every crate, contributions under DCO

All crates, including the low-level parsers (`rescyo-io`, `rescyo-fat`, `rescyo-ntfs`, `rescyo-dmde`), are GPL-3.0-or-later, and contributions are accepted under the Developer Certificate of Origin rather than a CLA. The parsers for damaged file systems are the most valuable part of the project and the part a closed competitor would want to lift, so we do not dual-license them as MIT/Apache-2.0 even though that is the Rust-ecosystem default.

## Consequences

DCO does not grant relicensing rights. Once external contributions land, moving any crate to a permissive license requires consent from every contributor to it. If permissive parser crates are ever wanted, decide while the code is still single-author.
