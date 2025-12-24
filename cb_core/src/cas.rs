use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use sha2::{Digest, Sha256};
#[derive(Clone, Debug)]
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

    /// M3: 获取一个用于传输的临时文件路径
    /// 格式: <cache_dir>/tmp/<transfer_id>
    pub fn get_tmp_path(&self, transfer_id: &str) -> PathBuf {
        self.tmp_dir.join(transfer_id)
    }

    /// M3: 将临时文件“转正”为 Blob
    /// 1. 检查 sha256 是否匹配 (Actor 已校验，这里主要是移动文件)
    /// 2. 移动(rename)到 blobs 目录
    /// 返回: 最终的 blob 路径
    pub fn commit_tmp_file(&self, tmp_path: &Path, sha256: &str) -> anyhow::Result<PathBuf> {
        let dst = self.blob_path(sha256);

        // 1. 目标如果已存在，直接删除临时文件并返回成功
        if dst.exists() {
            let _ = fs::remove_file(tmp_path);
            return Ok(dst);
        }

        // 2. 确保父目录存在
        if let Some(parent) = dst.parent() {
            fs::create_dir_all(parent)?;
        }

        // 3. 原子重命名
        // 注意：跨分区 rename 可能会失败，但在 cache_dir 内部通常没问题
        match fs::rename(tmp_path, &dst) {
            Ok(_) => Ok(dst),
            Err(e) => {
                // 如果失败，清理临时文件
                let _ = fs::remove_file(tmp_path);
                Err(e.into())
            }
        }
    }

    /// M3: 为 Blob 创建一个带后缀的“视图” (用于 Image/File)
    /// 策略：尝试硬链接，不支持则复制。文件名为 <sha256>.<ext>，放在 blobs/views/ 目录下 (或 cache_dir/files)
    pub fn materialize_blob(&self, sha256: &str, ext: &str) -> anyhow::Result<PathBuf> {
        let blob_path = self.blob_path(sha256);
        if !blob_path.exists() {
            anyhow::bail!("Blob not found: {}", sha256);
        }

        // 构造目标路径：cache_dir/files/<sha256>.<ext>
        // 这样可以避免同一个 sha 不同后缀的冲突，且易于清理
        let views_dir = self.cache_dir.join("files");
        if !views_dir.exists() {
            fs::create_dir_all(&views_dir)?;
        }

        // 清理 ext 中的点，防止传入 ".png" 导致 "..png"
        let safe_ext = ext.trim_start_matches('.');
        let filename = format!("{}.{}", sha256, safe_ext);
        let target_path = views_dir.join(filename);

        if target_path.exists() {
            return Ok(target_path);
        }

        // 尝试硬链接 (高性能)
        if fs::hard_link(&blob_path, &target_path).is_err() {
            // 硬链接失败（可能是跨分区），回退到复制
            fs::copy(&blob_path, &target_path)?;
        }

        Ok(target_path)
    }

    /// M3-3: 为 Blob 创建指定文件名的“视图”
    /// 路径: <cache_dir>/downloads/<transfer_id>/<filename>
    /// 这样可以隔离不同传输的同名文件，且方便 Shell 访问
    pub fn materialize_file(&self, sha256: &str, transfer_id: &str, filename: &str) -> anyhow::Result<PathBuf> {
        let blob_path = self.blob_path(sha256);
        if !blob_path.exists() {
            anyhow::bail!("Blob not found: {}", sha256);
        }

        // 目录结构：cache/downloads/transfer_id/
        let download_dir = self.cache_dir.join("downloads").join(transfer_id);
        fs::create_dir_all(&download_dir)?;

        let target_path = download_dir.join(filename);

        // 如果文件已存在且 Hash 一致，直接返回（断点续传/幂等优化）
        if target_path.exists() {
            // 这里简略跳过 hash 校验，假设路径独占
            return Ok(target_path);
        }

        // 尝试硬链接，失败则复制
        if fs::hard_link(&blob_path, &target_path).is_err() {
            fs::copy(&blob_path, &target_path)?;
        }

        Ok(target_path)
    }

    /// 辅助方法：直接将内存数据写入 Blob，返回 sha256
    pub fn put_blob(&self, data: &[u8]) -> anyhow::Result<String> {
        // 1. 计算 Hash
        let mut hasher = Sha256::new();
        hasher.update(data);
        let sha256 = hex::encode(hasher.finalize());

        // 2. 检查是否已存在
        let target_path = self.blob_path(&sha256);
        if target_path.exists() {
            return Ok(sha256);
        }

        // 3. 写入临时文件
        // 使用一个随机 UUID 或简单名字做临时文件名
        let tmp_name = format!("ingest_{}", uuid::Uuid::new_v4());
        let tmp_path = self.tmp_dir.join(tmp_name);

        {
            let mut file = std::fs::File::create(&tmp_path)?;
            file.write_all(data)?;
            file.flush()?;
        }

        // 4. Commit (复用已有的 commit_tmp_file 或直接 rename)
        // 这里直接调用我们在 Step 2 实现的 commit_tmp_file 即可
        // 注意：commit_tmp_file 可能需要 public，或者在这里直接写 rename 逻辑
        self.commit_tmp_file(&tmp_path, &sha256)?;

        Ok(sha256)
    }
}
