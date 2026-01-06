package com.ryan416.clipbridgeshellandroid.events;
import android.util.Log;

import org.json.JSONObject;

import java.util.concurrent.BlockingQueue;
import java.util.concurrent.LinkedBlockingQueue;

public class EventPump {
	private static final String TAG = "CB.EventPump";

	public interface Handler {
		void handle(JSONObject envelope);
	}

	private final BlockingQueue<String> queue = new LinkedBlockingQueue<>();
	private final Handler handler;

	private Thread thread;
	private volatile boolean running = false;

	public EventPump(Handler handler) {
		this.handler = handler;
	}

	/** CoreEventSink.onCoreEvent(json) 调用这里 */
	public void post(String json) {
		if (!running) return;
		queue.offer(json);
	}

	public void start() {
		if (running) return;
		running = true;

		thread = new Thread(() -> {
			Log.i(TAG, "EventPump Started.");
			while (running) {
				try {
					String json = queue.take();
					JSONObject env = new JSONObject(json);
					handler.handle(env);
				} catch (InterruptedException ie) {
					// allow exit
				} catch (Throwable t) {
					Log.e(TAG, "Event parse/handle error", t);
				}
			}
			Log.i(TAG, "EventPump Stopped.");
		}, "CB-EventPump");

		thread.start();
	}

	public void stop() {
		running = false;
		if (thread != null) {
			thread.interrupt();
			thread = null;
		}
		queue.clear();
	}
}
