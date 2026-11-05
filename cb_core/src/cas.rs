use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

/// SHA-256 hex 的固定长度（64 个小写 hex 字符）
const SHA256_HEX_LEN: usize = 64;

/// 校验 SHA-256 hex：必须恰好 64 个字符，且全部为小写 hex。
///
/// 该值来自对端 `ContentBegin.sha256`，会被直接拼进文件系统路径，必须严格校验，
/// 否则 `../x`、空串或非 ASCII 值会导致目录逃逸或 UTF-8 切片 panic。
fn validate_hash(sha256_hex: &str) -> anyhow::Result<()> {
	if sha256_hex.len() != SHA256_HEX_LEN {
		anyhow::bail!(
			"invalid sha256: expected {} hex chars, got {}",
			SHA256_HEX_LEN,
			sha256_hex.len()
		);
	}
	if !sha256_hex
		.bytes()
		.all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
	{
		anyhow::bail!("invalid sha256: expected lowercase hex characters only");
	}
	Ok(())
}

/// 校验单个路径分量（文件名或目录名）。
///
/// 拒绝：空串、`.`、`..`、绝对路径、任何路径分隔符。
fn validate_component(name: &str) -> anyhow::Result<()> {
	if name.is_empty() || name == "." || name == ".." {
		anyhow::bail!("invalid path component: {name:?}");
	}
	let path = Path::new(name);
	if path.is_absolute() || path.components().count() != 1 {
		anyhow::bail!("invalid path component: {name:?}");
	}
	// 白名单校验：解析回来的 file_name 必须与原始字符串完全一致
	if path.file_name().and_then(|n| n.to_str()) != Some(name) {
		anyhow::bail!("invalid path component: {name:?}");
	}
	Ok(())
}

/// 文件身份（卷号, inode / file index）。
///
/// 硬链接共享同一个 inode，因此统计“真实磁盘占用”时必须按身份去重，
/// 否则 `blobs/` 与 `files/`、`downloads/` 指向同一份数据时会被重复累加。
type FileId = (u64, u64);

/// 获取文件身份。
///
/// - Windows：`(VolumeSerialNumber, FileIndex)`，来自 `GetFileInformationByHandle`。
/// - Unix（含 Android / macOS）：`(dev, ino)`。
/// - 取不到时返回 `None`，调用方按“每个路径各计一次”统计（宁可高估，不可低估）。
#[cfg(windows)]
fn file_id(path: &Path, _meta: &fs::Metadata) -> Option<FileId> {
	use std::os::windows::io::AsRawHandle;

	/// `BY_HANDLE_FILE_INFORMATION`（15 个 DWORD）。
	#[repr(C)]
	#[derive(Default)]
	struct ByHandleFileInformation {
		file_attributes: u32,
		creation_time_low: u32,
		creation_time_high: u32,
		last_access_time_low: u32,
		last_access_time_high: u32,
		last_write_time_low: u32,
		last_write_time_high: u32,
		volume_serial_number: u32,
		file_size_high: u32,
		file_size_low: u32,
		number_of_links: u32,
		file_index_high: u32,
		file_index_low: u32,
	}

	#[link(name = "kernel32")]
	extern "system" {
		fn GetFileInformationByHandle(
			file: *mut std::ffi::c_void,
			info: *mut ByHandleFileInformation,
		) -> i32;
	}

	// 只读句柄查询索引；std 的 File::open 以共享方式打开，一般不会阻塞或失败。
	let file = fs::File::open(path).ok()?;
	let mut info = ByHandleFileInformation::default();
	// SAFETY: `file` 持有有效句柄，`info` 是合法的可写结构体指针；
	// 返回 0 表示失败，此时不读取 `info`。
	let ok = unsafe { GetFileInformationByHandle(file.as_raw_handle().cast(), &raw mut info) };
	if ok == 0 {
		return None;
	}
	Some((
		u64::from(info.volume_serial_number),
		(u64::from(info.file_index_high) << 32) | u64::from(info.file_index_low),
	))
}

