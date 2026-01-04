package com.ryan416.clipbridgeshellandroid.core;

public class CoreInterop {
	static {
		// TODO: 等你放入 libclipbridge_core.so 后再打开
		// System.loadLibrary("clipbridge_core");
	}

	// Core handle（与 Windows 类似）
	private long handle = 0;

	public long getHandle() { return handle; }

	public void init(String configJson, CoreEventSink sink) {
		// TODO: native init -> handle
		// handle = nativeInit(configJson, sink);
		throw new UnsupportedOperationException("Core not wired yet");
	}

	public void shutdown() {
		// TODO nativeShutdown(handle)
		handle = 0;
	}

	public String ingestLocalCopy(String snapshotJson) {
		// TODO nativeIngestLocalCopy(handle, snapshotJson)
		throw new UnsupportedOperationException("Core not wired yet");
	}

	public String ensureContentCached(String reqJson) {
		// TODO nativeEnsureContentCached(handle, reqJson)
		throw new UnsupportedOperationException("Core not wired yet");
	}

	// private native long nativeInit(String configJson, CoreEventSink sink);
	// private native void nativeShutdown(long handle);
	// private native String nativeIngestLocalCopy(long handle, String snapshotJson);
	// private native String nativeEnsureContentCached(long handle, String reqJson);
}
