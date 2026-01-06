package com.ryan416.clipbridgeshellandroid;

import android.Manifest;
import android.content.Intent;
import android.content.pm.PackageManager;
import android.os.Build;
import android.os.Bundle;

import androidx.appcompat.app.AppCompatActivity;
import androidx.core.app.ActivityCompat;
import androidx.core.content.ContextCompat;

import rikka.shizuku.Shizuku;

import com.ryan416.clipbridgeshellandroid.service.ForegroundService;

public class MainActivity extends AppCompatActivity {
	private static final int REQ_NOTIF = 100;
	private static final int REQ_SHIZUKU = 200;

	private final Shizuku.OnRequestPermissionResultListener shizukuListener = (requestCode, grantResult) -> {
		if (requestCode == REQ_SHIZUKU && grantResult == PackageManager.PERMISSION_GRANTED) {
			startSvcAndExit();
		}
	};

	@Override
	protected void onCreate(Bundle savedInstanceState) {
		super.onCreate(savedInstanceState);
		Shizuku.addRequestPermissionResultListener(shizukuListener);
		checkStateAndStart();
	}

	@Override
	protected void onDestroy() {
		super.onDestroy();
		Shizuku.removeRequestPermissionResultListener(shizukuListener);
	}

	/**
	 * 利用 onResume 生命周期：
	 * 当用户在设置页面开启权限后返回 App，会自动触发此方法，
	 * 从而实现“开启后自动启动服务并最小化”的效果。
	 */
	@Override
	protected void onResume() {
		super.onResume();
		checkStateAndStart();
		checkShizukuPermission();
	}

	private void checkStateAndStart() {
		// 1. 检查 Shizuku 权限 (核心)
		if (!Shizuku.pingBinder()) {
			// Shizuku 没启动
			return;
		}

		if (Shizuku.checkSelfPermission() != PackageManager.PERMISSION_GRANTED) {
			if (!Shizuku.isPreV11()) {
				Shizuku.requestPermission(REQ_SHIZUKU);
			}
		} else {
			// 2. 权限已有，检查通知并启动
			checkNotificationAndStart();
		}
	}

	private void checkNotificationAndStart() {
		if (Build.VERSION.SDK_INT >= 33) {
			if (ContextCompat.checkSelfPermission(this, Manifest.permission.POST_NOTIFICATIONS)
				!= PackageManager.PERMISSION_GRANTED) {
				ActivityCompat.requestPermissions(this,
					new String[]{Manifest.permission.POST_NOTIFICATIONS},
					REQ_NOTIF);
				return;
			}
		}
		startSvcAndExit();
	}

	@Override
	public void onRequestPermissionsResult(int requestCode, String[] permissions, int[] grantResults) {
		super.onRequestPermissionsResult(requestCode, permissions, grantResults);
		if (requestCode == REQ_NOTIF) {
			startSvcAndExit();
		}
	}

	private void startSvcAndExit() {
		Intent svc = new Intent(this, ForegroundService.class);
		if (Build.VERSION.SDK_INT >= 26) {
			startForegroundService(svc);
		} else {
			startService(svc);
		}
	}

	private void checkShizukuPermission() {
		// 1. 检查 Shizuku 是否运行
		if (!Shizuku.pingBinder()) {
			// Shizuku 没运行 (用户没激活)
			// 这里可以弹个 Toast 提示用户去 Shizuku App 里启动服务
			return;
		}

		// 2. 检查是否有权限
		if (Shizuku.checkSelfPermission() != PackageManager.PERMISSION_GRANTED) {
			if (Shizuku.isPreV11()) {
				// 旧版本 Shizuku 处理（一般不用管）
			} else {
				// 3. 请求权限
				Shizuku.requestPermission(REQ_SHIZUKU);
			}
		} else {
			// 4. 权限已有，启动前台服务
			startSvcAndExit();
		}
	}


}
