package com.ryan416.clipbridgeshellandroid.events;

import android.util.Log;

import org.json.JSONObject;

public class CoreEventRouter implements EventPump.Handler {
	private static final String TAG = "CB.EventRouter";

	public interface MetaListener {
		void onItemMeta(JSONObject meta);
	}

	private final ContentFetchAwaiter awaiter;
	private final MetaListener metaListener;

	public CoreEventRouter(ContentFetchAwaiter awaiter, MetaListener metaListener) {
		this.awaiter = awaiter;
		this.metaListener = metaListener;
	}

	@Override
	public void handle(JSONObject env) {
		// Windows 外壳: type 在 envelope 顶层；payload 在 envelope.payload
		String type = env.optString("type", "");
		JSONObject payload = env.optJSONObject("payload");
		if (payload == null) payload = new JSONObject();

		switch (type) {
			// Windows 外壳把这三个都当“History meta”
			case "ITEM_META_ADDED":
			case "ITEM_ADDED":
			case "ITEM_UPDATED": {
				JSONObject meta = env.optJSONObject("meta");

				// 2. 如果顶层没有，再尝试从 payload 里找
				if (meta == null) {
					meta = tryPickMeta(payload);
				}

				if (meta != null) {
					metaListener.onItemMeta(meta);
				} else {
					// 打印完整 env 以便调试
					Log.w(TAG, "Meta event but no meta found. Env: " + env.toString());
				}
				break;
			}

			case "CONTENT_CACHED": {
				// Windows 外壳: payload.transfer_id + payload.local_ref
				String transferId = payload.optString("transfer_id", null);
				JSONObject localRef = payload.optJSONObject("local_ref");
				if (transferId == null || localRef == null) {
					Log.w(TAG, "CONTENT_CACHED missing fields");
					return;
				}

				String textUtf8 = localRef.optString("text_utf8", null);
				String localPath = localRef.optString("local_path", null);
				String mime = localRef.optString("mime", null);

				awaiter.complete(new ContentFetchAwaiter.Result(transferId, textUtf8, localPath, mime));
				break;
			}

			case "TRANSFER_FAILED": {
				// Windows 外壳: payload.detail.transfer_id 优先，否则 payload.transfer_id
				String transferId = null;
				JSONObject detail = payload.optJSONObject("detail");
				if (detail != null) transferId = detail.optString("transfer_id", null);
				if (transferId == null) transferId = payload.optString("transfer_id", null);

				if (transferId == null) {
					Log.w(TAG, "TRANSFER_FAILED missing transfer_id");
					return;
				}

				String reason = payload.optString("reason",
					(detail != null ? detail.optString("reason", "transfer failed") : "transfer failed"));
				awaiter.fail(transferId, reason);
				break;
			}

			default:
				// 其他事件先忽略
				break;
		}
	}

	/**
	 * Windows 外壳 TryPickMeta:
	 * 1) 若 payload.meta 是对象，用它
	 * 2) 否则若 payload 本体有 item_id/kind，则把 payload 当 meta
	 */
	private static JSONObject tryPickMeta(JSONObject payload) {
		JSONObject m = payload.optJSONObject("meta");
		if (m != null) return m;

		// 兜底：payload 本体就是 meta
		String itemId = payload.optString("item_id", null);
		String kind = payload.optString("kind", null);
		if (itemId != null && kind != null) return payload;

		return null;
	}
}
