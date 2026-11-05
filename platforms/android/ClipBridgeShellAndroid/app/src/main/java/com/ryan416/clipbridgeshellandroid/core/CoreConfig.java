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
	private static final String KEY_ACCOUNT_PASSWORD = "account_password";

	// 临时配对凭据占位符：与下方 account_uid="default_user" / account_tag="default_tag" 配套。
	// Android 外壳目前还没有真实账号体系，先用明文 SharedPreferences 跑通配对流程；
	// 注意：这只是临时方案，后续必须改为向用户索取真实密码，并用 Android Keystore
	// 加密保存（或直接由 Keystore 派生配对密钥），不要再把明文密码写进 SharedPreferences/init JSON。
	private static final String DEFAULT_ACCOUNT_PASSWORD = "default_password";

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

			// 2. 读取账号密码：目前从 SharedPreferences 读取；没有写入过时退回占位符，
			//    与 default_user / default_tag 占位符保持一致（临时配对凭据，见文件头部说明）。
			String accountPassword = prefs.getString(KEY_ACCOUNT_PASSWORD, DEFAULT_ACCOUNT_PASSWORD);
			if (DEFAULT_ACCOUNT_PASSWORD.equals(accountPassword)) {
				Log.w(TAG, "account_password 使用占位符默认值；如需真实配对，请向 core_prefs 写入 " + KEY_ACCOUNT_PASSWORD);
			}

			// 3. 计算路径 (对应 Windows 的 CoreConfigBuilder)
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

			// 4. 构造 JSON
			JSONObject root = new JSONObject();
			root.put("device_id", deviceId);
			// 获取手机型号作为名称
			root.put("device_name", Build.MANUFACTURER + " " + Build.MODEL);
			root.put("account_uid", "default_user");
			root.put("account_tag", "default_tag");
			// Rust InitConfigDto.account_password 是必填字段（无 serde default），缺了 init 会直接失败
			root.put("account_password", accountPassword);
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
			// 不要把 account_password 打进 logcat
			Log.d(TAG, "Generated Config JSON for device_id=" + deviceId + " (account_password redacted)");
			return json;

		} catch (Exception e) {
			Log.e(TAG, "Failed to build config", e);
			// 返回一个最小可用配置，避免崩溃，但 Core 可能初始化失败
			return "{}";
		}
	}
}
