package dev.gpui.mobile;

import android.content.Context;
import android.content.SharedPreferences;
import android.net.ConnectivityManager;
import android.net.Network;
import android.util.Log;

import com.google.firebase.FirebaseApp;
import com.google.firebase.FirebaseOptions;
import com.google.firebase.messaging.FirebaseMessaging;

import java.util.UUID;
import java.util.concurrent.atomic.AtomicBoolean;

/**
 * Manages Android FCM initialization (baked google-services.json or runtime Device Sync options),
 * token fetching, and ConnectivityManager network-change notifications to Rust Engine (MS-443).
 */
public final class MailstreamFcm {
    private static final String TAG = "MailstreamFcm";
    private static final String PREFS = "mailstream_push_prefs";
    private static final String KEY_DEVICE_ID = "device_id";
    private static final String KEY_APP_ID = "fcm_app_id";
    private static final String KEY_API_KEY = "fcm_api_key";
    private static final String KEY_PROJECT_ID = "fcm_project_id";
    private static final String KEY_SENDER_ID = "fcm_sender_id";

    private static final AtomicBoolean NETWORK_CALLBACK_REGISTERED = new AtomicBoolean(false);
    private static volatile boolean seenInitialNetwork = false;

    private MailstreamFcm() {}

    private static native void nativeOnFcmToken(String supportDir, String cacheDir, String deviceId, String token);
    private static native void nativeNetworkChanged();

    public static String deviceId(final Context context) {
        SharedPreferences prefs = context.getApplicationContext().getSharedPreferences(PREFS, Context.MODE_PRIVATE);
        String id = prefs.getString(KEY_DEVICE_ID, null);
        if (id == null || id.isEmpty()) {
            id = "android-" + UUID.randomUUID().toString();
            prefs.edit().putString(KEY_DEVICE_ID, id).apply();
        }
        return id;
    }

    /** Called from Activity.onCreate to start FCM token sync and network monitoring. */
    public static void start(final Context context) {
        if (context == null) {
            return;
        }
        Context appCtx = context.getApplicationContext();
        registerNetworkCallback(appCtx);
        fetchAndReportToken(appCtx);
    }

    /**
     * Ensures FirebaseApp is initialized, either from baked google_services.xml resources
     * (initialized by FirebaseInitProvider) or from runtime options cached in SharedPreferences.
     */
    public static boolean ensureInitialized(final Context context) {
        Context appCtx = context.getApplicationContext();
        if (!FirebaseApp.getApps(appCtx).isEmpty()) {
            return true;
        }
        try {
            if (FirebaseApp.initializeApp(appCtx) != null) {
                return true;
            }
        } catch (Throwable ignored) {
        }
        SharedPreferences prefs = appCtx.getSharedPreferences(PREFS, Context.MODE_PRIVATE);
        String appId = prefs.getString(KEY_APP_ID, null);
        String apiKey = prefs.getString(KEY_API_KEY, null);
        String projectId = prefs.getString(KEY_PROJECT_ID, null);
        String senderId = prefs.getString(KEY_SENDER_ID, null);
        if (isEmpty(appId) || isEmpty(apiKey) || isEmpty(projectId) || isEmpty(senderId)) {
            return false;
        }
        return initWithOptionsInternal(appCtx, appId, apiKey, projectId, senderId);
    }

    /** Invoked via JNI when Device Sync or config.json supplies Firebase client options. */
    public static void initializeWithOptions(
        final Context context,
        final String appId,
        final String apiKey,
        final String projectId,
        final String senderId
    ) {
        if (context == null || isEmpty(appId) || isEmpty(apiKey) || isEmpty(projectId) || isEmpty(senderId)) {
            return;
        }
        Context appCtx = context.getApplicationContext();
        SharedPreferences prefs = appCtx.getSharedPreferences(PREFS, Context.MODE_PRIVATE);
        prefs.edit()
            .putString(KEY_APP_ID, appId.trim())
            .putString(KEY_API_KEY, apiKey.trim())
            .putString(KEY_PROJECT_ID, projectId.trim())
            .putString(KEY_SENDER_ID, senderId.trim())
            .apply();
        if (initWithOptionsInternal(appCtx, appId.trim(), apiKey.trim(), projectId.trim(), senderId.trim())) {
            fetchAndReportToken(appCtx);
        }
    }

    private static boolean initWithOptionsInternal(
        final Context appCtx,
        final String appId,
        final String apiKey,
        final String projectId,
        final String senderId
    ) {
        if (!FirebaseApp.getApps(appCtx).isEmpty()) {
            return true;
        }
        try {
            FirebaseOptions options = new FirebaseOptions.Builder()
                .setApplicationId(appId)
                .setApiKey(apiKey)
                .setProjectId(projectId)
                .setGcmSenderId(senderId)
                .build();
            FirebaseApp.initializeApp(appCtx, options);
            Log.i(TAG, "Initialized FirebaseApp from runtime options (project=" + projectId + ")");
            return true;
        } catch (Throwable t) {
            Log.w(TAG, "FirebaseApp runtime initialization failed", t);
            return false;
        }
    }

    public static void fetchAndReportToken(final Context context) {
        if (context == null) {
            return;
        }
        final Context appCtx = context.getApplicationContext();
        if (!ensureInitialized(appCtx)) {
            Log.i(TAG, "FCM not configured yet (no google-services.json or synced FCM options)");
            return;
        }
        try {
            FirebaseMessaging.getInstance().getToken().addOnCompleteListener(task -> {
                if (!task.isSuccessful()) {
                    Log.w(TAG, "Fetching FCM registration token failed", task.getException());
                    return;
                }
                String token = task.getResult();
                if (!isEmpty(token)) {
                    reportToken(appCtx, token);
                }
            });
        } catch (Throwable t) {
            Log.w(TAG, "FirebaseMessaging.getToken() failed", t);
        }
    }

    public static void reportToken(final Context context, final String token) {
        if (context == null || isEmpty(token)) {
            return;
        }
        Context appCtx = context.getApplicationContext();
        try {
            System.loadLibrary("mailstream_android_poc");
            nativeOnFcmToken(
                appCtx.getFilesDir().getAbsolutePath(),
                appCtx.getCacheDir().getAbsolutePath(),
                deviceId(appCtx),
                token
            );
        } catch (Throwable t) {
            Log.w(TAG, "nativeOnFcmToken failed", t);
        }
    }

    public static void registerNetworkCallback(final Context context) {
        if (context == null || !NETWORK_CALLBACK_REGISTERED.compareAndSet(false, true)) {
            return;
        }
        try {
            ConnectivityManager cm = (ConnectivityManager) context.getSystemService(Context.CONNECTIVITY_SERVICE);
            if (cm == null) {
                return;
            }
            cm.registerDefaultNetworkCallback(new ConnectivityManager.NetworkCallback() {
                @Override
                public void onAvailable(Network network) {
                    if (!seenInitialNetwork) {
                        seenInitialNetwork = true;
                        return;
                    }
                    try {
                        nativeNetworkChanged();
                    } catch (UnsatisfiedLinkError ignored) {
                    }
                }
            });
        } catch (Throwable t) {
            NETWORK_CALLBACK_REGISTERED.set(false);
            Log.w(TAG, "Failed to register ConnectivityManager callback", t);
        }
    }

    private static boolean isEmpty(final String s) {
        return s == null || s.trim().isEmpty();
    }
}
