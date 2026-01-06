package com.ryan416.clipbridgeshellandroid;

import android.app.Application;
import android.content.Context;
import org.lsposed.hiddenapibypass.HiddenApiBypass;

public class App extends Application {
	@Override
	protected void attachBaseContext(Context base) {
		super.attachBaseContext(base);
		// 解除 Hidden API 限制
		if (android.os.Build.VERSION.SDK_INT >= 28) {
			HiddenApiBypass.addHiddenApiExemptions("");
		}
	}
}
