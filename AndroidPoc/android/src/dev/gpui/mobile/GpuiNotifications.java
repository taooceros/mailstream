package dev.gpui.mobile;

import android.Manifest;
import android.app.Activity;
import android.app.Notification;
import android.app.NotificationChannel;
import android.app.NotificationManager;
import android.app.PendingIntent;
import android.content.Context;
import android.content.Intent;
import android.content.pm.PackageManager;
import android.os.Build;

import dev.mailstream.gpui.poc.R;

/**
 * Native Android notification bridge for Mailstream GPUI and background FCM wakes (MS-443).
 * Uses pure Android Framework APIs (minSdk 26 / targetSdk 35) with channels, tap-to-open
 * PendingIntent, and background Mark Read / Archive quick action buttons.
 */
public final class GpuiNotifications {
    public static final String CHANNEL_MAIL = "mail";
    public static final String CHANNEL_CHAT = "chat";
    public static final String CHANNEL_DEFAULT = "default";

    public static final String EXTRA_NOTIFICATION_TAG = "dev.mailstream.EXTRA_NOTIFICATION_TAG";
    public static final String EXTRA_NOTIFICATION_ACTION = "dev.mailstream.EXTRA_NOTIFICATION_ACTION";

    private static final String GROUP_MAIL = "dev.mailstream.GROUP_MAIL";
    private static final String GROUP_CHAT = "dev.mailstream.GROUP_CHAT";

    private GpuiNotifications() {}

    public static void initialize(final Activity activity) {
        if (activity != null) {
            ensureChannels(activity.getApplicationContext());
        }
    }

    public static void ensureChannels(final Context context) {
        if (context == null || Build.VERSION.SDK_INT < Build.VERSION_CODES.O) {
            return;
        }
        NotificationManager nm = (NotificationManager) context.getSystemService(Context.NOTIFICATION_SERVICE);
        if (nm == null) {
            return;
        }
        NotificationChannel mail = new NotificationChannel(
            CHANNEL_MAIL,
            "Mail",
            NotificationManager.IMPORTANCE_HIGH
        );
        mail.setDescription("New incoming email notifications");
        nm.createNotificationChannel(mail);

        NotificationChannel chat = new NotificationChannel(
            CHANNEL_CHAT,
            "Google Chat",
            NotificationManager.IMPORTANCE_HIGH
        );
        chat.setDescription("New Google Chat messages");
        nm.createNotificationChannel(chat);

        NotificationChannel def = new NotificationChannel(
            CHANNEL_DEFAULT,
            "General",
            NotificationManager.IMPORTANCE_DEFAULT
        );
        def.setDescription("General Mailstream notifications");
        nm.createNotificationChannel(def);
    }

