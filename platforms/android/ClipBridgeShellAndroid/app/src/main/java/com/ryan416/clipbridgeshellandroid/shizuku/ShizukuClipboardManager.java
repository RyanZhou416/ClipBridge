package com.ryan416.clipbridgeshellandroid.shizuku;

import android.content.ClipData;
import android.content.IClipboard;
import android.content.IOnPrimaryClipChangedListener;
import android.os.IBinder;
import android.os.RemoteException;
import android.util.Log;

import rikka.shizuku.ShizukuBinderWrapper;
import rikka.shizuku.SystemServiceHelper;

public class ShizukuClipboardManager {
	private static final String TAG = "CB.ShizukuMgr";
	private static IClipboard iClipboard;

	// 关键点：既然借用了 Shell 权限，就必须自称是 Shell
	private static final String SHELL_PKG = "com.android.shell";

	private static int getUserId() {
		return 0; // 主用户 ID
	}

	private static int getDeviceId() {
		return 0; // DEVICE_ID_DEFAULT
	}

	private static IClipboard getService() {
		if (iClipboard != null && iClipboard.asBinder().isBinderAlive()) {
			return iClipboard;
		}
		IBinder binder = SystemServiceHelper.getSystemService("clipboard");
		if (binder == null) return null;
		iClipboard = IClipboard.Stub.asInterface(new ShizukuBinderWrapper(binder));
		return iClipboard;
	}

	public static boolean setPrimaryClip(ClipData clip, String ignoredPkg) {
		IClipboard service = getService();
		if (service == null) return false;
		try {
			// [修改] 传 SHELL_PKG
			service.setPrimaryClip(clip, SHELL_PKG, null, getUserId(), getDeviceId());
			return true;
		} catch (RemoteException e) {
			Log.e(TAG, "setPrimaryClip failed", e);
			return false;
		}
	}

	public static ClipData getPrimaryClip(String ignoredPkg) {
		IClipboard service = getService();
		if (service == null) return null;
		try {
			// [修改] 传 SHELL_PKG
			return service.getPrimaryClip(SHELL_PKG, null, getUserId(), getDeviceId());
		} catch (RemoteException e) {
			Log.e(TAG, "getPrimaryClip failed", e);
			return null;
		}
	}

	public static void addPrimaryClipChangedListener(IOnPrimaryClipChangedListener listener, String ignoredPkg) {
		IClipboard service = getService();
		if (service == null) return;
		try {
			// [修改] 传 SHELL_PKG
			service.addPrimaryClipChangedListener(listener, SHELL_PKG, null, getUserId(), getDeviceId());
			Log.i(TAG, "Listener registered as Shell!");
		} catch (RemoteException e) {
			Log.e(TAG, "addListener failed", e);
		}
	}

	public static void removePrimaryClipChangedListener(IOnPrimaryClipChangedListener listener, String ignoredPkg) {
		IClipboard service = getService();
		if (service == null) return;
		try {
			// [修改] 传 SHELL_PKG
			service.removePrimaryClipChangedListener(listener, SHELL_PKG, null, getUserId(), getDeviceId());
		} catch (RemoteException e) {
			Log.e(TAG, "removeListener failed", e);
		}
	}
}
