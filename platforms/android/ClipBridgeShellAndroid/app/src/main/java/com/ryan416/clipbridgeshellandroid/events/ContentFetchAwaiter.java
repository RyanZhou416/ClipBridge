package com.ryan416.clipbridgeshellandroid.events;

import java.util.concurrent.CompletableFuture;
import java.util.concurrent.ConcurrentHashMap;

public class ContentFetchAwaiter {

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

	public CompletableFuture<Result> await(String transferId) {
		return map.computeIfAbsent(transferId, k -> new CompletableFuture<>());
	}

	public void complete(Result r) {
		CompletableFuture<Result> f = map.remove(r.transferId);
		if (f != null) f.complete(r);
	}

	public void fail(String transferId, String reason) {
		CompletableFuture<Result> f = map.remove(transferId);
		if (f != null) f.completeExceptionally(new RuntimeException(reason));
	}

	public void clear() {
		map.clear();
	}
}