    private static boolean hasPostPermission(final Context context) {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.TIRAMISU) {
            return true;
        }
        return context.checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS)
            == PackageManager.PERMISSION_GRANTED;
    }

    public static int notificationIdForTag(final String tag) {
        int h = tag != null ? tag.hashCode() : 1;
        return h == 0 ? 1 : h;
    }

    /** Invoked via JNI from `AndroidPlatform::show_system_notification`. */
    public static void showWithActions(
        final Activity activity,
        final String tag,
        final String channelId,
        final String title,
        final String body,
        final String[] actionIds,
        final String[] actionLabels
    ) {
        if (activity == null) {
            return;
        }
        showFromContext(activity.getApplicationContext(), tag, channelId, title, body, actionIds, actionLabels);
    }

    /** Can be invoked from either an Activity or a background Service (e.g. FCM wake). */
    public static void showFromContext(
        final Context context,
        final String tag,
        final String channelId,
        final String title,
        final String body,
        final String[] actionIds,
        final String[] actionLabels
    ) {
        if (context == null || !hasPostPermission(context)) {
            return;
        }
        ensureChannels(context);
        NotificationManager nm = (NotificationManager) context.getSystemService(Context.NOTIFICATION_SERVICE);
        if (nm == null) {
            return;
        }

        final String safeTag = (tag != null && !tag.isEmpty()) ? tag : "default";
        final String safeChannel = resolveChannel(channelId, safeTag);
        final int notifId = notificationIdForTag(safeTag);

        Intent openIntent = new Intent(context, GpuiInputActivity.class);
        openIntent.setAction("dev.mailstream.OPEN_NOTIFICATION." + notifId);
        openIntent.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK | Intent.FLAG_ACTIVITY_SINGLE_TOP | Intent.FLAG_ACTIVITY_CLEAR_TOP);
        openIntent.putExtra(EXTRA_NOTIFICATION_TAG, safeTag);
        PendingIntent contentIntent = PendingIntent.getActivity(
            context,
            notifId,
            openIntent,
            PendingIntent.FLAG_UPDATE_CURRENT | PendingIntent.FLAG_IMMUTABLE
        );

        Notification.Builder builder = Build.VERSION.SDK_INT >= Build.VERSION_CODES.O
            ? new Notification.Builder(context, safeChannel)
            : new Notification.Builder(context);

        builder
            .setSmallIcon(R.drawable.ic_stat_mailstream)
            .setContentTitle(title != null ? title : "Mailstream")
            .setContentText(body != null ? body : "")
            .setStyle(new Notification.BigTextStyle().bigText(body != null ? body : ""))
            .setAutoCancel(true)
            .setContentIntent(contentIntent)
            .setCategory(CHANNEL_CHAT.equals(safeChannel) ? Notification.CATEGORY_MESSAGE : Notification.CATEGORY_EMAIL)
            .setGroup(CHANNEL_CHAT.equals(safeChannel) ? GROUP_CHAT : GROUP_MAIL);

        if (actionIds != null && actionLabels != null) {
            int count = Math.min(actionIds.length, actionLabels.length);
            for (int i = 0; i < count; i++) {
                String actionId = actionIds[i];
                String label = actionLabels[i];
                if (actionId == null || actionId.isEmpty() || label == null || label.isEmpty()) {
                    continue;
                }
                Intent actionIntent = new Intent(context, GpuiNotificationActionReceiver.class);
                actionIntent.setAction("dev.mailstream.NOTIFICATION_ACTION." + actionId + "." + notifId);
                actionIntent.putExtra(EXTRA_NOTIFICATION_TAG, safeTag);
                actionIntent.putExtra(EXTRA_NOTIFICATION_ACTION, actionId);
                PendingIntent pi = PendingIntent.getBroadcast(
                    context,
                    notifId * 31 + i,
                    actionIntent,
                    PendingIntent.FLAG_UPDATE_CURRENT | PendingIntent.FLAG_IMMUTABLE
                );
                Notification.Action action = new Notification.Action.Builder(null, label, pi).build();
                builder.addAction(action);
            }
        }

        nm.notify(safeTag, notifId, builder.build());
    }

    public static void dismissByTag(final Activity activity, final String tag) {
        if (activity == null) {
            return;
        }
        dismissFromContext(activity.getApplicationContext(), tag);
    }

    public static void dismissFromContext(final Context context, final String tag) {
        if (context == null || tag == null || tag.isEmpty()) {
            return;
        }
        NotificationManager nm = (NotificationManager) context.getSystemService(Context.NOTIFICATION_SERVICE);
        if (nm != null) {
            nm.cancel(tag, notificationIdForTag(tag));
        }
    }

    /** Legacy gpui-mobile `packages::notifications` entry point. */
    public static void show(
        final Activity activity,
        final int id,
        final String title,
        final String body,
        final String channelId,
        final String channelName,
        final String channelDesc,
        final int importance,
        final String payload
    ) {
        if (activity == null) {
            return;
        }
        String tag = (payload != null && !payload.isEmpty()) ? payload : ("local:" + id);
        showFromContext(activity.getApplicationContext(), tag, channelId, title, body, null, null);
    }

    public static void cancel(final Activity activity, final int id) {
        if (activity == null) {
            return;
        }
        NotificationManager nm = (NotificationManager) activity.getSystemService(Context.NOTIFICATION_SERVICE);
        if (nm != null) {
            nm.cancel(id);
        }
    }

    public static void cancelAll(final Activity activity) {
        if (activity == null) {
            return;
        }
        NotificationManager nm = (NotificationManager) activity.getSystemService(Context.NOTIFICATION_SERVICE);
        if (nm != null) {
            nm.cancelAll();
        }
    }

    private static String resolveChannel(final String channelId, final String tag) {
        if (CHANNEL_MAIL.equals(channelId) || CHANNEL_CHAT.equals(channelId) || CHANNEL_DEFAULT.equals(channelId)) {
            return channelId;
        }
        if (tag.startsWith("chat:")) {
            return CHANNEL_CHAT;
        }
        if (tag.startsWith("mail:")) {
            return CHANNEL_MAIL;
        }
        return CHANNEL_DEFAULT;
    }
}
