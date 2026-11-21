package dev.gpui.mobile;

import android.content.Context;
import android.util.Log;

import com.google.firebase.messaging.FirebaseMessagingService;
import com.google.firebase.messaging.RemoteMessage;

import org.json.JSONArray;
import org.json.JSONObject;

import java.util.Map;

/**
 * Receives silent high-priority data-only FCM wakes ("tickles") from Mailstream's Cloudflare
 * Push Worker, runs a bounded native Engine sync pass, and posts local Android OS notifications
 * straight from the device (zero message content ever traverses FCM, MS-443).
 */
public final class MailstreamMessagingService extends FirebaseMessagingService {
    private static final String TAG = "MailstreamFcmService";
    private static final String[] MAIL_ACTION_IDS = new String[] { "mark_read", "archive" };
    private static final String[] MAIL_ACTION_LABELS = new String[] { "Mark Read", "Archive" };

    private static native String nativeOnPushWake(
        String supportDir,
        String cacheDir,
        String source,
        String account,
        String subscriptionId
    );

    @Override
    public void onNewToken(final String token) {
        super.onNewToken(token);
        Log.i(TAG, "FCM token rotated");
        MailstreamFcm.reportToken(getApplicationContext(), token);
    }

    @Override
    public void onMessageReceived(final RemoteMessage message) {
        super.onMessageReceived(message);
        if (message == null) {
            return;
        }
        final Context appCtx = getApplicationContext();
        final Map<String, String> data = message.getData();
        final String source = nonNull(data.get("source"), "mail");
        final String account = nonNull(data.get("account"), "");
        final String subscriptionId = nonNull(data.get("subscriptionId"), "");

        Log.i(TAG, "FCM silent wake received: source=" + source + " account=" + account);

        try {
            System.loadLibrary("mailstream_android_poc");
            String supportDir = appCtx.getFilesDir().getAbsolutePath();
            String cacheDir = appCtx.getCacheDir().getAbsolutePath();
            String json = nativeOnPushWake(supportDir, cacheDir, source, account, subscriptionId);
            if (json == null || json.isEmpty()) {
                return;
            }
            JSONArray items = new JSONArray(json);
            for (int i = 0; i < items.length(); i++) {
                JSONObject obj = items.optJSONObject(i);
                if (obj == null) {
                    continue;
                }
                String tag = obj.optString("tag", "");
                String channel = obj.optString("channel", GpuiNotifications.CHANNEL_MAIL);
                String title = obj.optString("title", "Mailstream");
                String body = obj.optString("body", "");
                boolean isMail = GpuiNotifications.CHANNEL_MAIL.equals(channel);
                GpuiNotifications.showFromContext(
                    appCtx,
                    tag,
                    channel,
                    title,
                    body,
                    isMail ? MAIL_ACTION_IDS : null,
                    isMail ? MAIL_ACTION_LABELS : null
                );
            }
        } catch (Throwable t) {
            Log.w(TAG, "FCM silent wake sync failed", t);
        }
    }

    private static String nonNull(final String v, final String fallback) {
        return v != null ? v : fallback;
    }
}
