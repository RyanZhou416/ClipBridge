use anyhow::Context as _;
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{
	atomic::{AtomicBool, Ordering},
	Arc, Mutex,
};
use tokio::sync::{mpsc, watch};

/// SHA-256 hex 的固定长度（64 个小写 hex 字符）
const SHA256_HEX_LEN: usize = 64;

/// 环境变量：选择接收路径的 fsync 策略（`always` | `large-only` | `never`）。
///
/// 只在 `Core::init` 读取一次，之后由 [`Cas`] 持有；接收路径（`session::actor`）
/// 通过 [`Cas::fsync_policy`] 读取，绝不在每次传输时重新解析环境变量。
pub const FSYNC_POLICY_ENV: &str = "CB_FSYNC_POLICY";

/// 大文件阈值：8 MiB。
///
/// `large-only` 策略下，`total_bytes < 8 MiB` 的传输在 rename 前同步 `sync_all()`；
/// `>= 8 MiB` 的传输走“先 rename 上报完成、再由后台 `sync_data()`”的折衷路径。
///
/// 取值依据（ADR-002 §3.7，本机 NVMe 实测）：
/// - `fsync` 吞吐约 **740 MiB/s**（256 MiB / 0.346 s）；
/// - 局域网接收吞吐约 **100–190 MiB/s**；
/// - 因此对 256 MiB 文件，同步 fsync 约 0.35 s，1–2.5 GiB 级别传输会累积
///   0.3–2.6 s 的尾延迟。8 MiB 以下的 fsync 通常在 10 ms 量级，值得为“报告完成
///   即已落盘”付出；8 MiB 以上则改为后台补同步，避免把尾延迟压在传输完成路径上。
pub const LARGE_FILE_FSYNC_THRESHOLD_BYTES: u64 = 8 * 1024 * 1024;

/// 后台持久化队列的容量（每 Core 一个队列）。
///
/// 队列满时不再等待：该传输的 `content_cache.durable` 已经是 0，30 s 周期 sweep
/// 会兜底扫到它。这样后台同步永远不会反向阻塞接收路径。
pub const DURABILITY_QUEUE_CAPACITY: usize = 64;

/// 接收完成路径的 fsync 策略（`CB_FSYNC_POLICY`）。
///
/// # 决策表（详见 [`FsyncPolicy::decision`]）
///
/// | 策略 | 大小 | rename 前 | 上报完成前 | 崩溃可能丢失的内容 |
/// |------|------|-----------|------------|--------------------|
/// | `always` | 任意 | `sync_all()` | 等 `sync_all` 成功 | 内容数据不会丢；未 fsync 的父目录项仍有极小窗口 |
/// | `large-only`（默认） | `< 8 MiB` | `sync_all()` | 等 `sync_all` 成功 | 同上（小文件，代价毫秒级） |
/// | `large-only`（默认） | `>= 8 MiB` | 无（rename 即上报） | rename 成功即可；`durable=0` | 后台 `sync_data` 完成前断电：最后这次大传输可能丢失/截断 |
/// | `never` | 任意 | 无 | 立即 | 断电可能丢掉最近所有“已报告完成”的内容（页缓存未落盘） |
///
/// 取舍：同步 fsync 会按文件尺寸增加完成尾延迟（本机约 740 MiB/s，见
/// [`LARGE_FILE_FSYNC_THRESHOLD_BYTES`]）；`large-only` 用 8 MiB 切分，把
/// “报告完成即持久”的承诺留给真正便宜的传输，对大文件显式接受一段
/// “已报告完成但尚未 sync”的窗口，并由 `durable` 标记 + 后台 sweep 兜底。
///
/// 注意：rename 之后没有对父目录再做 fsync，因此所有模式都存在“目录项本身
/// 未落盘”的窄窗口（本任务只按 ADR 要求同步文件内容，不扩展目录同步）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FsyncPolicy {
	/// 每次传输都在 commit/rename 前 `sync_all()`。
	Always,
	/// 默认：小文件同步 `sync_all()`，大文件 rename 后后台 `sync_data()`。
	#[default]
	LargeOnly,
	/// 完全不做 fsync（等价于改动前的行为）。
	Never,
}