#[cfg(unix)]
fn file_id(_path: &Path, meta: &fs::Metadata) -> Option<FileId> {
	use std::os::unix::fs::MetadataExt;
	Some((meta.dev(), meta.ino()))
}

#[cfg(not(any(windows, unix)))]
fn file_id(_path: &Path, _meta: &fs::Metadata) -> Option<FileId> {
	None
}

#[derive(Clone, Debug)]
pub struct Cas {
	cache_dir: PathBuf,
	blobs_dir: PathBuf,
	tmp_dir: PathBuf,
}

impl Cas {
	/// 防御性校验：目标路径的父目录 canonicalize 后必须位于 `cache_dir` 之内。
	fn ensure_within_cache(&self, target: &Path) -> anyhow::Result<()> {
		let cache_root = self.cache_dir.canonicalize()?;
		let parent = target
			.parent()
			.ok_or_else(|| anyhow::anyhow!("target path has no parent: {}", target.display()))?;
		let parent = parent.canonicalize()?;
		if !parent.starts_with(&cache_root) {
			anyhow::bail!("refusing to write outside cache dir: {}", target.display());
		}
		Ok(())
	}

	pub fn new(cache_dir: impl AsRef<Path>) -> anyhow::Result<Self> {
		let cache_dir = cache_dir.as_ref().to_path_buf();
		let blobs_dir = cache_dir.join("blobs").join("sha256");
		let tmp_dir = cache_dir.join("tmp");
		fs::create_dir_all(&blobs_dir)?;
		fs::create_dir_all(&tmp_dir)?;
		println!("[cas] blobs_dir = {}", blobs_dir.display());
		Ok(Self {
			cache_dir,
			blobs_dir,
			tmp_dir,
		})
	}

	pub fn blob_path(&self, sha256_hex: &str) -> anyhow::Result<PathBuf> {
		validate_hash(sha256_hex)?;
		let prefix = &sha256_hex[0..2];
		Ok(self.blobs_dir.join(prefix).join(sha256_hex))
	}

	pub fn blob_exists(&self, sha256_hex: &str) -> bool {
		self.blob_path(sha256_hex).is_ok_and(|p| p.exists())
	}

