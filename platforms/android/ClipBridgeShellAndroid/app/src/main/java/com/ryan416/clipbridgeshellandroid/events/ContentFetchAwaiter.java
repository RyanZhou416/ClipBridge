package com.ryan416.clipbridgeshellandroid.events;

import java.util.concurrent.CompletableFuture;
import java.util.concurrent.ConcurrentHashMap;
import java.util.concurrent.ExecutionException;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.TimeoutException;

public class ContentFetchAwaiter {

	/**
	 * 内容获取超时（10s），与 Windows 外壳 ContentFetchAwaiter.Timeout 保持一致。
	 * Core 可能在 CONTENT_CACHED / TRANSFER_FAILED 之前因网络问题长期静默，
	 * 无限等待会让单线程 executor 永久卡死。
	 */
	public static final long DEFAULT_TIMEOUT_MS = 10_000L;

	public static class Result {
		public final String transferId;
		public final String textUtf8;   // 可能为空
		public final String localPath;  // 可能为空
		public final String mime;       // 可能为空

		public Result(String transferId, String textUtf8, String localPath, String mime) {
			this.transferId = transferId;
			this.textUtf8 = textUtf8;
			this.localPath = localPath;
			this.mime = mime;
		}
	}

	private final ConcurrentHashMap<String, CompletableFuture<Result>> map = new ConcurrentHashMap<>();
	// 早到缓存：CONTENT_CACHED / TRANSFER_FAILED 可能先于 await() 注册到达，
	// 没有这项缓存时该终态会丢失，之后的等待将永远挂起（与 Windows 外壳一致）。
	private final ConcurrentHashMap<String, Result> pendingLocal = new ConcurrentHashMap<>();
	private final ConcurrentHashMap<String, RuntimeException> pendingFailures = new ConcurrentHashMap<>();

	public CompletableFuture<Result> await(String transferId) {
		CompletableFuture<Result> f = map.computeIfAbsent(transferId, k -> new CompletableFuture<>());

		// 先注册等待者、再检查缓存：complete/fail 无论发生在注册前还是注册后都不会丢失
		RuntimeException earlyFailure = pendingFailures.remove(transferId);
		if (earlyFailure != null) {
			if (map.remove(transferId, f)) f.completeExceptionally(earlyFailure);
			return f;
		}

		Result earlyLocal = pendingLocal.remove(transferId);
		if (earlyLocal != null) {
			if (map.remove(transferId, f)) f.complete(earlyLocal);
			return f;
		}

		return f;
	}

	/** 带超时的阻塞等待；超时会移除等待者，避免 map 泄漏。 */
	public Result awaitResult(String transferId, long timeoutMs)
		throws InterruptedException, ExecutionException, TimeoutException {
		CompletableFuture<Result> f = await(transferId);
		try {
			return f.get(timeoutMs, TimeUnit.MILLISECONDS);
		} catch (TimeoutException e) {
			map.remove(transferId, f);
			throw new TimeoutException(
				"content fetch timed out after " + timeoutMs + "ms (transfer_id=" + transferId + ")");
		}
	}

	public Result awaitResult(String transferId)
		throws InterruptedException, ExecutionException, TimeoutException {
		return awaitResult(transferId, DEFAULT_TIMEOUT_MS);
	}

	public void complete(Result r) {
		CompletableFuture<Result> f = map.remove(r.transferId);
		if (f != null) {
			pendingFailures.remove(r.transferId);
			f.complete(r);
			return;
		}

		// 没有等待者：缓存成功结果（同时清掉更早的失败，保持“最后一次终态”语义）
		pendingFailures.remove(r.transferId);
		pendingLocal.put(r.transferId, r);
	}

	public void fail(String transferId, String reason) {
		RuntimeException ex = new RuntimeException(reason);

		CompletableFuture<Result> f = map.remove(transferId);
		if (f != null) {
			pendingLocal.remove(transferId);
			f.completeExceptionally(ex);
			return;
		}

		// 没有等待者：缓存失败，留给下一次 awaitResult 立即抛出（否则失败事件丢失）
		pendingLocal.remove(transferId);
		pendingFailures.put(transferId, ex);
	}

	public void clear() {
		for (String transferId : map.keySet()) {
			CompletableFuture<Result> f = map.remove(transferId);
			if (f != null) f.completeExceptionally(new RuntimeException("awaiter cleared"));
		}
		pendingLocal.clear();
		pendingFailures.clear();
	}
}
