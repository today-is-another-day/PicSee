use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::LargeImageError;

pub const PYRAMID_ALGO_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PyramidKey {
    pub hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LevelMeta {
    pub z: u32,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PyramidManifest {
    pub algo_version: u32,
    pub tile_size: u32,
    pub levels: Vec<LevelMeta>,
}

pub fn pyramid_key(path: &Path, size: u64, mtime_ns: i128, tile_size: u32) -> PyramidKey {
    pyramid_key_with_version(path, size, mtime_ns, tile_size, PYRAMID_ALGO_VERSION)
}

fn pyramid_key_with_version(
    path: &Path,
    size: u64,
    mtime_ns: i128,
    tile_size: u32,
    algo_version: u32,
) -> PyramidKey {
    // N2: canonicalize 失败（如文件已被移动/权限）时，先用 std::path::absolute 规整为
    // 绝对路径再回退，避免同一文件的不同相对路径得到不同 hash → 重复建塔/漏复用。
    // std::path::absolute 不触碰文件系统、不解析 symlink，但保证路径绝对化且不 panic。
    let canonical = fs::canonicalize(path)
        .or_else(|_| std::path::absolute(path))
        .unwrap_or_else(|_| path.to_path_buf());
    let mut hasher = Sha256::new();
    hasher.update(canonical.to_string_lossy().as_bytes());
    hasher.update([0]);
    hasher.update(size.to_le_bytes());
    hasher.update(mtime_ns.to_le_bytes());
    hasher.update(tile_size.to_le_bytes());
    hasher.update(algo_version.to_le_bytes());
    let hash = hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    PyramidKey { hash }
}

pub fn pyramid_dir(cache_root: &Path, key: &PyramidKey) -> PathBuf {
    cache_root.join("large-pyramid").join(&key.hash)
}

pub fn load_manifest(dir: &Path) -> Option<PyramidManifest> {
    let manifest: PyramidManifest =
        serde_json::from_slice(&fs::read(dir.join("manifest.json")).ok()?).ok()?;
    if manifest.algo_version != PYRAMID_ALGO_VERSION
        || manifest.levels.is_empty()
        || manifest
            .levels
            .iter()
            .any(|level| level.z == 0 || !dir.join(format!("z{}.bmp", level.z)).is_file())
    {
        return None;
    }
    Some(manifest)
}

pub fn write_manifest(dir: &Path, manifest: &PyramidManifest) -> Result<(), LargeImageError> {
    fs::create_dir_all(dir)
        .map_err(|error| LargeImageError::io(format!("创建持久金字塔目录失败: {error}")))?;
    let path = dir.join("manifest.json");
    let part = dir.join("manifest.json.part");
    let bytes = serde_json::to_vec_pretty(manifest)
        .map_err(|error| LargeImageError::io(format!("序列化金字塔 manifest 失败: {error}")))?;
    fs::write(&part, bytes)
        .map_err(|error| LargeImageError::io(format!("写金字塔 manifest 失败: {error}")))?;
    fs::rename(&part, &path).map_err(|error| {
        let _ = fs::remove_file(&part);
        LargeImageError::io(format!("发布金字塔 manifest 失败: {error}"))
    })
}

pub fn touch(dir: &Path) {
    let marker = dir.join(".touch");
    if fs::write(&marker, []).is_ok() {
        let _ = fs::remove_file(marker);
    }
}

pub fn evict_to_limit(cache_root: &Path, limit_bytes: u64, protected: &HashSet<String>) {
    if limit_bytes == 0 {
        return;
    }
    let root = cache_root.join("large-pyramid");
    let Ok(entries) = fs::read_dir(&root) else {
        return;
    };
    let mut directories = Vec::new();
    let mut total = 0u64;
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let size = directory_size(&path);
        total = total.saturating_add(size);
        let modified = entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .unwrap_or(UNIX_EPOCH);
        directories.push((modified, path, size));
    }
    if total <= limit_bytes {
        return;
    }
    directories.sort_by_key(|(modified, _, _)| *modified);
    for (_, path, size) in directories {
        let hash = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("");
        if protected.contains(hash) {
            continue;
        }
        if fs::remove_dir_all(&path).is_ok() {
            total = total.saturating_sub(size);
        }
        if total <= limit_bytes {
            break;
        }
    }
}

fn directory_size(path: &Path) -> u64 {
    let Ok(entries) = fs::read_dir(path) else {
        return 0;
    };
    entries
        .filter_map(Result::ok)
        .map(|entry| {
            entry
                .metadata()
                .map(|metadata| {
                    if metadata.is_dir() {
                        directory_size(&entry.path())
                    } else {
                        metadata.len()
                    }
                })
                .unwrap_or(0)
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use filetime::{set_file_mtime, FileTime};

    fn manifest() -> PyramidManifest {
        PyramidManifest {
            algo_version: PYRAMID_ALGO_VERSION,
            tile_size: 512,
            levels: vec![LevelMeta {
                z: 1,
                width: 50,
                height: 25,
            }],
        }
    }

    #[test]
    fn test_pyramid_key_is_stable_and_sensitive() {
        let directory = tempfile::tempdir_in(".").unwrap();
        let path = directory.path().join("source.bmp");
        fs::write(&path, b"bmp").unwrap();
        let key = pyramid_key(&path, 3, 7, 512);
        assert_eq!(key, pyramid_key(&path, 3, 7, 512));
        assert_ne!(key, pyramid_key(&path, 4, 7, 512));
        assert_ne!(key, pyramid_key(&path, 3, 8, 512));
        assert_ne!(key, pyramid_key(&path, 3, 7, 256));
        assert_ne!(
            key,
            pyramid_key_with_version(&path, 3, 7, 512, PYRAMID_ALGO_VERSION + 1)
        );
    }

    #[test]
    fn test_manifest_roundtrip_and_invalidation() {
        let directory = tempfile::tempdir_in(".").unwrap();
        fs::write(directory.path().join("z1.bmp"), b"level").unwrap();
        write_manifest(directory.path(), &manifest()).unwrap();
        assert_eq!(load_manifest(directory.path()), Some(manifest()));

        fs::remove_file(directory.path().join("z1.bmp")).unwrap();
        assert!(load_manifest(directory.path()).is_none());
        fs::write(directory.path().join("z1.bmp"), b"level").unwrap();
        let mut invalid = manifest();
        invalid.algo_version += 1;
        write_manifest(directory.path(), &invalid).unwrap();
        assert!(load_manifest(directory.path()).is_none());
    }

    #[test]
    fn test_evict_to_limit_skips_protected_directory() {
        let cache = tempfile::tempdir_in(".").unwrap();
        let root = cache.path().join("large-pyramid");
        let old = root.join("old");
        let protected = root.join("protected");
        let newest = root.join("new");
        for (path, size) in [(&old, 60), (&protected, 60), (&newest, 60)] {
            fs::create_dir_all(path).unwrap();
            fs::write(path.join("z1.bmp"), vec![0u8; size]).unwrap();
        }
        set_file_mtime(&old, FileTime::from_unix_time(1, 0)).unwrap();
        set_file_mtime(&protected, FileTime::from_unix_time(2, 0)).unwrap();
        set_file_mtime(&newest, FileTime::from_unix_time(3, 0)).unwrap();

        evict_to_limit(cache.path(), 100, &HashSet::from(["protected".to_string()]));

        assert!(!old.exists());
        assert!(protected.exists());
        assert!(!newest.exists());
    }
}
