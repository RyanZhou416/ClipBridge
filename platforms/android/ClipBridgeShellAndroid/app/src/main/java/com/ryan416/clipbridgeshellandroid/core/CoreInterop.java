package com.ryan416.clipbridgeshellandroid.core;

import android.util.Log;

import org.json.JSONObject;

public class CoreInterop {
	static {
		// 先 load Rust 的 core-ffi，再 load JNI shim
		System.loadLibrary("core_ffi_android");
		System.loadLibrary("cb_jni");
	}

	public interface CoreEventListener {
		void onCoreEvent(String json);
	}

	// Core handle（与 Windows 类似）
	private long handlePtr = 0;

	public synchronized String init(String cfgJson, CoreEventListener listener) {
		String env = nativeInit(cfgJson, listener);

		try {
			JSONObject root = new JSONObject(env);
			if (!root.optBoolean("ok", false)) return env;

			// data 应该是一个对象 {"handle": 123...}
			JSONObject data = root.optJSONObject("data");
			if (data != null) {
				// 获取 handle 的字符串形式，防止 JSON 库自动转成截断的 Long
				// 注意：如果 org.json 已经在内部解析成了 Number，toString 可能会拿到科学计数法或截断值
				// 最稳妥的方法是让 Rust 返回 String 类型的 handle，但在不改 Rust 的情况下：
				// 我们尝试读取 String，如果读不到再尝试读 Long

				long h = 0;
				Object rawHandle = data.opt("handle");

				if (rawHandle instanceof String) {
					h = Long.parseUnsignedLong((String) rawHandle);
				} else if (rawHandle instanceof Number) {
					// 如果已经是 Number，直接强转可能会溢出，但通常 JSON 库会存为 BigInteger 或 Long
					// 我们这里做一个特殊的处理：利用 String 转换
					h = Long.parseUnsignedLong(String.valueOf(rawHandle));
				}

				this.handlePtr = h;
				android.util.Log.i("CB.Interop", "Handle parsed successfully: " + Long.toUnsignedString(h));
			}
		} catch (Exception e) {
			android.util.Log.e("CB.Interop", "Failed to parse handle", e);
		}
		return env;
	}

	public synchronized String shutdown() {
		if (handlePtr == 0) return "{\"ok\":true,\"data\":null}";
		String env = nativeShutdown(handlePtr);
		handlePtr = 0;
		return env;
	}

	public synchronized long getHandlePtr() {
		return handlePtr;
	}

	public synchronized boolean isReady() {
		return handlePtr != 0;
	}

	public synchronized String planLocalIngest(String snapshotJson) {
		if (handlePtr == 0) throw new IllegalStateException("core not initialized");
		return nativePlanLocalIngest(handlePtr, snapshotJson);
	}

	public synchronized String ingestLocalCopy(String snapshotJson) {
		if (handlePtr == 0) throw new IllegalStateException("core not initialized");
		return nativeIngestLocalCopy(handlePtr, snapshotJson);
	}

	public synchronized String listPeers() {
		if (handlePtr == 0) throw new IllegalStateException("core not initialized");
		return nativeListPeers(handlePtr);
	}

	public synchronized String getStatus() {
		if (handlePtr == 0) throw new IllegalStateException("core not initialized");
		return nativeGetStatus(handlePtr);
	}

	public synchronized String ensureContentCached(String reqJson) {
		if (handlePtr == 0) throw new IllegalStateException("core not initialized");
		return nativeEnsureContentCached(handlePtr, reqJson);
	}

	public synchronized String cancelTransfer(String transferIdJson) {
		if (handlePtr == 0) throw new IllegalStateException("core not initialized");
		return nativeCancelTransfer(handlePtr, transferIdJson);
	}

	public synchronized String listHistory(String queryJson) {
		if (handlePtr == 0) throw new IllegalStateException("core not initialized");
		return nativeListHistory(handlePtr, queryJson);
	}

	public synchronized String getItemMeta(String itemIdJson) {
		if (handlePtr == 0) throw new IllegalStateException("core not initialized");
		return nativeGetItemMeta(handlePtr, itemIdJson);
	}

	// --- native methods ---
	private static native String nativeInit(String cfgJson, CoreEventListener listener);

	private static native String nativeShutdown(long handlePtr);

	private static native String nativePlanLocalIngest(long handlePtr, String snapshotJson);

	private static native String nativeIngestLocalCopy(long handlePtr, String snapshotJson);

	private static native String nativeListPeers(long handlePtr);

	private static native String nativeGetStatus(long handlePtr);

	private static native String nativeEnsureContentCached(long handlePtr, String reqJson);

	private static native String nativeCancelTransfer(long handlePtr, String transferIdJson);

	private static native String nativeListHistory(long handlePtr, String queryJson);

	private static native String nativeGetItemMeta(long handlePtr, String itemIdJson);
}