impl FsyncPolicy {
	pub fn as_str(self) -> &'static str {
		match self {
			Self::Always => "always",
			Self::LargeOnly => "large-only",
			Self::Never => "never",
		}
	}

	/// 解析 `CB_FSYNC_POLICY` 的取值。
	///
	/// `None` / 空串按默认值（`large-only`）处理；大小写与首尾空白会被归一化。
	/// 非法值返回 `Err(原始值)`，由调用方（`Core::init`）记录警告后回退默认值。
	pub fn parse(raw: Option<&str>) -> Result<Self, String> {
		match raw.map(str::trim) {
			None | Some("") => Ok(Self::default()),
			Some(value) => match value.to_ascii_lowercase().as_str() {
				"always" => Ok(Self::Always),
				"large-only" => Ok(Self::LargeOnly),
				"never" => Ok(Self::Never),
				_ => Err(value.to_string()),
			},
		}
	}

	/// 按“本次传输总字节数”给出 fsync 决策。
	///
	/// 边界：`large-only` 下 `<` 8 MiB 同步；**恰好等于** 8 MiB 已属于大文件，
	/// 走后台补同步（与 `>=` 的文案一致，避免边界歧义）。
	pub fn decision(self, total_bytes: u64) -> FsyncDecision {
		match self {
			Self::Always => FsyncDecision::SyncBeforeCommit,
			Self::Never => FsyncDecision::NoSync,
			Self::LargeOnly => {
				if total_bytes < LARGE_FILE_FSYNC_THRESHOLD_BYTES {
					FsyncDecision::SyncBeforeCommit
				} else {
					FsyncDecision::CommitThenSyncInBackground
				}
			}
		}
	}
}

/// 一次传输在 commit 前/后要执行的持久化动作。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FsyncDecision {
	/// rename 前对临时文件（或已存在的最终 blob）执行 `sync_all()`，失败则传输失败。
	SyncBeforeCommit,
	/// rename 后立即上报完成，把 `sync_data()` 交给后台任务，并置 `durable=0`。
	CommitThenSyncInBackground,
	/// 不做任何 fsync（`never`）。
	NoSync,
}

/// 实际完成的持久化结果（随最终化结果回传给 actor）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DurabilityOutcome {
	/// 已在 rename 前同步落盘（小文件 / `always`）。
	Synced,
	/// 已 rename 到最终路径，但 `sync_data` 仍待后台完成（大文件 / `large-only`）。
	Pending,
	/// 按 `never` 策略没有做 fsync。
	NotSynced,
}

/// `commit_tmp_file_with_policy` 的结果。
#[derive(Clone, Debug)]
pub struct CommitOutcome {
	/// 最终 blob 路径（rename 之后）。
	pub blob_path: PathBuf,
	/// 本次 commit 的持久化状态。
	pub durability: DurabilityOutcome,
}

/// 可注入的文件同步实现。
///
/// 生产路径用 [`OsFileSyncer`]；测试用它注入计数器，从而在没有真实 socket /
/// 慢磁盘的情况下断言“小文件写路径确实调用了 sync 步骤”。
pub trait FileSyncer: Send + Sync + std::fmt::Debug {
	fn sync_all(&self, path: &Path) -> std::io::Result<()>;
	fn sync_data(&self, path: &Path) -> std::io::Result<()>;
}

/// 真实文件系统同步（`fsync` / `FlushFileBuffers`）。
#[derive(Debug, Default)]
pub struct OsFileSyncer;

impl FileSyncer for OsFileSyncer {
	fn sync_all(&self, path: &Path) -> std::io::Result<()> {
		// Windows 的 FlushFileBuffers 需要带写权限的句柄，因此不能只读打开；
		// Unix 上写权限打开后 fsync 行为相同。
		let file = fs::OpenOptions::new().write(true).open(path)?;
		file.sync_all()
	}

	fn sync_data(&self, path: &Path) -> std::io::Result<()> {
		let file = fs::OpenOptions::new().write(true).open(path)?;
		file.sync_data()
	}
}

/// 后台补同步队列里的一条待办。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingSync {
	/// 需要 `sync_data()` 的最终 blob（文件名 = 传输 hash）。
	pub blob_sha256: String,
	/// 同步成功后要翻回 `durable=1` 的 `content_cache` 主键（item content hash）。
	pub cache_sha256: String,
}

/// 每 Core 一份的 durable 队列与 worker 句柄。
///
/// `Cas` 会被 clone 给所有会话，`Arc` 保证队列/“只启动一次”的标记全局唯一。
pub(crate) struct DurabilityQueue {
	tx: mpsc::Sender<PendingSync>,
	rx: Mutex<Option<mpsc::Receiver<PendingSync>>>,
	started: AtomicBool,
	stop_tx: watch::Sender<bool>,
}

