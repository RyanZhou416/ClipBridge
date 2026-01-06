package com.ryan416.clipbridgeshellandroid.clipboard;

import android.content.ClipData;
import android.content.ClipboardManager;
import android.content.Context;
import android.util.Log;


import com.ryan416.clipbridgeshellandroid.core.CoreInterop;
import com.ryan416.clipbridgeshellandroid.events.ContentFetchAwaiter;

import org.json.JSONObject;

import java.io.ByteArrayOutputStream;
import java.io.FileInputStream;
import java.nio.charset.StandardCharsets;
import java.util.concurrent.ExecutorService;
import java.util.concurrent.Executors;

import com.ryan416.clipbridgeshellandroid.shizuku.ShizukuClipboardManager;

public class ClipboardApplyService {
	private static final String TAG = "CB.ApplyService";

	private final Context appCtx;
	private final ClipboardWatcher watcher; // 用于 suppressNext
	private final CoreInterop core;
	private final ContentFetchAwaiter awaiter;

	private final ExecutorService exec = Executors.newSingleThreadExecutor();

	public ClipboardApplyService(Context ctx, ClipboardWatcher watcher, CoreInterop core, ContentFetchAwaiter awaiter) {
		this.appCtx = ctx.getApplicationContext();
		this.watcher = watcher;
		this.core = core;
		this.awaiter = awaiter;
	}

	/** 路由器回调：收到 meta 后触发拉取并应用 */
	public void onItemMeta(JSONObject meta) {
		// 只做最小闭环：拿 item_id
		final String itemId = meta.optString("item_id", null);
		if (itemId == null) {
			Log.w(TAG, "meta missing item_id");
			return;
		}

		// 你现在目标是 text；但这里不强制 kind==text（避免未来扩展时不兼容）
		exec.submit(() -> applyItemToClipboard(itemId));
	}

	private void applyItemToClipboard(String itemId) {
		try {
			// 关键：这里不构造 JSON 请求（避免我编造格式）
			// 直接调用 core.ensureContentCached(itemId, null) 让 native 层按真实 core API 调用。
			String transferId = core.ensureContentCached(itemId);
			if (transferId == null || transferId.isEmpty()) {
				Log.w(TAG, "ensureContentCached returned empty transferId");
				return;
			}

			ContentFetchAwaiter.Result r = awaiter.await(transferId).get(); // 阻塞等事件完成

			String text = null;
			if (r.textUtf8 != null && !r.textUtf8.isEmpty()) {
				text = r.textUtf8;
			} else if (r.localPath != null && !r.localPath.isEmpty()) {
				text = readUtf8File(r.localPath);
			} else {
				Log.w(TAG, "No text_utf8 or local_path in local_ref. transferId=" + transferId);
				return;
			}

			// 写入系统剪贴板 + 防回环
			watcher.markNextChangeSuppressed(text);
			ClipData clip = ClipData.newPlainText("ClipBridge", text);

			boolean success = ShizukuClipboardManager.setPrimaryClip(clip, appCtx.getPackageName());

			if (success) {
				Log.i(TAG, "Applied to system clipboard via Shizuku. len=" + text.length());
			} else {
				Log.e(TAG, "Failed to apply via Shizuku (Binder Error?)");
			}
		} catch (Throwable t) {
			Log.e(TAG, "applyItemToClipboard failed", t);
		}
	}

	private static String readUtf8File(String path) throws Exception {
		try (FileInputStream fis = new FileInputStream(path);
			 ByteArrayOutputStream bos = new ByteArrayOutputStream()) {
			byte[] buf = new byte[8192];
			int n;
			while ((n = fis.read(buf)) > 0) bos.write(buf, 0, n);
			return bos.toString(StandardCharsets.UTF_8.name());
		}
	}

	public void shutdown() {
		exec.shutdownNow();
	}
}
