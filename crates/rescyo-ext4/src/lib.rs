//! ext2/ext3/ext4 recovery engine (experimental, ADR-0005).
//!
//! Reads intact volumes (extent trees, block maps, inline data, hashed directories read block by
//! block) and the deleted entries their metadata still remembers: names left in directory slack,
//! and extents a deleted inode still holds. Every structure is treated as hostile: depths,
//! counts and sizes are bounded.
#![forbid(unsafe_code)]

mod dir;
mod inode;
mod sb;
mod walk;

pub use dir::{DirEntry, parse_block};
pub use sb::{
    Ext4Geometry, Found, SB_LEN, SB_OFFSET, inode_tables, parse_superblock, probe, superblock_group,
};
pub use walk::{WalkError, WalkStats, root_ok, walk};
