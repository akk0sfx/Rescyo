//! Where everything is on a FAT or exFAT volume.

use rescyo_fs::{Evidence, FsKind, VolumeCandidate};
use serde::{Deserialize, Serialize};

/// One copy of the allocation table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FatCopy {
    /// Absolute byte offset on the Source.
    pub offset: u64,
    /// Length in bytes.
    pub len: u64,
}

/// Where the root directory lives.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RootDir {
    /// FAT32 and exFAT: a cluster chain.
    Cluster { cluster: u32 },
    /// FAT12/16: a fixed region between the FATs and the data area.
    Region { offset: u64, len: u64 },
    /// Not known (inferred geometry): only seeds and scans can find directories.
    Unknown,
}

/// Everything the engine needs to read a volume. Stored as a Volume candidate's `params`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FatGeometry {
    pub fs: FsKind,
    /// Byte offset of the boot sector, when known.
    pub volume_offset: Option<u64>,
    pub bytes_per_sector: u32,
    pub cluster_size: u32,
    /// Absolute byte offset of cluster #2.
    pub data_offset: u64,
    /// Highest valid cluster number.
    pub max_cluster: u32,
    pub fats: Vec<FatCopy>,
    pub root: RootDir,
    pub label: Option<String>,
}

impl FatGeometry {
    /// Absolute byte offset of `cluster`, if it is a valid data cluster.
    pub fn cluster_offset(&self, cluster: u32) -> Option<u64> {
        if cluster < 2 || cluster > self.max_cluster {
            return None;
        }
        Some(self.data_offset + u64::from(cluster - 2) * u64::from(self.cluster_size))
    }

    /// The cluster that starts at absolute `offset`, if it lies exactly on the cluster grid.
    pub fn cluster_at(&self, offset: u64) -> Option<u32> {
        let rel = offset.checked_sub(self.data_offset)?;
        let c = u64::from(self.cluster_size);
        if !rel.is_multiple_of(c) {
            return None;
        }
        let cluster = u32::try_from(rel / c + 2).ok()?;
        (cluster <= self.max_cluster).then_some(cluster)
    }

    /// End of the data area (exclusive).
    pub fn data_end(&self) -> u64 {
        self.data_offset + u64::from(self.max_cluster.saturating_sub(1)) * u64::from(self.cluster_size)
    }

    pub fn is_exfat(&self) -> bool {
        self.fs == FsKind::Exfat
    }

    pub fn to_candidate(&self, evidence: Vec<Evidence>, score: u64) -> VolumeCandidate {
        VolumeCandidate {
            fs: self.fs,
            offset: self.volume_offset,
            cluster_size: self.cluster_size,
            data_offset: self.data_offset,
            size: self.volume_offset.map(|v| self.data_end().saturating_sub(v)),
            label: self.label.clone(),
            evidence,
            score,
            params: serde_json::to_value(self).unwrap_or(serde_json::Value::Null),
        }
    }

    /// Recovers the geometry from a candidate produced by this crate.
    pub fn from_candidate(c: &VolumeCandidate) -> Option<Self> {
        let g: Self = serde_json::from_value(c.params.clone()).ok()?;
        g.is_sane().then_some(g)
    }

    /// Rejects geometries that no valid volume could have (hostile or corrupted params).
    pub fn is_sane(&self) -> bool {
        let cs = self.cluster_size;
        let bps = self.bytes_per_sector;
        cs.is_power_of_two()
            && (512..=(32 << 20)).contains(&cs)
            && bps.is_power_of_two()
            && (512..=4096).contains(&bps)
            && self.max_cluster >= 2
            && self.max_cluster <= 0x0FFF_FFF6
            && self.fats.len() <= 2
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn geo() -> FatGeometry {
        FatGeometry {
            fs: FsKind::Fat32,
            volume_offset: Some(0),
            bytes_per_sector: 512,
            cluster_size: 0x4000,
            data_offset: 0x100_0000,
            max_cluster: 1_918_209,
            fats: vec![],
            root: RootDir::Cluster { cluster: 2 },
            label: None,
        }
    }

    #[test]
    fn cluster_grid_matches_the_flash_drive() {
        let g = geo();
        // From the DMDE log: dirclus 0x1c48000:788.
        assert_eq!(g.cluster_offset(788), Some(0x1c4_8000));
        assert_eq!(g.cluster_at(0x1c4_8000), Some(788));
        // Off-grid entries such as 0xf00ff000 are rejected.
        assert_eq!(g.cluster_at(0xf00f_f000), None);
        assert_eq!(g.cluster_offset(1), None);
        assert_eq!(g.cluster_offset(g.max_cluster + 1), None);
    }

    #[test]
    fn candidate_round_trip_and_sanity() {
        let g = geo();
        let c = g.to_candidate(vec![], 10);
        assert_eq!(FatGeometry::from_candidate(&c), Some(g.clone()));
        let mut bad = c.clone();
        bad.params["cluster_size"] = serde_json::json!(3000);
        assert_eq!(FatGeometry::from_candidate(&bad), None);
    }
}
