package com.ryan416.clipbridgeshellandroid;

import android.Manifest;
import android.content.Intent;
import android.content.pm.PackageManager;
import android.os.Build;
import android.os.Bundle;

import androidx.activity.EdgeToEdge;
import androidx.appcompat.app.AppCompatActivity;
import androidx.core.app.ActivityCompat;
import androidx.core.content.ContextCompat;
import androidx.core.graphics.Insets;
import androidx.core.view.ViewCompat;
import androidx.core.view.WindowInsetsCompat;

import com.ryan416.clipbridgeshellandroid.core.CoreNative;
import com.ryan416.clipbridgeshellandroid.service.ClipBridgeForegroundService;

public class MainActivity extends AppCompatActivity {
	private static final int REQ_NOTIF = 100;
	@Override
	protected void onCreate(Bundle savedInstanceState) {
		super.onCreate(savedInstanceState);

		//测试导入本地库是否成功
		CoreNative unused = new CoreNative();
		if (Build.VERSION.SDK_INT >= 33) {
			if (ContextCompat.checkSelfPermission(this, Manifest.permission.POST_NOTIFICATIONS)
				!= PackageManager.PERMISSION_GRANTED) {
				ActivityCompat.requestPermissions(this,
					new String[]{Manifest.permission.POST_NOTIFICATIONS},
					REQ_NOTIF);
				return;
			}
		}
		unused = null;
		startSvcAndExit();
	}

	@Override
	public void onRequestPermissionsResult(int requestCode, String[] permissions, int[] grantResults) {
		super.onRequestPermissionsResult(requestCode, permissions, grantResults);
		if (requestCode == REQ_NOTIF) {
			// 不管给不给都继续：不给会影响通知，但你至少能看到行为
			startSvcAndExit();
		}
	}

	private void startSvcAndExit() {
		Intent svc = new Intent(this, ClipBridgeForegroundService.class);
		startForegroundService(svc);
		moveTaskToBack(true);
	}
}
