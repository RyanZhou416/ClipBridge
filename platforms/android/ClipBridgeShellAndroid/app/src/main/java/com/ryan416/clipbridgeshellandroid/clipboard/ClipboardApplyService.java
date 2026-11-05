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
import java.util.concurrent.TimeoutException;

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
			// FFI 契约（crates/cb_core_ffi）：请求是 {"item_id":"..."}，
			// 响应是 {"ok":true,"data":{"transfer_id":...}}；不能把裸 item_id 当请求，
			// 也不能把整个 envelope 字符串当 transfer_id。
			JSONObject req = new JSONObject();
			req.put("item_id", itemId);
			String reqJson = req.toString();
			Log.d(TAG, "ensureContentCached request: " + reqJson);

			String envJson = core.ensureContentCached(reqJson);
			Log.d(TAG, "ensureContentCached response: " + envJson);

			JSONObject env = new JSONObject(envJson);
			if (!env.optBoolean("ok", false)) {
				JSONObject error = env.optJSONObject("error");
				String code = error != null ? error.optString("code", "UNKNOWN") : "UNKNOWN";
				String message = error != null ? error.optString("message", envJson) : envJson;
				Log.e(TAG, "ensureContentCached failed. itemId=" + itemId
					+ " code=" + code + " message=" + message);
				return;
			}

			JSONObject data = env.optJSONObject("data");
			String transferId = data != null ? data.optString("transfer_id", null) : null;
			if (transferId == null || transferId.isEmpty()) {
				Log.w(TAG, "ensureContentCached returned no transfer_id. itemId=" + itemId + " env=" + envJson);
				return;
			}

			// 带超时等待 CONTENT_CACHED / TRANSFER_FAILED，避免单线程 executor 被永久阻塞
			ContentFetchAwaiter.Result r = awaiter.awaitResult(transferId);

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
			// watcher 必须由 ForegroundService 先注入；若仍为空则显式报错，不要再让 NPE 被 catch(Throwable) 吞掉
			if (watcher == null) {
				Log.e(TAG, "ClipboardWatcher 未初始化，无法抑制回环，放弃本次应用. itemId=" + itemId);
				return;
			}
			watcher.markNextChangeSuppressed(text);
			ClipData clip = ClipData.newPlainText("ClipBridge", text);

			boolean success = ShizukuClipboardManager.setPrimaryClip(clip, appCtx.getPackageName());

			if (success) {
				Log.i(TAG, "Applied to system clipboard via Shizuku. len=" + text.length());
			} else {
				Log.e(TAG, "Failed to apply via Shizuku (Binder Error?)");
			}
		} catch (TimeoutException te) {
			// 超时是正常失败路径：记录后释放 executor，不再无限阻塞
			Log.e(TAG, "等待内容超时，放弃本次应用. itemId=" + itemId + " detail=" + te.getMessage());
		} catch (InterruptedException ie) {
			Thread.currentThread().interrupt();
			Log.e(TAG, "等待内容被中断，放弃本次应用. itemId=" + itemId, ie);
		} catch (Throwable t) {
			// 带上 itemId，便于在 logcat 里定位是哪一条内容失败
			Log.e(TAG, "applyItemToClipboard failed. itemId=" + itemId, t);
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
