package dev.gpui.mobile;

import android.app.Activity;
import android.content.ClipData;
import android.content.ClipboardManager;
import android.content.Context;
import android.os.Looper;

import java.util.concurrent.CountDownLatch;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.atomic.AtomicReference;

/**
 * System clipboard bridge invoked via JNI by gpui-mobile's {@code clipboard} package.
 */
public final class GpuiClipboard {
    private GpuiClipboard() {}

    public static void setText(final Activity activity, final String text) {
        if (activity == null) {
            return;
        }
        Runnable task = () -> {
            ClipboardManager cm = (ClipboardManager) activity.getSystemService(Context.CLIPBOARD_SERVICE);
            if (cm != null) {
                cm.setPrimaryClip(ClipData.newPlainText("Mailstream", text != null ? text : ""));
            }
        };
        if (Looper.myLooper() == Looper.getMainLooper()) {
            task.run();
        } else {
            activity.runOnUiThread(task);
        }
    }

    public static String getText(final Activity activity) {
        if (activity == null) {
            return null;
        }
        if (Looper.myLooper() == Looper.getMainLooper()) {
            return readNow(activity);
        }
        final AtomicReference<String> result = new AtomicReference<>(null);
        final CountDownLatch latch = new CountDownLatch(1);
        activity.runOnUiThread(() -> {
            try {
                result.set(readNow(activity));
            } finally {
                latch.countDown();
            }
        });
        try {
            latch.await(1, TimeUnit.SECONDS);
        } catch (InterruptedException ignored) {
            Thread.currentThread().interrupt();
        }
        return result.get();
    }

    public static boolean hasText(final Activity activity) {
        String text = getText(activity);
        return text != null && !text.isEmpty();
    }

    private static String readNow(Activity activity) {
        ClipboardManager cm = (ClipboardManager) activity.getSystemService(Context.CLIPBOARD_SERVICE);
        if (cm == null || !cm.hasPrimaryClip()) {
            return null;
        }
        ClipData clip = cm.getPrimaryClip();
        if (clip == null || clip.getItemCount() == 0) {
            return null;
        }
        CharSequence cs = clip.getItemAt(0).coerceToText(activity);
        return cs != null ? cs.toString() : null;
    }
}
