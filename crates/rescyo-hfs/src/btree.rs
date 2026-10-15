//! HFS+ B-tree files (catalog, extents overflow) read as a sequence of nodes.

use rescyo_io::{GuardError, GuardedReader};

use crate::{Fork, HfsGeometry, be16, be32};

/// Kinds of B-tree nodes.
pub const LEAF: i8 = -1;

/// Most nodes read from one tree (a 16 GiB catalog of 4 KiB nodes).
const MAX_NODES: u32 = 1 << 22;

/// A B-tree file: its fork mapped to disk, read node by node.
pub struct Tree<'a> {
    r: &'a GuardedReader,
    g: &'a HfsGeometry,
    /// (first node, absolute offset, node count) runs.
    runs: Vec<(u32, u64, u32)>,
    pub node_size: u32,
    pub first_leaf: u32,
    pub total_nodes: u32,
}

impl<'a> Tree<'a> {
    /// Opens the tree stored in `fork` (plus any overflow extents).
    pub fn open(
        r: &'a GuardedReader,
        g: &'a HfsGeometry,
        fork: &Fork,
        extra: &[(u32, u32)],
    ) -> Result<Option<Self>, GuardError> {
        let bs = u64::from(g.block_size);
        let mut runs = Vec::new();
        let mut blocks_so_far = 0u64;
        // Node size is not known yet: map blocks first, convert after reading the header.
        let mut block_runs = Vec::new();
        for &(start, count) in fork.extents.iter().chain(extra) {
            let Some(off) = g.block_offset(start) else {
                return Ok(None);
            };
            block_runs.push((blocks_so_far, off, u64::from(count)));
            blocks_so_far += u64::from(count);
        }
        let Some(&(_, first, _)) = block_runs.first() else {
            return Ok(None);
        };
        let mut head = vec![0u8; 512];
        if !crate::read_clean(r, first, &mut head)? {
            return Ok(None);
        }
        // Header node: descriptor (14 bytes), then the header record.
        let (Some(first_leaf), Some(node_size), Some(total_nodes)) =
            (be32(&head, 14 + 10), be16(&head, 14 + 18), be32(&head, 14 + 22))
        else {
            return Ok(None);
        };
        let node_size = u32::from(node_size);
        if !node_size.is_power_of_two() || !(512..=32768).contains(&node_size) {
            return Ok(None);
        }
        let ns = u64::from(node_size);
        for (b0, off, count) in block_runs {
            let first_node = b0 * bs / ns;
            let nodes = count * bs / ns;
            runs.push((
                u32::try_from(first_node).unwrap_or(u32::MAX),
                off,
                u32::try_from(nodes).unwrap_or(u32::MAX),
            ));
        }
        Ok(Some(Self {
            r,
            g,
            runs,
            node_size,
            first_leaf,
            total_nodes: total_nodes.min(MAX_NODES),
        }))
    }

    fn offset(&self, n: u32) -> Option<u64> {
        self.runs
            .iter()
            .find(|r| n >= r.0 && n - r.0 < r.2)
            .map(|r| r.1 + u64::from(n - r.0) * u64::from(self.node_size))
    }

    /// Reads node `n`; `None` when it is outside the tree or unreadable.
    pub fn node(&self, n: u32) -> Result<Option<Vec<u8>>, GuardError> {
        let Some(off) = self.offset(n) else {
            return Ok(None);
        };
        if off + u64::from(self.node_size) > self.g.volume_offset + self.g.size() {
            return Ok(None);
        }
        let mut b = vec![0u8; self.node_size as usize];
        Ok(crate::read_clean(self.r, off, &mut b)?.then_some(b))
    }
}

/// A node's kind, next node and record slices; the last item is the free space.
pub struct Node<'b> {
    pub kind: i8,
    pub next: u32,
    pub records: Vec<&'b [u8]>,
    pub free: &'b [u8],
}

pub fn parse(b: &[u8]) -> Option<Node<'_>> {
    let next = be32(b, 0)?;
    let kind = i8::from_be_bytes([*b.get(8)?]);
    let count = usize::from(be16(b, 10)?);
    let size = b.len();
    if count > size / 2 {
        return None;
    }
    // Offsets are stored backwards from the end of the node: count + 1 entries.
    let off = |i: usize| -> Option<usize> { be16(b, size.checked_sub(2 * (i + 1))?).map(usize::from) };
    let table_start = size.checked_sub(2 * (count + 1))?;
    let mut records = Vec::with_capacity(count);
    for i in 0..count {
        let (s, e) = (off(i)?, off(i + 1)?);
        if s < 14 || e < s || e > table_start {
            return None;
        }
        records.push(b.get(s..e)?);
    }
    let free_start = off(count)?;
    let free = b
        .get(free_start.min(table_start)..table_start)
        .unwrap_or_default();
    Some(Node {
        kind,
        next,
        records,
        free,
    })
}
