package com.ryan416.clipbridgeshellandroid.clipboard;

import android.content.ClipData;
import android.content.ClipboardManager;
import android.content.Context;
import android.util.Log;
import org.json.JSONObject;

public class ClipboardWatcher {
	private static final String TAG = "CB.ClipboardWatcher";

	private final ClipboardManager clipboard;
	private ClipboardManager.OnPrimaryClipChangedListener listener;

	// 用于简单防回环
	private volatile boolean suppressNext = false;
	private volatile String lastAppliedText = null;
	private volatile long lastAppliedAtMs = 0;

	public ClipboardWatcher(Context ctx) {
		this.clipboard = (ClipboardManager) ctx.getSystemService(Context.CLIPBOARD_SERVICE);
	}

	public void start() {
		if (listener != null) return;

		listener = () -> {
			try {
				if (clipboard == null) return;

				ClipData data = clipboard.getPrimaryClip();
				if (data == null || data.getItemCount() <= 0) return;

				CharSequence cs = data.getItemAt(0).coerceToText(null);
				if (cs == null) return;

				String text = cs.toString();

				// 防回环：吞掉一次或吞掉同内容
				long now = System.currentTimeMillis();
				if (suppressNext && (now - lastAppliedAtMs) < 1500) {
					Log.i(TAG, "Suppressed clipboard change (recent apply).");
					suppressNext = false;
					return;
				}
				if (lastAppliedText != null && lastAppliedText.equals(text) && (now - lastAppliedAtMs) < 1500) {
					Log.i(TAG, "Suppressed clipboard change (same text).");
					return;
				}

				Log.i(TAG, "Clipboard changed: len=" + text.length());

				JSONObject snap = new JSONObject();
				snap.put("type", "ClipboardSnapshot");
				snap.put("ts_ms", System.currentTimeMillis());
				snap.put("kind", "text");
				snap.put("share_mode", "default");

				JSONObject textObj = new JSONObject();
				textObj.put("mime", "text/plain");
				textObj.put("utf8", text);
				snap.put("text", textObj);

				Log.i(TAG, "Snapshot JSON = " + snap.toString());

			} catch (Throwable t) {
				Log.e(TAG, "Clipboard listener error", t);
			}
		};

		clipboard.addPrimaryClipChangedListener(listener);
		Log.i(TAG, "Started.");
	}

	public void stop() {
		if (clipboard != null && listener != null) {
			clipboard.removePrimaryClipChangedListener(listener);
			listener = null;
		}
		Log.i(TAG, "Stopped.");
	}

	/** 供 ApplyService 写回剪贴板前调用 */
	public void markNextChangeSuppressed(String appliedText) {
		suppressNext = true;
		lastAppliedText = appliedText;
		lastAppliedAtMs = System.currentTimeMillis();
	}
}
