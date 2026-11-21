package dev.gpui.mobile;

import android.content.BroadcastReceiver;
import android.content.Context;
import android.content.Intent;
import android.util.Log;

import java.util.concurrent.ExecutorService;
import java.util.concurrent.Executors;

/**
 * Handles Mailstream notification action buttons ("Mark Read", "Archive") in the background
 * without opening the UI (MS-443).
 */
public final class GpuiNotificationActionReceiver extends BroadcastReceiver {
    private static final String TAG = "MailstreamNotifAction";
    private static final ExecutorService EXECUTOR = Executors.newSingleThreadExecutor();

    private static native void nativeNotificationAction(
        String supportDir,
        String cacheDir,
        String tag,
        String actionId
    );

    @Override
    public void onReceive(final Context context, final Intent intent) {
        if (context == null || intent == null) {
            return;
        }
        final String tag = intent.getStringExtra(GpuiNotifications.EXTRA_NOTIFICATION_TAG);
        final String actionId = intent.getStringExtra(GpuiNotifications.EXTRA_NOTIFICATION_ACTION);
        if (tag == null || tag.isEmpty() || actionId == null || actionId.isEmpty()) {
            return;
        }
        GpuiNotifications.dismissFromContext(context, tag);

        final Context appCtx = context.getApplicationContext();
        final PendingResult pending = goAsync();
        EXECUTOR.execute(() -> {
            try {
                System.loadLibrary("mailstream_android_poc");
                String supportDir = appCtx.getFilesDir().getAbsolutePath();
                String cacheDir = appCtx.getCacheDir().getAbsolutePath();
                nativeNotificationAction(supportDir, cacheDir, tag, actionId);
            } catch (Throwable t) {
                Log.w(TAG, "Failed to run notification action " + actionId + " for " + tag, t);
            } finally {
                pending.finish();
            }
        });
    }
}
