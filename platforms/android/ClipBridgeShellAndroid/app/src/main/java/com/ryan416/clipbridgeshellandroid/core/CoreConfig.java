package com.ryan416.clipbridgeshellandroid.core;

import android.content.Context;
import android.content.SharedPreferences;
import android.os.Build;
import android.util.Log;

import org.json.JSONObject;

import java.io.File;
import java.util.UUID;

public class CoreConfig {
	private static final String TAG = "CB.CoreConfig";
	private static final String PREF_NAME = "core_prefs";
	private static final String KEY_DEVICE_ID = "device_id";

	public static String build(Context ctx) {
		try {
			// 1. 获取或生成 Device ID
			SharedPreferences prefs = ctx.getSharedPreferences(PREF_NAME, Context.MODE_PRIVATE);
			String deviceId = prefs.getString(KEY_DEVICE_ID, null);
			if (deviceId == null) {
				deviceId = UUID.randomUUID().toString();
				prefs.edit().putString(KEY_DEVICE_ID, deviceId).apply();
				Log.i(TAG, "Generated new Device ID: " + deviceId);
			}

			// 2. 计算路径 (对应 Windows 的 CoreConfigBuilder)
			// data_dir: /data/user/0/.../files/core/data
			File coreRoot = new File(ctx.getFilesDir(), "core");
			File dataDir = new File(coreRoot, "data");
			// cache_dir: /data/user/0/.../cache/core/cache
			File cacheRoot = new File(ctx.getCacheDir(), "core");
			File cacheDir = new File(cacheRoot, "cache");
			// logs:
			File logDir = new File(coreRoot, "logs");

			// 确保目录存在
			if (!dataDir.exists()) dataDir.mkdirs();
			if (!cacheDir.exists()) cacheDir.mkdirs();
			if (!logDir.exists()) logDir.mkdirs();

			// 3. 构造 JSON
			JSONObject root = new JSONObject();
			root.put("device_id", deviceId);
			// 获取手机型号作为名称
			root.put("device_name", Build.MANUFACTURER + " " + Build.MODEL);
			root.put("account_uid", "default_user");
			root.put("account_tag", "default_tag");
			root.put("data_dir", dataDir.getAbsolutePath());
			root.put("cache_dir", cacheDir.getAbsolutePath());
			root.put("log_dir", logDir.getAbsolutePath()); // 加上日志路径方便排查

			// App Config
			JSONObject appConfig = new JSONObject();
			appConfig.put("global_policy", "AllowAll");

			JSONObject sizeLimits = new JSONObject();
			sizeLimits.put("soft_text_bytes", 1024 * 1024); // 1MB
			appConfig.put("size_limits", sizeLimits);

			root.put("app_config", appConfig);

			String json = root.toString();
			Log.d(TAG, "Generated Config JSON: " + json);
			return json;

		} catch (Exception e) {
			Log.e(TAG, "Failed to build config", e);
			// 返回一个最小可用配置，避免崩溃，但 Core 可能初始化失败
			return "{}";
		}
	}
}
