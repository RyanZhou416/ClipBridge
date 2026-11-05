//! `cb_handle` 生命周期、重入守卫与幂等 shutdown 语义。

use cb_core::api::{Core, CoreEventSink};
use std::ffi::CString;
use std::os::raw::{c_char, c_void};
use std::sync::atomic::{AtomicBool, Ordering};

/// 包装 `cb_core::api::Core` 的 C 兼容句柄。
#[repr(C)]
pub struct cb_handle {
	/// 句柄是否仍然存活；`cb_shutdown` 会以原子方式置为 `false`。
	alive: AtomicBool,
	/// 廉价的“调用中”标记：用于把并发重入转成 `CORE_BUSY`，而不是别名 UB。
	busy: AtomicBool,
	/// 核心实例；`cb_shutdown` 时被 `take()` 出来执行关闭。
	core: Option<Core>,
}

impl cb_handle {
	/// 为 `Core` 构造一个存活、空闲的句柄。
	pub(crate) fn new(core: Core) -> Self {
		Self {
			alive: AtomicBool::new(true),
			busy: AtomicBool::new(false),
			core: Some(core),
		}
	}

	/// 通过共享引用读取存活标记（必须在任何 `&mut *h` 之前调用）。
	fn is_alive(&self) -> bool {
		self.alive.load(Ordering::Acquire)
	}

	/// 取得 `Core` 的可变引用；句柄已关闭时返回 `CORE_SHUTDOWN` 语义的错误。
	pub(crate) fn core_mut(&mut self) -> anyhow::Result<&mut Core> {
		if !self.is_alive() {
			return Err(anyhow::Error::new(HandleError::Shutdown));
		}
		self.core
			.as_mut()
			.ok_or_else(|| anyhow::Error::new(HandleError::Shutdown))
	}
}

/// 壳侧提供的事件回调函数原型。
pub type OnEventFn = extern "C" fn(json: *const c_char, user_data: *mut c_void);

/// 内部结构，用于将 FFI 回调和用户数据转发给核心层。
pub(crate) struct FfiSink {
	pub(crate) cb: OnEventFn,
	pub(crate) user: *mut c_void,
}

// function pointer + raw pointer：我们保证只“转发调用”，线程安全由壳侧处理
unsafe impl Send for FfiSink {}
unsafe impl Sync for FfiSink {}

impl CoreEventSink for FfiSink {
	fn emit(&self, event_json: String) {
		let Ok(cstr) = CString::new(event_json) else {
			return;
		};
		(self.cb)(cstr.as_ptr(), self.user);
		// cstr 释放后指针失效；壳侧必须在回调里拷贝
	}
}

/// FFI 句柄生命周期错误；`error_code` 会把它们映射为稳定的错误码。
#[derive(Debug)]
pub(crate) enum HandleError {
	/// 句柄已经关闭。
	Shutdown,
	/// 同一句柄上检测到并发/重入调用。
	Busy,
}

impl std::fmt::Display for HandleError {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		match self {
			HandleError::Shutdown => write!(f, "core handle has been shut down"),
			HandleError::Busy => write!(
				f,
				"core handle is busy: concurrent calls are not supported, serialize calls on the shell side"
			),
		}
	}
}

impl std::error::Error for HandleError {}

/// 入口守卫：持有期间 `busy` 为 `true`，`Drop`（包括 panic 展开）时释放。
///
/// 只保存裸指针（而不是 `&cb_handle`），避免在随后创建 `&mut *h` 时产生别名冲突。
pub(crate) struct CallGuard {
	h: *mut cb_handle,
}

impl Drop for CallGuard {
	fn drop(&mut self) {
		// SAFETY: shutdown 时句柄盒被故意泄漏（见 shutdown），因此指针始终有效；
		// 此时本函数内的 `&mut *h` 借用已经结束。
		unsafe {
			(*self.h).busy.store(false, Ordering::Release);
		}
	}
}

/// 所有带句柄入口的第一步：占住重入标记，并在任何 `&mut *h` 之前检查 `alive`。
pub(crate) fn enter(h: *mut cb_handle) -> anyhow::Result<CallGuard> {
	if h.is_null() {
		anyhow::bail!("null handle");
	}
	// SAFETY: 调用者保证 h 是 cb_init 返回且未被释放的句柄。这里只读取原子标记。
	if unsafe {
		(*h).busy
			.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
			.is_err()
	} {
		return Err(anyhow::Error::new(HandleError::Busy));
	}
	// 守卫负责在返回/panic 时把 busy 复位。
	let guard = CallGuard { h };
	// 共享读取存活标记；必须在任何 `&mut *h` 之前完成，且不跨越后续借用。
	let alive = unsafe { (&*h).is_alive() };
	if !alive {
		return Err(anyhow::Error::new(HandleError::Shutdown));
	}
	Ok(guard)
}

/// 幂等关闭：原子地置 `alive = false`，`take()` 出 `Core` 并 `shutdown()`。
///
/// **不** 调用 `Box::from_raw(h)`：句柄盒被故意泄漏。它很小且结构固定，用这点有界
/// 泄漏换取“shutdown 之后迟到的调用仍然可以安全解引用并得到 `CORE_SHUTDOWN`”的保障。
/// 若真要回收，需要额外的跨线程生命周期协议（Mutex/Arc 设计），不在本次范围内。
///
/// 与普通入口相同的重入保护：避免 shutdown 与正在进行的调用并发执行；句柄可能在
/// 调用进行中被关闭，因此这里有限次重试 `busy` 而不是立即放弃。
pub(crate) fn shutdown(h: *mut cb_handle) -> anyhow::Result<()> {
	if h.is_null() {
		anyhow::bail!("null handle");
	}
	let mut acquired = false;
	for _ in 0..500 {
		if unsafe {
			(*h).busy
				.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
				.is_ok()
		} {
			acquired = true;
			break;
		}
		std::thread::yield_now();
	}
	if !acquired {
		return Err(anyhow::Error::new(HandleError::Busy));
	}
	let _guard = CallGuard { h };

	// 幂等：第二次及以后的调用直接返回成功。
	if !unsafe { (*h).alive.load(Ordering::Acquire) } {
		return Ok(());
	}

	// 先置 false：此后任何入口都会拿到 CORE_SHUTDOWN（而不是 UAF）。
	unsafe {
		(*h).alive.store(false, Ordering::Release);
	}

	let core = unsafe { (*h).core.take() };
	if let Some(core) = core {
		core.shutdown();
	}
	Ok(())
}