impl std::fmt::Debug for DurabilityQueue {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("DurabilityQueue")
			.field("queue_capacity", &DURABILITY_QUEUE_CAPACITY)
			.field("started", &self.started.load(Ordering::Acquire))
			.field("tx", &self.tx)
			.field("rx", &self.rx)
			.field("stop_tx", &self.stop_tx)
			.finish()
	}
}

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
	/// 接收路径采用的 fsync 策略（`Core::init` 解析一次后注入）。
	fsync_policy: FsyncPolicy,
	/// 可注入的同步实现：生产为真实 fsync，测试为计数器。
	syncer: Arc<dyn FileSyncer>,
	/// 后台补同步队列（每 Core 一份，clone 共享）。
	durability: Arc<DurabilityQueue>,
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

	/// 默认构造：`large-only` 策略 + 真实 fsync。
	///
	/// 生产路径（`Core::init`）使用 [`Cas::new_with_policy`]，把从环境变量解析出的
	/// 策略显式注入；测试/会话夹具使用本函数即可保持默认行为。
	pub fn new(cache_dir: impl AsRef<Path>) -> anyhow::Result<Self> {
		Self::new_with_policy(cache_dir, FsyncPolicy::default())
	}

	/// 用指定 fsync 策略构造（`CB_FSYNC_POLICY` 在 `Core::init` 只解析一次）。
	pub fn new_with_policy(
		cache_dir: impl AsRef<Path>,
		policy: FsyncPolicy,
	) -> anyhow::Result<Self> {
		Self::new_with_syncer(cache_dir, policy, Arc::new(OsFileSyncer))
	}

	/// 测试入口：注入自定义同步实现，用于断言 sync 步骤被调用。
	pub(crate) fn new_with_syncer(
		cache_dir: impl AsRef<Path>,
		policy: FsyncPolicy,
		syncer: Arc<dyn FileSyncer>,
	) -> anyhow::Result<Self> {
		let cache_dir = cache_dir.as_ref().to_path_buf();
		let blobs_dir = cache_dir.join("blobs").join("sha256");
		let tmp_dir = cache_dir.join("tmp");
		fs::create_dir_all(&blobs_dir)?;
		fs::create_dir_all(&tmp_dir)?;
		println!("[cas] blobs_dir = {}", blobs_dir.display());
		let (tx, rx) = mpsc::channel(DURABILITY_QUEUE_CAPACITY);
		let (stop_tx, _stop_rx) = watch::channel(false);
		Ok(Self {
			cache_dir,
			blobs_dir,
			tmp_dir,
			fsync_policy: policy,
			syncer,
			durability: Arc::new(DurabilityQueue {
				tx,
				rx: Mutex::new(Some(rx)),
				started: AtomicBool::new(false),
				stop_tx,
			}),
		})
	}

	/// 本 Core 的 fsync 策略（接收路径只读这个值）。
	pub fn fsync_policy(&self) -> FsyncPolicy {
		self.fsync_policy
	}

	/// `fsync`（数据 + 元数据）。`commit_tmp_file_with_policy` 的同步分支使用。
	pub(crate) fn sync_all_file(&self, path: &Path) -> std::io::Result<()> {
		self.syncer.sync_all(path)
	}

	/// `fdatasync`（仅数据）。后台补同步使用。
	pub(crate) fn sync_data_file(&self, path: &Path) -> std::io::Result<()> {
		self.syncer.sync_data(path)
	}

	/// 把一条大文件传输放进后台补同步队列。
	///
	/// 返回 `false` 表示队列已满或 worker 已退出——此时行上的 `durable=0` 还在，
	/// 30 s sweep 会兜底，接收路径不等待、不失败。
	pub fn enqueue_background_sync(&self, blob_sha256: String, cache_sha256: String) -> bool {
		self.durability
			.tx
			.try_send(PendingSync {
				blob_sha256,
				cache_sha256,
			})
			.is_ok()
	}

	/// 抢占唯一一次 worker 启动权并取走 receiver / stop 监听。
	///
	/// 返回 `None` 表示本 Core 的 worker 已经启动过（`Cas` clone 共享同一个
	/// `AtomicBool`），调用方直接返回即可。
	pub(crate) fn claim_durability_worker(
		&self,
	) -> Option<(mpsc::Receiver<PendingSync>, watch::Receiver<bool>)> {
		if self.durability.started.swap(true, Ordering::AcqRel) {
			return None;
		}
		let rx = self.durability.rx.lock().unwrap().take()?;
		Some((rx, self.durability.stop_tx.subscribe()))
	}

	/// Core 停机时唤醒后台线程退出（不 join：与 NetManager 一样是尽力而为）。
	pub fn stop_durability_worker(&self) {
		let _ = self.durability.stop_tx.send(true);
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

	/// M3: 将临时文件“转正”为 Blob（无 fsync，保持旧行为；本地 ingest 路径使用）。
	/// 1. 检查 sha256 是否匹配 (Actor 已校验，这里主要是移动文件)
	/// 2. 移动(rename)到 blobs 目录
	///
	/// 返回: 最终的 blob 路径
	pub fn commit_tmp_file(&self, tmp_path: &Path, sha256: &str) -> anyhow::Result<PathBuf> {
		self.commit_tmp_file_inner(tmp_path, sha256)
	}

	/// 实际的 rename 逻辑（带策略与不带策略的 commit 共用）。
	fn commit_tmp_file_inner(&self, tmp_path: &Path, sha256: &str) -> anyhow::Result<PathBuf> {
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

	/// 按 fsync 策略把临时文件转正。
	///
	/// 顺序契约（接收路径据此决定何时允许上报 `CONTENT_CACHED`）：
	/// - [`FsyncDecision::SyncBeforeCommit`]：先 `sync_all()`（目标已存在时同步最终
	///   blob），成功后才 rename；sync 失败直接返回错误，传输不得上报完成。
	/// - [`FsyncDecision::CommitThenSyncInBackground`]：只 rename；返回
	///   [`DurabilityOutcome::Pending`]，由调用方置 `durable=0` 并投递后台队列。
	/// - [`FsyncDecision::NoSync`]：只 rename，返回 [`DurabilityOutcome::NotSynced`]。
	pub fn commit_tmp_file_with_policy(
		&self,
		tmp_path: &Path,
		sha256: &str,
		decision: FsyncDecision,
	) -> anyhow::Result<CommitOutcome> {
		let durability = match decision {
			FsyncDecision::SyncBeforeCommit => {
				let dst = self.blob_path(sha256)?;
				// 目标已存在说明 rename 只做去重删除；此时真正要落盘的是最终 blob。
				let to_sync = if dst.exists() {
					dst.as_path()
				} else {
					tmp_path
				};
				let sync_result = self.sync_all_file(to_sync);
				if let Err(e) = sync_result {
					// fsync 失败等价于“无法保证已落盘”：commit 失败，并清掉 tmp
					// （writer Task 的 TempFileGuard 在提交前已放弃删除权）。
					let _ = fs::remove_file(tmp_path);
					return Err(anyhow::Error::new(e)).with_context(|| {
						format!("fsync before commit failed: {}", to_sync.display())
					});
				}
				DurabilityOutcome::Synced
			}
			FsyncDecision::CommitThenSyncInBackground => DurabilityOutcome::Pending,
			FsyncDecision::NoSync => DurabilityOutcome::NotSynced,
		};

		let blob_path = self.commit_tmp_file_inner(tmp_path, sha256)?;
		Ok(CommitOutcome {
			blob_path,
			durability,
		})
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

	/// 计数的同步实现：证明“小文件 commit 路径确实调用了 sync 步骤”，
	/// 不需要真实 socket，也不需要慢磁盘。
	#[derive(Debug, Default)]
	struct CountingSyncer {
		sync_all: std::sync::atomic::AtomicUsize,
		sync_data: std::sync::atomic::AtomicUsize,
	}

	impl FileSyncer for CountingSyncer {
		fn sync_all(&self, _path: &Path) -> std::io::Result<()> {
			self.sync_all.fetch_add(1, Ordering::Relaxed);
			Ok(())
		}
		fn sync_data(&self, _path: &Path) -> std::io::Result<()> {
			self.sync_data.fetch_add(1, Ordering::Relaxed);
			Ok(())
		}
	}

	#[test]
	fn fsync_policy_parsing_defaults_and_modes() {
		assert_eq!(
			FsyncPolicy::parse(None).unwrap(),
			FsyncPolicy::LargeOnly,
			"未设置时默认 large-only"
		);
		assert_eq!(
			FsyncPolicy::parse(Some("")).unwrap(),
			FsyncPolicy::LargeOnly
		);
		assert_eq!(
			FsyncPolicy::parse(Some(" ALWAYS ")).unwrap(),
			FsyncPolicy::Always
		);
		assert_eq!(
			FsyncPolicy::parse(Some("large-only")).unwrap(),
			FsyncPolicy::LargeOnly
		);
		assert_eq!(
			FsyncPolicy::parse(Some("never")).unwrap(),
			FsyncPolicy::Never
		);

		// 非法值必须报错，由 Core::init 记录警告后回退默认（不是 panic）。
		for bad in ["sometimes", "large", "0", "true", "-"] {
			assert!(
				FsyncPolicy::parse(Some(bad)).is_err(),
				"非法策略必须被拒绝: {bad:?}"
			);
		}
		// 首尾空白属于允许的归一化，不是非法值。
		assert_eq!(
			FsyncPolicy::parse(Some("  always  ")).unwrap(),
			FsyncPolicy::Always
		);
		assert_eq!(FsyncPolicy::default(), FsyncPolicy::LargeOnly);
	}

	#[test]
	fn fsync_decision_respects_8mib_boundary() {
		const MIB: u64 = 1024 * 1024;
		let threshold = LARGE_FILE_FSYNC_THRESHOLD_BYTES;
		assert_eq!(threshold, 8 * MIB);

		// always：任意大小都在 commit 前同步。
		for size in [
			0,
			1,
			threshold - 1,
			threshold,
			threshold + 1,
			4 * 1024 * MIB,
		] {
			assert_eq!(
				FsyncPolicy::Always.decision(size),
				FsyncDecision::SyncBeforeCommit
			);
		}

		// never：任意大小都不同步。
		for size in [0, 1, threshold - 1, threshold, threshold + 1] {
			assert_eq!(FsyncPolicy::Never.decision(size), FsyncDecision::NoSync);
		}

		// large-only：严格小于 8 MiB 同步；恰好 8 MiB 及更大走后台补同步。
		assert_eq!(
			FsyncPolicy::LargeOnly.decision(threshold - 1),
			FsyncDecision::SyncBeforeCommit,
			"just below 8 MiB 必须同步"
		);
		assert_eq!(
			FsyncPolicy::LargeOnly.decision(threshold),
			FsyncDecision::CommitThenSyncInBackground,
			"恰好 8 MiB 归入大文件"
		);
		assert_eq!(
			FsyncPolicy::LargeOnly.decision(threshold + 1),
			FsyncDecision::CommitThenSyncInBackground,
			"just above 8 MiB 走后台补同步"
		);
		assert_eq!(
			FsyncPolicy::LargeOnly.decision(0),
			FsyncDecision::SyncBeforeCommit
		);
	}

	fn counting_cas(dir: &Path, policy: FsyncPolicy) -> (Cas, Arc<CountingSyncer>) {
		let syncer = Arc::new(CountingSyncer::default());
		let cas = Cas::new_with_syncer(dir, policy, syncer.clone()).unwrap();
		(cas, syncer)
	}

	/// 小文件（large-only / always）的 commit 写路径必须在 rename 前调用一次
	/// `sync_all`，并且不进入后台 pending。
	#[test]
	fn small_file_commit_calls_sync_all_before_rename() {
		let dir = tempfile::tempdir().unwrap();
		let (cas, counter) = counting_cas(dir.path(), FsyncPolicy::LargeOnly);
		let sha = "d".repeat(64);
		let tmp = cas.get_tmp_path("transfer-small").unwrap();
		fs::write(&tmp, b"small payload").unwrap();

		let outcome = cas
			.commit_tmp_file_with_policy(&tmp, &sha, cas.fsync_policy().decision(12))
			.unwrap();

		assert_eq!(outcome.durability, DurabilityOutcome::Synced);
		assert!(outcome.blob_path.exists());
		assert!(!tmp.exists(), "tmp 必须在 rename 后消失");
		assert_eq!(
			counter.sync_all.load(Ordering::Relaxed),
			1,
			"小文件 commit 必须调用一次 sync_all"
		);
		assert_eq!(counter.sync_data.load(Ordering::Relaxed), 0);
	}

	/// 大文件（large-only ≥ 8 MiB）的 commit 路径：rename 前不 sync，
	/// 结果标记为 Pending，由调用方置 durable=0 并投递后台队列。
	#[test]
	fn large_file_commit_skips_sync_and_marks_pending() {
		let dir = tempfile::tempdir().unwrap();
		let (cas, counter) = counting_cas(dir.path(), FsyncPolicy::LargeOnly);
		let sha = "e".repeat(64);
		let tmp = cas.get_tmp_path("transfer-large").unwrap();
		fs::write(&tmp, b"large payload").unwrap();

		let outcome = cas
			.commit_tmp_file_with_policy(
				&tmp,
				&sha,
				cas.fsync_policy()
					.decision(LARGE_FILE_FSYNC_THRESHOLD_BYTES),
			)
			.unwrap();

		assert_eq!(outcome.durability, DurabilityOutcome::Pending);
		assert!(outcome.blob_path.exists());
		assert_eq!(
			counter.sync_all.load(Ordering::Relaxed),
			0,
			"大文件 rename 前不得同步 fsync"
		);
		assert_eq!(counter.sync_data.load(Ordering::Relaxed), 0);
	}

	/// always 策略对小文件同样先 sync；never 策略一次 fsync 都不做。
	#[test]
	fn always_and_never_policies_drive_commit_behavior() {
		let dir = tempfile::tempdir().unwrap();

		{
			let (cas, counter) = counting_cas(&dir.path().join("always"), FsyncPolicy::Always);
			let sha = "f".repeat(64);
			let tmp = cas.get_tmp_path("t-always").unwrap();
			fs::write(&tmp, b"x").unwrap();
			let outcome = cas
				.commit_tmp_file_with_policy(&tmp, &sha, FsyncDecision::SyncBeforeCommit)
				.unwrap();
			assert_eq!(outcome.durability, DurabilityOutcome::Synced);
			assert_eq!(counter.sync_all.load(Ordering::Relaxed), 1);
		}

		{
			let (cas, counter) = counting_cas(&dir.path().join("never"), FsyncPolicy::Never);
			let sha = "0".repeat(64);
			let tmp = cas.get_tmp_path("t-never").unwrap();
			fs::write(&tmp, b"x").unwrap();
			let decision = cas.fsync_policy().decision(8 * 1024 * 1024);
			assert_eq!(decision, FsyncDecision::NoSync);
			let outcome = cas
				.commit_tmp_file_with_policy(&tmp, &sha, decision)
				.unwrap();
			assert_eq!(outcome.durability, DurabilityOutcome::NotSynced);
			assert_eq!(counter.sync_all.load(Ordering::Relaxed), 0);
			assert_eq!(counter.sync_data.load(Ordering::Relaxed), 0);
		}
	}

	/// sync_all 失败时 commit 必须失败（绝不能上报“已完成”），tmp 由调用方清理。
	#[test]
	fn sync_failure_aborts_commit() {
		#[derive(Debug)]
		struct FailingSyncer;
		impl FileSyncer for FailingSyncer {
			fn sync_all(&self, _path: &Path) -> std::io::Result<()> {
				Err(std::io::Error::other("simulated fsync failure"))
			}
			fn sync_data(&self, _path: &Path) -> std::io::Result<()> {
				Err(std::io::Error::other("simulated fdatasync failure"))
			}
		}
		let dir = tempfile::tempdir().unwrap();
		let cas = Cas::new_with_syncer(dir.path(), FsyncPolicy::LargeOnly, Arc::new(FailingSyncer))
			.unwrap();
		let sha = "1".repeat(64);
		let tmp = cas.get_tmp_path("t-fail").unwrap();
		fs::write(&tmp, b"payload").unwrap();

		let err = cas
			.commit_tmp_file_with_policy(&tmp, &sha, FsyncDecision::SyncBeforeCommit)
			.unwrap_err();
		assert!(err.to_string().contains("fsync before commit failed"));
		assert!(!cas.blob_exists(&sha), "sync 失败时不得把 blob 转正");
		assert!(!tmp.exists(), "sync 失败时必须清掉 tmp，避免泄漏");
	}

	/// 后台队列：容量之外返回 false（接收路径不回滚、不阻塞，交给 sweep）。
	#[test]
	fn durability_queue_is_bounded_and_reports_full() {
		let dir = tempfile::tempdir().unwrap();
		let cas = Cas::new(dir.path()).unwrap();
		let mut accepted = 0;
		for _ in 0..DURABILITY_QUEUE_CAPACITY {
			if cas.enqueue_background_sync("a".repeat(64), "b".repeat(64)) {
				accepted += 1;
			}
		}
		assert_eq!(accepted, DURABILITY_QUEUE_CAPACITY);
		assert!(
			!cas.enqueue_background_sync("a".repeat(64), "b".repeat(64)),
			"队列满时必须返回 false，让调用方依赖 sweep"
		);
	}
}
