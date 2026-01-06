package com.ryan416.clipbridgeshellandroid.clipboard;

import android.content.ClipData;
import android.content.Context;
import android.content.pm.PackageManager;
import android.os.Handler;
import android.os.Looper;

import android.content.IOnPrimaryClipChangedListener;
import android.util.Log;

import com.ryan416.clipbridgeshellandroid.core.CoreInterop;
import com.ryan416.clipbridgeshellandroid.shizuku.ShizukuClipboardManager;

import org.json.JSONObject;

import java.io.BufferedReader;
import java.io.InputStreamReader;
import java.util.concurrent.Executors;
import java.util.concurrent.ScheduledExecutorService;
import java.util.concurrent.TimeUnit;

import rikka.shizuku.Shizuku;

public class ClipboardWatcher {
	private static final String TAG = "CB.ShizukuWatcher";
	private final Context context;
	private final CoreInterop core;
	private String lastIngestedText = "";

	//  用于切主线程
	private final Handler mainHandler = new Handler(Looper.getMainLooper());

	private final android.content.IOnPrimaryClipChangedListener.Stub listener = new android.content.IOnPrimaryClipChangedListener.Stub() {
		@Override
		public void dispatchPrimaryClipChanged() {
			Log.d(TAG, "Shizuku callback: Clipboard Changed!");
			// AIDL 回调在 Binder 线程，切到主线程处理
			mainHandler.post(() -> readClipboard());
		}
	};


	public ClipboardWatcher(Context context, CoreInterop core) {
		this.context = context;
		this.core = core;
	}

	public void start() {
		Log.i(TAG, "Registering Shizuku Listener...");
		// 注册监听
		ShizukuClipboardManager.addPrimaryClipChangedListener(listener, context.getPackageName());

		// 启动时先读一次，防止漏掉启动前的复制
		mainHandler.post(() -> readClipboard());
	}
	public void stop() {
		Log.i(TAG, "Unregistering Shizuku Listener...");
		ShizukuClipboardManager.removePrimaryClipChangedListener(listener, context.getPackageName());
	}

	private void readClipboard() {
		try {
			ClipData clip = ShizukuClipboardManager.getPrimaryClip(context.getPackageName());
			if (clip == null || clip.getItemCount() == 0) return;

			CharSequence cs = clip.getItemAt(0).coerceToText(context);
			if (cs == null) return;

			String currentText = cs.toString();

			if (currentText.isEmpty()) return;
			if (currentText.equals(lastIngestedText)) return;

			Log.i(TAG, "Ingesting text detected by Shizuku Listener. Len: " + currentText.length());

			// 此时我们在主线程，调用 Core 应该是安全的
			ingestText(currentText);

			lastIngestedText = currentText;

		} catch (Exception e) {
			Log.e(TAG, "Error reading clipboard via Shizuku", e);
		}
	}

	// 供 ApplyService 调用，用于防止回环
	public void markNextChangeSuppressed(String text) {
		// 简单处理：直接更新 lastIngestedText，这样下次 check 就会认为重复
		this.lastIngestedText = text;
	}

	private void ingestText(String text) {
		try {
			JSONObject snap = new JSONObject();
			snap.put("type", "ClipboardSnapshot");
			snap.put("ts_ms", System.currentTimeMillis());
			snap.put("share_mode", "local_only");
			snap.put("kind", "text");
			JSONObject textObj = new JSONObject();
			textObj.put("mime", "text/plain");
			textObj.put("utf8", text);
			snap.put("text", textObj);

			if (core == null || !core.isReady() || core.getHandlePtr() == 0) {
				Log.w(TAG, "Core not ready, skipping ingest.");
				return;
			}
			Log.d(TAG, "Calling nativeIngestLocalCopy...");
			String result = core.ingestLocalCopy(snap.toString());
			Log.i(TAG, "Core ingest result: " + result);
		} catch (Throwable e) {
			Log.e(TAG, "Ingest failed (Java side)", e);
		}
	}
}
