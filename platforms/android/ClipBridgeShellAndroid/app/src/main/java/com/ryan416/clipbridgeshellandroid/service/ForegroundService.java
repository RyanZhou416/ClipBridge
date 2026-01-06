package com.ryan416.clipbridgeshellandroid.service;

import android.app.Notification;
import android.app.NotificationChannel;
import android.app.NotificationManager;
import android.app.Service;
import android.content.Intent;
import android.content.pm.ServiceInfo;
import android.os.IBinder;
import android.util.Log;

import androidx.annotation.Nullable;
import androidx.core.app.NotificationCompat;

import com.ryan416.clipbridgeshellandroid.R;
import com.ryan416.clipbridgeshellandroid.clipboard.ClipboardApplyService;
import com.ryan416.clipbridgeshellandroid.clipboard.ClipboardWatcher;
import com.ryan416.clipbridgeshellandroid.core.CoreEventSink;
import com.ryan416.clipbridgeshellandroid.core.CoreInterop;
import com.ryan416.clipbridgeshellandroid.events.ContentFetchAwaiter;
import com.ryan416.clipbridgeshellandroid.events.CoreEventRouter;
import com.ryan416.clipbridgeshellandroid.events.EventPump;

import org.json.JSONObject;

public class ForegroundService extends Service {
	private static final String TAG = "CB.Service";
	private static final String CHANNEL_ID = "clipbridge_foreground";
	private static final int NOTIF_ID = 1001;

	private ClipboardWatcher clipboardWatcher;
	private CoreInterop core;
	private ContentFetchAwaiter awaiter;
	private ClipboardApplyService applyService;
	private EventPump eventPump;
	private CoreEventRouter router;
	private CoreInterop.CoreEventListener listener;

	private final CoreEventSink sink = json -> {
		if (eventPump != null) eventPump.post(json);
	};

	@Override
	public void onCreate() {
		super.onCreate();
		Log.i(TAG, "onCreate");
		ensureNotificationChannel();

		Notification notification = buildNotification("ClipBridge Running", "Shizuku Mode Active");
		if (android.os.Build.VERSION.SDK_INT >= 34) {
			startForeground(NOTIF_ID, notification, ServiceInfo.FOREGROUND_SERVICE_TYPE_DATA_SYNC);
		} else {
			startForeground(NOTIF_ID, notification);
		}

		// 以前的 心跳检测 和 悬浮窗 代码统统不要了！
	}

	@Override
	public int onStartCommand(Intent intent, int flags, int startId) {
		Log.i(TAG, "onStartCommand");

		if (awaiter == null) awaiter = new ContentFetchAwaiter();
		if (listener == null) listener = sink::onCoreEvent;

		if (applyService == null) {
			if (core == null) core = new CoreInterop();
			Log.i(TAG, "Initializing Core...");

			// Core Init
			String configJson = com.ryan416.clipbridgeshellandroid.core.CoreConfig.build(getApplicationContext());
			String resultJson = core.init(configJson, listener);
			Log.i(TAG, "Core init result: " + resultJson);

			// Apply Service
			applyService = new ClipboardApplyService(getApplicationContext(), clipboardWatcher, core, awaiter);
		}

		if (router == null) {
			router = new CoreEventRouter(awaiter, meta -> applyService.onItemMeta(meta));
		}

		if (eventPump == null) {
			eventPump = new EventPump(router);
			eventPump.start();
		}

		if (clipboardWatcher == null) {
			clipboardWatcher = new ClipboardWatcher(getApplicationContext(), core);

			new android.os.Handler(android.os.Looper.getMainLooper()).postDelayed(() -> {
				if (clipboardWatcher != null) {
					Log.i(TAG, "Delayed start of ClipboardWatcher...");
					clipboardWatcher.start();
				}
			}, 500); // 延迟 500ms
		}

		return START_STICKY;
	}

	@Override
	public void onDestroy() {
		Log.i(TAG, "onDestroy");
		if (clipboardWatcher != null) { clipboardWatcher.stop(); clipboardWatcher = null; }
		if (eventPump != null) { eventPump.stop(); eventPump = null; }
		if (applyService != null) { applyService.shutdown(); applyService = null; }
		if (awaiter != null) { awaiter.clear(); awaiter = null; }
		super.onDestroy();
	}

	@Nullable
	@Override
	public IBinder onBind(Intent intent) { return null; }

	private void ensureNotificationChannel() {
		NotificationChannel channel = new NotificationChannel(CHANNEL_ID, "ClipBridge", NotificationManager.IMPORTANCE_LOW);
		NotificationManager nm = getSystemService(NotificationManager.class);
		if (nm != null) nm.createNotificationChannel(channel);
	}

	private Notification buildNotification(String title, String text) {
		return new NotificationCompat.Builder(this, CHANNEL_ID)
			.setSmallIcon(R.mipmap.ic_launcher)
			.setContentTitle(title)
			.setContentText(text)
			.setOngoing(true)
			.build();
	}
}
