package com.ryan416.clipbridgeshellandroid.service;

import android.app.Notification;
import android.app.NotificationChannel;
import android.app.NotificationManager;
import android.app.Service;
import android.content.Intent;
import android.content.pm.ServiceInfo;
import android.os.Build;
import android.os.IBinder;
import android.util.Log;

import androidx.annotation.Nullable;
import androidx.core.app.NotificationCompat;

import com.ryan416.clipbridgeshellandroid.R;
import com.ryan416.clipbridgeshellandroid.clipboard.ClipboardWatcher;

public class ClipBridgeForegroundService extends Service {
	private static final String TAG = "CB.Service";
	private static final String CHANNEL_ID = "clipbridge_foreground";
	private static final int NOTIF_ID = 1001;
	private ClipboardWatcher clipboardWatcher;

	@Override
	public void onCreate() {
		super.onCreate();
		Log.i(TAG, "onCreate");
		ensureNotificationChannel();
	}

	@Override
	public int onStartCommand(Intent intent, int flags, int startId) {
		Log.i(TAG, "onStartCommand");
		Notification notification = buildNotification("Running", "ClipBridge service is active");
		if (Build.VERSION.SDK_INT >= 34) {
			startForeground(NOTIF_ID, notification, ServiceInfo.FOREGROUND_SERVICE_TYPE_DATA_SYNC);
		} else {
			startForeground(NOTIF_ID, notification);
		}

		if (clipboardWatcher == null) {
			clipboardWatcher = new ClipboardWatcher(getApplicationContext());
			clipboardWatcher.start();
		}
		// TODO Step 6: 初始化 Core + EventPump

		return START_STICKY;
	}

	@Override
	public void onDestroy() {
		Log.i(TAG, "onDestroy");
		if (clipboardWatcher != null) {
			clipboardWatcher.stop();
			clipboardWatcher = null;
		}
		super.onDestroy();
	}

	@Nullable
	@Override
	public IBinder onBind(Intent intent) {
		return null; // 不提供绑定
	}

	private void ensureNotificationChannel() {
		NotificationChannel channel = new NotificationChannel(
			CHANNEL_ID,
			"ClipBridge",
			NotificationManager.IMPORTANCE_LOW
		);
		channel.setDescription("ClipBridge foreground service");

		NotificationManager nm = getSystemService(NotificationManager.class);
		if (nm != null) nm.createNotificationChannel(channel);
	}

	private Notification buildNotification(String title, String text) {
		return new NotificationCompat.Builder(this, CHANNEL_ID)
			.setSmallIcon(R.mipmap.ic_launcher) // 你可以换成自己的图标
			.setContentTitle(title)
			.setContentText(text)
			.setOngoing(true)
			.build();
	}
}