	/// 返回：是否发生了“新写入”
	pub fn put_if_absent(
		&self,
		sha256_hex: &str,
		bytes: &[u8],
		tmp_name: &str,
	) -> anyhow::Result<bool> {
		use std::io;

		let dst = self.blob_path(sha256_hex)?;
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

	/// `<cache_dir>/files`：`materialize_blob` 创建的带后缀视图。
	fn files_dir(&self) -> PathBuf {
		self.cache_dir.join("files")
	}

	/// `<cache_dir>/downloads`：`materialize_file` 创建的按传输隔离的视图。
	fn downloads_dir(&self) -> PathBuf {
		self.cache_dir.join("downloads")
	}

	/// 递归统计目录的“真实”字节数：同一个 inode 只计一次（硬链接不重复计数）。
	///
	/// 无法取得文件身份时退化为按路径计数（宁可高估，避免 GC 低估占用后不回收）。
	fn dir_size(dir: &Path, seen: &mut HashSet<FileId>) -> anyhow::Result<u64> {
		let entries = match fs::read_dir(dir) {
			Ok(entries) => entries,
			Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
			Err(e) => return Err(e.into()),
		};
		let mut sum = 0u64;
		for entry in entries {
			let entry = match entry {
				Ok(entry) => entry,
				// 并发删除（GC / 传输结束）时目录项可能消失，跳过即可
				Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
				Err(e) => return Err(e.into()),
			};
			let path = entry.path();
			let meta = match fs::symlink_metadata(&path) {
				Ok(meta) => meta,
				Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
				Err(e) => return Err(e.into()),
			};
			let ft = meta.file_type();
			if ft.is_dir() {
				sum += Self::dir_size(&path, seen)?;
			} else if ft.is_file() {
				// 硬链接共享 inode：只统计第一次出现的路径，避免 blobs/ 与视图重复累加
				let already_counted = file_id(&path, &meta).is_some_and(|id| !seen.insert(id));
				if !already_counted {
					sum += meta.len();
				}
			}
		}
		Ok(sum)
	}

	/// 统计缓存真实磁盘占用：`blobs/` + `files/` + `downloads/`。
	///
	/// 三个目录里的同一份数据可能是硬链接（共享 inode），因此按文件身份去重，
	/// 每个 inode 只累加一次；复制回退产生的独立副本是真实占用，会被分别计入。
	/// `tmp/` 是瞬态目录，不计入（传输中断后由调用方清理）。
	pub fn total_size_bytes(&self) -> anyhow::Result<i64> {
		let mut seen = HashSet::new();
		let mut total = 0u64;
		total += Self::dir_size(&self.blobs_dir, &mut seen)?;
		total += Self::dir_size(&self.files_dir(), &mut seen)?;
		total += Self::dir_size(&self.downloads_dir(), &mut seen)?;
		Ok(total as i64)
	}

	/// 删除 `files/` 下名字为 `<sha>.<ext>`（或 Windows 归一化后的 `<sha>`）的视图链接。
	fn remove_files_views(&self, sha256_hex: &str) -> anyhow::Result<()> {
		let entries = match fs::read_dir(self.files_dir()) {
			Ok(entries) => entries,
			Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
			Err(e) => return Err(e.into()),
		};
		let prefix = format!("{sha256_hex}.");
		for entry in entries {
			let entry = match entry {
				Ok(entry) => entry,
				Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
				Err(e) => return Err(e.into()),
			};
			let path = entry.path();
			if !path.is_file() {
				continue;
			}
			let name = entry.file_name();
			let name = name.to_string_lossy();
			if name == sha256_hex || name.starts_with(&prefix) {
				match fs::remove_file(&path) {
					Ok(()) => {}
					Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
					Err(e) => {
						return Err(anyhow::Error::new(e)
							.context(format!("failed to remove view link: {}", path.display())))
					}
				}
			}
		}
		Ok(())
	}

	/// 删除 `downloads/` 下与 blob 指向同一 inode 的硬链接视图。
	///
	/// 目录结构是 `downloads/<transfer_id>/<filename>`，没有持久化 sha 映射，
	/// 因此用文件身份（inode）匹配；复制回退产生的独立副本无法匹配（见 [`Cas::remove_blob`]）。
	fn remove_download_views_with_id(&self, id: FileId) -> anyhow::Result<()> {
		fn walk(dir: &Path, id: FileId) -> anyhow::Result<()> {
			let entries = match fs::read_dir(dir) {
				Ok(entries) => entries,
				Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
				Err(e) => return Err(e.into()),
			};
			for entry in entries {
				let entry = match entry {
					Ok(entry) => entry,
					Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
					Err(e) => return Err(e.into()),
				};
				let path = entry.path();
				let meta = match fs::symlink_metadata(&path) {
					Ok(meta) => meta,
					Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
					Err(e) => return Err(e.into()),
				};
				let ft = meta.file_type();
				if ft.is_dir() {
					walk(&path, id)?;
				} else if ft.is_file() && file_id(&path, &meta) == Some(id) {
					match fs::remove_file(&path) {
						Ok(()) => {}
						Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
						Err(e) => {
							return Err(anyhow::Error::new(e).context(format!(
								"failed to remove download view link: {}",
								path.display()
							)))
						}
					}
				}
			}
			Ok(())
		}
		walk(&self.downloads_dir(), id)
	}

	/// 删除 blob，并同步删除引用它的视图链接（`files/<sha>.<ext>` 与
	/// `downloads/<tid>/<name>` 中指向同一 inode 的硬链接），返回被删 blob 自身的字节数。
	///
	/// 硬链接共享 inode：只删 blob 不会真正释放磁盘（视图还持有该 inode），
	/// 所以必须先删视图、再删 blob。调用方也不能把返回值当作“已释放字节数”，
	/// GC 会在删除后用 [`Cas::total_size_bytes`] 重新核算。
	///
	/// 视图删除失败（例如 Windows 上目标文件仍被外壳打开）时返回错误且**不删除 blob**，
	/// 避免出现“blob 没了、视图还在、空间仍被占用”的半完成状态。
	///
	/// 已知限制：`downloads/` 视图没有持久化 sha 映射，只能按 inode 识别硬链接；
	/// 若创建时因文件系统不支持硬链接而退化成了复制，该副本无法被追踪，只能等
	/// `clear_all()` 清理。
	pub fn remove_blob(&self, sha256_hex: &str) -> anyhow::Result<i64> {
		let p = self.blob_path(sha256_hex)?;
		let id = match fs::symlink_metadata(&p) {
			Ok(meta) => match file_id(&p, &meta) {
				Some(id) => Some(id),
				// 取不到身份就无法确认 downloads/ 里是否还有硬链接视图，
				// 拒绝删除，避免“账面释放、实际未释放”。
				None => anyhow::bail!(
					"cannot determine file identity for blob {sha256_hex}; refusing to evict"
				),
			},
			Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
			Err(e) => return Err(e.into()),
		};

		// 1) files/ 视图按名字精确匹配
		self.remove_files_views(sha256_hex)?;

		// 2) downloads/ 视图按同一 inode 识别
		if let Some(id) = id {
			self.remove_download_views_with_id(id)?;
		}

		// 3) 最后删 blob 本身
		if !p.exists() {
			return Ok(0);
		}
		let sz = p.metadata().map_or(0, |m| m.len() as i64);
		fs::remove_file(p)?;
		Ok(sz)
	}

	#[allow(dead_code)]
	pub fn cache_dir(&self) -> &Path {
		&self.cache_dir
	}

	/// M3: 获取一个用于传输的临时文件路径
	/// 格式: <cache_dir>/tmp/<transfer_id>
	///
	/// transfer_id 来自对端 `ContentBegin.req_id`，必须校验为单一安全分量，
	/// 否则对端可以借 `../` 写到缓存目录之外。
	pub fn get_tmp_path(&self, transfer_id: &str) -> anyhow::Result<PathBuf> {
		validate_component(transfer_id)?;
		Ok(self.tmp_dir.join(transfer_id))
	}

	/// M3: 将临时文件“转正”为 Blob
	/// 1. 检查 sha256 是否匹配 (Actor 已校验，这里主要是移动文件)
	/// 2. 移动(rename)到 blobs 目录
	///
	/// 返回: 最终的 blob 路径
	pub fn commit_tmp_file(&self, tmp_path: &Path, sha256: &str) -> anyhow::Result<PathBuf> {
		let dst = self.blob_path(sha256)?;

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
			Ok(()) => Ok(dst),
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
		let blob_path = self.blob_path(sha256)?;
		if !blob_path.exists() {
			anyhow::bail!("Blob not found: {sha256}");
		}

		// 构造目标路径：cache_dir/files/<sha256>.<ext>
		// 这样可以避免同一个 sha 不同后缀的冲突，且易于清理
		let views_dir = self.cache_dir.join("files");
		if !views_dir.exists() {
			fs::create_dir_all(&views_dir)?;
		}

		// 清理 ext 中的点，防止传入 ".png" 导致 "..png"
		let safe_ext = ext.trim_start_matches('.');
		let filename = format!("{sha256}.{safe_ext}");
		let target_path = views_dir.join(filename);
		self.ensure_within_cache(&target_path)?;

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
	pub fn materialize_file(
		&self,
		sha256: &str,
		transfer_id: &str,
		filename: &str,
	) -> anyhow::Result<PathBuf> {
		// sha256、transfer_id、filename 都来自对端（ContentBegin / ItemMeta），
		// 任一值未校验都可能把文件写出 cache_dir。
		validate_hash(sha256)?;
		validate_component(transfer_id)?;
		validate_component(filename)?;

		let blob_path = self.blob_path(sha256)?;
		if !blob_path.exists() {
			anyhow::bail!("Blob not found: {sha256}");
		}

		// 目录结构：cache/downloads/transfer_id/
		let download_dir = self.cache_dir.join("downloads").join(transfer_id);
		fs::create_dir_all(&download_dir)?;

		let target_path = download_dir.join(filename);
		self.ensure_within_cache(&target_path)?;

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
		let target_path = self.blob_path(&sha256)?;
		if target_path.exists() {
			return Ok(sha256);
		}

		// 3. 写入临时文件
		// 使用一个随机 UUID 或简单名字做临时文件名
		let tmp_name = format!("ingest_{}", uuid::Uuid::new_v4());
		let tmp_path = self.tmp_dir.join(tmp_name);

		{
			let mut file = fs::File::create(&tmp_path)?;
			file.write_all(data)?;
			file.flush()?;
		}

		// 4. Commit (复用已有的 commit_tmp_file 或直接 rename)
		// 这里直接调用我们在 Step 2 实现的 commit_tmp_file 即可
		// 注意：commit_tmp_file 可能需要 public，或者在这里直接写 rename 逻辑
		self.commit_tmp_file(&tmp_path, &sha256)?;

		Ok(sha256)
	}

	/// 清空所有缓存（删除所有 blobs 和临时文件）
	pub fn clear_all(&self) -> anyhow::Result<()> {
		// 删除 blobs 目录下的所有文件
		if self.blobs_dir.exists() {
			// 遍历所有子目录并删除文件
			fn remove_dir_contents(dir: &Path) -> anyhow::Result<()> {
				if !dir.exists() {
					return Ok(());
				}
				for entry in fs::read_dir(dir)? {
					let entry = entry?;
					let path = entry.path();
					if path.is_dir() {
						// 递归删除子目录
						remove_dir_contents(&path)?;
						let _ = fs::remove_dir(&path);
					} else {
						let _ = fs::remove_file(&path);
					}
				}
				Ok(())
			}
			remove_dir_contents(&self.blobs_dir)?;
		}

		// 删除 tmp 目录下的所有文件
		if self.tmp_dir.exists() {
			for entry in fs::read_dir(&self.tmp_dir)? {
				let entry = entry?;
				let path = entry.path();
				if path.is_file() {
					let _ = fs::remove_file(&path);
				}
			}
		}

		// 删除 files 目录（materialize_blob 创建的视图文件）
		let files_dir = self.cache_dir.join("files");
		if files_dir.exists() {
			for entry in fs::read_dir(&files_dir)? {
				let entry = entry?;
				let path = entry.path();
				if path.is_file() {
					let _ = fs::remove_file(&path);
				}
			}
		}

		// 删除 downloads 目录（materialize_file 创建的文件）
		let downloads_dir = self.cache_dir.join("downloads");
		if downloads_dir.exists() {
			fn remove_dir_recursive(dir: &Path) -> anyhow::Result<()> {
				if !dir.exists() {
					return Ok(());
				}
				for entry in fs::read_dir(dir)? {
					let entry = entry?;
					let path = entry.path();
					if path.is_dir() {
						remove_dir_recursive(&path)?;
						let _ = fs::remove_dir(&path);
					} else {
						let _ = fs::remove_file(&path);
					}
				}
				Ok(())
			}
			remove_dir_recursive(&downloads_dir)?;
		}

		Ok(())
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn valid_hash() -> String {
		"0123456789abcdef".repeat(4)
	}

	#[test]
	fn accept_normal_hash() {
		assert!(validate_hash(&valid_hash()).is_ok());
		assert!(validate_hash(&"0".repeat(64)).is_ok());
	}

	#[test]
	fn reject_bad_hash() {
		let cases = vec![
			String::new(),
			"abc".to_string(),
			valid_hash().to_uppercase(),
			"g".repeat(64),
			format!("{}{}", "a".repeat(62), "é"), // 非 ASCII（字节长度仍是 64）
			format!("{}/..", "a".repeat(61)),     // 含 ../ 的 64 字符值
			"../x".to_string(),
		];
		for bad in cases {
			assert!(
				validate_hash(&bad).is_err(),
				"expected hash rejection: {bad:?}"
			);
		}
	}

	#[test]
	fn accept_plain_component() {
		assert!(validate_component("report.pdf").is_ok());
		assert!(validate_component("a.txt").is_ok());
	}

	#[test]
	fn reject_unsafe_component() {
		for bad in ["../a", "/etc/passwd", "a/b", "..", "", "."] {
			assert!(
				validate_component(bad).is_err(),
				"expected component rejection: {bad:?}"
			);
		}
	}

	#[test]
	fn blob_path_is_validated() {
		let dir = tempfile::tempdir().unwrap();
		let cas = Cas::new(dir.path()).unwrap();
		let p = cas.blob_path(&valid_hash()).unwrap();
		assert!(p.starts_with(&cas.blobs_dir));
		assert_eq!(p.file_name().unwrap().to_str().unwrap(), valid_hash());
		// 穿越/空值必须在进入文件系统前失败
		assert!(cas.blob_path("../x").is_err());
		assert!(cas.blob_path("").is_err());
		assert!(!cas.blob_exists("../x"));
	}

	#[test]
	fn materialize_file_rejects_unsafe_names() {
		let dir = tempfile::tempdir().unwrap();
		let cas = Cas::new(dir.path()).unwrap();
		let sha = cas.put_blob(b"hello").unwrap();

		for bad in ["../a", "/etc/passwd", "a/b", "..", ""] {
			assert!(
				cas.materialize_file(&sha, "tid", bad).is_err(),
				"expected filename rejection: {bad:?}"
			);
			assert!(
				cas.materialize_file(&sha, bad, "a.txt").is_err(),
				"expected transfer_id rejection: {bad:?}"
			);
		}

		let target = cas.materialize_file(&sha, "tid-1", "a.txt").unwrap();
		let cache_root = cas.cache_dir().canonicalize().unwrap();
		assert!(target.canonicalize().unwrap().starts_with(&cache_root));
	}

	#[test]
	fn total_size_counts_hard_linked_views_once() {
		let dir = tempfile::tempdir().unwrap();
		let cas = Cas::new(dir.path()).unwrap();
		let data = [7u8; 128];
		let sha = cas.put_blob(&data).unwrap();

		let blob_view = cas.materialize_blob(&sha, "bin").unwrap();
		let download_view = cas.materialize_file(&sha, "tid-1", "a.bin").unwrap();
		assert!(blob_view.exists());
		assert!(download_view.exists());

		// blob + files/ 视图 + downloads/ 视图 是同一个 inode（硬链接），
		// 真实占用只应计算一次 128 字节。
		assert_eq!(cas.total_size_bytes().unwrap(), 128);
	}

	#[test]
	fn remove_blob_removes_view_links_and_frees_bytes() {
		let dir = tempfile::tempdir().unwrap();
		let cas = Cas::new(dir.path()).unwrap();
		let data = [9u8; 256];
		let sha = cas.put_blob(&data).unwrap();

		let blob_view = cas.materialize_blob(&sha, "png").unwrap();
		let download_view = cas.materialize_file(&sha, "tid-2", "b.png").unwrap();
		assert_eq!(cas.total_size_bytes().unwrap(), 256);

		cas.remove_blob(&sha).unwrap();

		// blob 与两个视图链接都必须消失，占用归零（否则 GC 会认为已释放而实际没有）
		assert!(!cas.blob_exists(&sha));
		assert!(!blob_view.exists());
		assert!(!download_view.exists());
		assert_eq!(cas.total_size_bytes().unwrap(), 0);
	}
}
