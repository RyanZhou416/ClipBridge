package com.ryan416.clipbridgeshellandroid.core;

import android.util.Log;

public final class CoreNative {

	public CoreNative() {
		try {
			System.loadLibrary("core_ffi_android");
			Log.i("CoreNative", "libcore_ffi_android loaded successfully");
		} catch (Throwable t) {
			Log.e("CoreNative", "Failed to load core_ffi_android", t);
			throw t;
		}

	}
}
