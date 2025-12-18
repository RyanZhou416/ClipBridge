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
        println!("[cas] blobs_dir = {:?}", blobs_dir);
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
    pub fn put_if_absent(
        &self,
        sha256_hex: &str,
        bytes: &[u8],
        tmp_name: &str,
    ) -> anyhow::Result<bool> {
        use std::io;

        let dst = self.blob_path(sha256_hex);
        if dst.exists() {
            return Ok(false);
        }
        if let Some(parent) = dst.parent() {
            fs::create_dir_all(parent)?;
        }

        let tmp = self.tmp_dir.join(tmp_name);
        fs::write(&tmp, bytes)?;

        // 最后再查一次，减少“覆盖写”的概率（仍可能竞态，但 worst case 写入相同内容）
        if dst.exists() {
            let _ = fs::remove_file(&tmp);
            return Ok(false);
        }

        match fs::rename(&tmp, &dst) {
            Ok(()) => Ok(true),

            // 竞态：别人已经写好了（Windows 常见：AlreadyExists）
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                let _ = fs::remove_file(&tmp);
                Ok(false)
            }

            // 其他错误：清理 tmp，再向上抛
            Err(e) => {
                let _ = fs::remove_file(&tmp);
                Err(e.into())
            }
        }
    }

    pub fn total_size_bytes(&self) -> anyhow::Result<i64> {
        fn dir_size(p: &Path) -> std::io::Result<u64> {
            let mut sum = 0u64;
            for e in fs::read_dir(p)? {
                let e = e?;
                let m = e.metadata()?;
                if m.is_dir() {
                    sum += dir_size(&e.path())?;
                } else {
                    sum += m.len();
                }
            }
            Ok(sum)
        }
        let s = dir_size(&self.blobs_dir)?;
        Ok(s as i64)
    }

    /// 删除 blob，返回释放的字节数（若不存在返回 0）
    pub fn remove_blob(&self, sha256_hex: &str) -> anyhow::Result<i64> {
        let p = self.blob_path(sha256_hex);
        if !p.exists() {
            return Ok(0);
        }
        let sz = p.metadata().map(|m| m.len() as i64).unwrap_or(0);
        fs::remove_file(p)?;
        Ok(sz)
    }


    #[allow(dead_code)]
    pub fn cache_dir(&self) -> &Path {
        &self.cache_dir
    }


}
