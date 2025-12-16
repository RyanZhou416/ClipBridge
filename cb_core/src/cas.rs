use std::fs;
use std::path::{Path, PathBuf};

pub struct Cas {
    cache_dir: PathBuf,
    blobs_dir: PathBuf,
    tmp_dir: PathBuf,
}

impl Cas {
    pub fn new(cache_dir: impl AsRef<Path>) -> anyhow::Result<Self> {
        let cache_dir = cache_dir.as_ref().to_path_buf();
        let blobs_dir = cache_dir.join("blobs").join("sha256");
        let tmp_dir = cache_dir.join("tmp");
        fs::create_dir_all(&blobs_dir)?;
        fs::create_dir_all(&tmp_dir)?;
        Ok(Self { cache_dir, blobs_dir, tmp_dir })
    }

    pub fn blob_path(&self, sha256_hex: &str) -> PathBuf {
        let prefix = &sha256_hex[0..2];
        self.blobs_dir.join(prefix).join(sha256_hex)
    }

    pub fn blob_exists(&self, sha256_hex: &str) -> bool {
        self.blob_path(sha256_hex).exists()
    }

    /// 返回：是否发生了“新写入”
    pub fn put_if_absent(&self, sha256_hex: &str, bytes: &[u8], tmp_name: &str) -> anyhow::Result<bool> {
        let dst = self.blob_path(sha256_hex);
        if dst.exists() {
            return Ok(false);
        }
        if let Some(parent) = dst.parent() {
            fs::create_dir_all(parent)?;
        }

        let tmp = self.tmp_dir.join(tmp_name);
        fs::write(&tmp, bytes)?;
        // 原子替换：tmp -> dst
        fs::rename(&tmp, &dst)?;
        Ok(true)
    }

    #[allow(dead_code)]
    pub fn cache_dir(&self) -> &Path {
        &self.cache_dir
    }
}
