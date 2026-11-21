package dev.gpui.mobile;

import android.Manifest;
import android.app.NativeActivity;
import android.content.Context;
import android.content.Intent;
import android.content.SharedPreferences;
import android.content.pm.PackageManager;
import android.os.Bundle;
import android.text.InputType;
import android.content.res.Configuration;
import android.graphics.Color;
import android.graphics.Insets;
import android.os.Build;
import android.view.KeyEvent;
import android.view.ViewGroup;
import android.view.Window;
import android.view.inputmethod.BaseInputConnection;
import android.view.inputmethod.EditorInfo;
import android.view.inputmethod.InputConnection;
import android.view.View;
import android.view.WindowInsets;
import android.view.WindowInsetsAnimation;
import android.view.WindowInsetsController;
import android.view.WindowManager;
import android.view.inputmethod.InputMethodManager;
import android.window.BackEvent;
import android.window.OnBackAnimationCallback;
import android.window.OnBackInvokedCallback;
import android.window.OnBackInvokedDispatcher;
import android.widget.EditText;
import android.widget.FrameLayout;
import java.util.List;

/**
 * Minimal NativeActivity host bridging Android IME (including CJK composing text) to gpui-mobile's
 * {@code Java_dev_gpui_mobile_GpuiInputActivity_nativeIme} JNI entry point.
 */
public class GpuiInputActivity extends NativeActivity {
    private static final int IME_COMPOSING = 0;
    private static final int IME_COMMIT = 1;
    private static final int IME_DELETE_SURROUNDING = 2;
    private static final int IME_DELETE_CODE_POINTS = 3;
    private static final int IME_ESCAPE = 4;
    private static final int REQ_POST_NOTIFICATIONS = 1001;
    private static final String PREFS_PERMS = "mailstream_perm_prefs";
    private static final String KEY_ASKED_NOTIF = "asked_post_notifications";
    private volatile long imeSession = 0;
    private int inputKind = 0;
    private ImeProxyView imeProxy;
    private volatile boolean backEnabled = false;
    private Object backCallback = null;
    private boolean backCallbackRegistered = false;
    private boolean imeAnimating = false;
    private int lastNavBarBottomPx = 0;
    private int imeTargetBottomPx = 0;

    static {
        System.loadLibrary("mailstream_android_poc");
    }

    private static native void nativeIme(long session, int kind, String text, int start, int end);
    private static native void nativeInsets(int top, int bottom, int left, int right, float density);
    private static native void nativeDisplayMetrics(float density, float fontScale);
    private static native void nativeBack();
    private static native void nativeImeInsets(int imeBottomPx, int targetBottomPx, boolean animating, float density);
    private static native void nativeBackStarted(float progress, int edge);
    private static native void nativeBackProgressed(float progress, int edge);
    private static native void nativeBackCancelled();
    private static native void nativeTrimMemory(int level);
    private static native void nativeNotificationResponse(String tag, String actionId);

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        imeProxy = new ImeProxyView(this);
        imeProxy.setAlpha(0f);
        imeProxy.setFocusable(true);
        imeProxy.setFocusableInTouchMode(true);
        ViewGroup content = findViewById(android.R.id.content);
        if (content != null) {
            content.addView(imeProxy, new FrameLayout.LayoutParams(1, 1));
        }
        final View decor = getWindow().getDecorView();
        Configuration initialConfig = getResources().getConfiguration();
        configureEdgeToEdge(initialConfig);
        pushDisplayMetrics(initialConfig);
        decor.setOnApplyWindowInsetsListener((v, insets) -> {
            int top, bottom, left, right;
            int imeBottom = -1;
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
                Insets bars = insets.getInsets(WindowInsets.Type.systemBars() | WindowInsets.Type.displayCutout());
                top = bars.top;
                bottom = bars.bottom;
                left = bars.left;
                right = bars.right;
                lastNavBarBottomPx = bars.bottom;
                if (!imeAnimating) {
                    Insets ime = insets.getInsets(WindowInsets.Type.ime());
                    imeBottom = Math.max(0, ime.bottom - bars.bottom);
                    imeTargetBottomPx = imeBottom;
                }
            } else {
                top = insets.getSystemWindowInsetTop();
                bottom = insets.getSystemWindowInsetBottom();
                left = insets.getSystemWindowInsetLeft();
                right = insets.getSystemWindowInsetRight();
            }
            float density = getResources().getDisplayMetrics().density;
            if (density <= 0f) density = 1f;
            try {
                nativeInsets(top, bottom, left, right, density);
                if (imeBottom >= 0) {
                    nativeImeInsets(imeBottom, imeBottom, false, density);
                }
            } catch (UnsatisfiedLinkError ignored) {
            }
            return v.onApplyWindowInsets(insets);
        });
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
            installImeAnimationCallback(decor);
        }
        decor.requestApplyInsets();
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.UPSIDE_DOWN_CAKE) {
            backCallback = createPredictiveBackCallback();
        } else if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
            backCallback = createLegacyOnBackInvokedCallback();
        }
        // Keep back callback unregistered at startup on the root inbox so Android 14+
        // plays the native Back-to-Home preview until Rust reports an in-app back target.
        GpuiNotifications.ensureChannels(this);
        maybeRequestNotificationPermission();
        MailstreamFcm.start(this);
        deliverNotificationIntent(getIntent());
    }

    @Override
    protected void onNewIntent(Intent intent) {
        super.onNewIntent(intent);
        setIntent(intent);
        deliverNotificationIntent(intent);
    }

    private void maybeRequestNotificationPermission() {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.TIRAMISU) {
            return;
        }
        if (checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) == PackageManager.PERMISSION_GRANTED) {
            return;
        }
        SharedPreferences prefs = getSharedPreferences(PREFS_PERMS, Context.MODE_PRIVATE);
        if (prefs.getBoolean(KEY_ASKED_NOTIF, false)) {
            return;
        }
        prefs.edit().putBoolean(KEY_ASKED_NOTIF, true).apply();
        requestPermissions(new String[] { Manifest.permission.POST_NOTIFICATIONS }, REQ_POST_NOTIFICATIONS);
    }

    private void deliverNotificationIntent(Intent intent) {
        if (intent == null) {
            return;
        }
        String tag = intent.getStringExtra(GpuiNotifications.EXTRA_NOTIFICATION_TAG);
        if (tag == null || tag.isEmpty()) {
            return;
        }
        String actionId = intent.getStringExtra(GpuiNotifications.EXTRA_NOTIFICATION_ACTION);
        intent.removeExtra(GpuiNotifications.EXTRA_NOTIFICATION_TAG);
        intent.removeExtra(GpuiNotifications.EXTRA_NOTIFICATION_ACTION);
        try {
            nativeNotificationResponse(tag, actionId != null ? actionId : "");
        } catch (UnsatisfiedLinkError ignored) {
        }
    }
    private void installImeAnimationCallback(final View decor) {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.R) {
            return;
        }
        decor.setWindowInsetsAnimationCallback(
            new WindowInsetsAnimation.Callback(WindowInsetsAnimation.Callback.DISPATCH_MODE_STOP) {
                @Override
                public void onPrepare(WindowInsetsAnimation animation) {
                    if ((animation.getTypeMask() & WindowInsets.Type.ime()) != 0) {
                        imeAnimating = true;
                    }
                }

                @Override
                public WindowInsetsAnimation.Bounds onStart(
                        WindowInsetsAnimation animation,
                        WindowInsetsAnimation.Bounds bounds) {
                    if ((animation.getTypeMask() & WindowInsets.Type.ime()) != 0) {
                        imeAnimating = true;
                        WindowInsets root = decor.getRootWindowInsets();
                        boolean opening = root != null && root.isVisible(WindowInsets.Type.ime());
                        int navBottom = lastNavBarBottomPx;
                        if (root != null) {
                            Insets bars = root.getInsets(
                                WindowInsets.Type.systemBars() | WindowInsets.Type.displayCutout());
                            navBottom = bars.bottom;
                            lastNavBarBottomPx = navBottom;
                        }
                        int upper = Math.max(0, bounds.getUpperBound().bottom - navBottom);
                        imeTargetBottomPx = opening ? upper : 0;
                        int initialBottom = opening ? 0 : upper;
                        float density = getResources().getDisplayMetrics().density;
                        if (density <= 0f) density = 1f;
                        try {
                            nativeImeInsets(initialBottom, imeTargetBottomPx, true, density);
                        } catch (UnsatisfiedLinkError ignored) {
                        }
                    }
                    return bounds;
                }

                @Override
                public WindowInsets onProgress(
                        WindowInsets insets,
                        List<WindowInsetsAnimation> runningAnimations) {
                    boolean hasIme = false;
                    for (int i = 0; i < runningAnimations.size(); i++) {
                        if ((runningAnimations.get(i).getTypeMask() & WindowInsets.Type.ime()) != 0) {
                            hasIme = true;
                            break;
                        }
                    }
                    if (hasIme) {
                        Insets bars = insets.getInsets(
                            WindowInsets.Type.systemBars() | WindowInsets.Type.displayCutout());
                        Insets ime = insets.getInsets(WindowInsets.Type.ime());
                        lastNavBarBottomPx = bars.bottom;
                        int imeBottom = Math.max(0, ime.bottom - bars.bottom);
                        float density = getResources().getDisplayMetrics().density;
                        if (density <= 0f) density = 1f;
                        try {
                            nativeImeInsets(imeBottom, imeTargetBottomPx, true, density);
                        } catch (UnsatisfiedLinkError ignored) {
                        }
                    }
                    return insets;
                }

                @Override
                public void onEnd(WindowInsetsAnimation animation) {
                    if ((animation.getTypeMask() & WindowInsets.Type.ime()) != 0) {
                        imeAnimating = false;
                        int finalBottom = 0;
                        WindowInsets root = decor.getRootWindowInsets();
                        if (root != null) {
                            Insets bars = root.getInsets(
                                WindowInsets.Type.systemBars() | WindowInsets.Type.displayCutout());
                            lastNavBarBottomPx = bars.bottom;
                            if (root.isVisible(WindowInsets.Type.ime())) {
                                Insets ime = root.getInsets(WindowInsets.Type.ime());
                                finalBottom = Math.max(0, ime.bottom - bars.bottom);
                            }
                        }
                        imeTargetBottomPx = finalBottom;
                        float density = getResources().getDisplayMetrics().density;
                        if (density <= 0f) density = 1f;
                        try {
                            nativeImeInsets(finalBottom, finalBottom, false, density);
                        } catch (UnsatisfiedLinkError ignored) {
                        }
                    }
                }
            }
        );
    }

    private Object createLegacyOnBackInvokedCallback() {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.TIRAMISU) {
            return null;
        }
        return new OnBackInvokedCallback() {
            @Override
            public void onBackInvoked() {
                try {
                    nativeBack();
                } catch (UnsatisfiedLinkError ignored) {
                }
            }
        };
    }

    private Object createPredictiveBackCallback() {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.UPSIDE_DOWN_CAKE) {
            return null;
        }
        return new OnBackAnimationCallback() {
            @Override
            public void onBackStarted(BackEvent backEvent) {
                try {
                    nativeBackStarted(backEvent.getProgress(), backEvent.getSwipeEdge());
                } catch (UnsatisfiedLinkError ignored) {
                }
            }

            @Override
            public void onBackProgressed(BackEvent backEvent) {
                try {
                    nativeBackProgressed(backEvent.getProgress(), backEvent.getSwipeEdge());
                } catch (UnsatisfiedLinkError ignored) {
                }
            }

            @Override
            public void onBackInvoked() {
                try {
                    nativeBack();
                } catch (UnsatisfiedLinkError ignored) {
                }
            }

            @Override
            public void onBackCancelled() {
                try {
                    nativeBackCancelled();
                } catch (UnsatisfiedLinkError ignored) {
                }
            }
        };
    }

    public void gpuiSetBackEnabled(final boolean enabled) {
        backEnabled = enabled;
        runOnUiThread(() -> {
            if (Build.VERSION.SDK_INT < Build.VERSION_CODES.TIRAMISU || backCallback == null) {
                return;
            }
            if (enabled == backCallbackRegistered) {
                return;
            }
            OnBackInvokedDispatcher dispatcher = getOnBackInvokedDispatcher();
            if (enabled) {
                dispatcher.registerOnBackInvokedCallback(
                    OnBackInvokedDispatcher.PRIORITY_DEFAULT,
                    (OnBackInvokedCallback) backCallback
                );
                backCallbackRegistered = true;
            } else {
                dispatcher.unregisterOnBackInvokedCallback((OnBackInvokedCallback) backCallback);
                backCallbackRegistered = false;
            }
        });
    }

    @Override
    public void onConfigurationChanged(Configuration newConfig) {
        super.onConfigurationChanged(newConfig);
        configureEdgeToEdge(newConfig);
        pushDisplayMetrics(newConfig);
    }

    @Override
    public void onTrimMemory(int level) {
        super.onTrimMemory(level);
        try {
            nativeTrimMemory(level);
        } catch (UnsatisfiedLinkError ignored) {
        }
    }

    private void pushDisplayMetrics(Configuration config) {
        float density = getResources().getDisplayMetrics().density;
        if (density <= 0f) density = 1f;
        float fontScale = config != null ? config.fontScale : 1f;
        if (fontScale <= 0f) fontScale = 1f;
        try {
            nativeDisplayMetrics(density, fontScale);
        } catch (UnsatisfiedLinkError ignored) {
        }
    }

    @SuppressWarnings("deprecation")
    private void configureEdgeToEdge(Configuration config) {
        Window window = getWindow();
        if (window == null) {
            return;
        }
        boolean night = (config.uiMode & Configuration.UI_MODE_NIGHT_MASK) == Configuration.UI_MODE_NIGHT_YES;
        // Warm linen (light) / warm cocoa (dark) matching GPUI Palette::tray so launch and bars never flash.
        window.getDecorView().setBackgroundColor(night ? 0xFF1C1815 : 0xFFF8F3EC);
        window.addFlags(WindowManager.LayoutParams.FLAG_DRAWS_SYSTEM_BAR_BACKGROUNDS);
        window.setStatusBarColor(Color.TRANSPARENT);
        window.setNavigationBarColor(Color.TRANSPARENT);
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
            window.setStatusBarContrastEnforced(false);
            window.setNavigationBarContrastEnforced(false);
        }
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
            window.setDecorFitsSystemWindows(false);
            WindowInsetsController controller = window.getInsetsController();
            if (controller != null) {
                int lightFlags = WindowInsetsController.APPEARANCE_LIGHT_STATUS_BARS
                        | WindowInsetsController.APPEARANCE_LIGHT_NAVIGATION_BARS;
                controller.setSystemBarsAppearance(night ? 0 : lightFlags, lightFlags);
            }
        }
    }
    @Override
    @SuppressWarnings("deprecation")
    public void onBackPressed() {
        if (!backEnabled) {
            super.onBackPressed();
            return;
        }
        try {
            nativeBack();
        } catch (UnsatisfiedLinkError ignored) {
            super.onBackPressed();
        }
    }

    public void gpuiShowKeyboard(final int kind, final long session) {
        runOnUiThread(() -> {
            imeSession = session;
            inputKind = kind;
            if (imeProxy == null) {
                return;
            }
            imeProxy.setInputType(androidInputType(kind));
            imeProxy.requestFocus();
            InputMethodManager imm = (InputMethodManager) getSystemService(Context.INPUT_METHOD_SERVICE);
            if (imm != null) {
                imm.restartInput(imeProxy);
                imm.showSoftInput(imeProxy, InputMethodManager.SHOW_IMPLICIT);
            }
        });
    }

    public void gpuiHideKeyboard(final long session) {
        runOnUiThread(() -> {
            imeSession = session;
            if (imeProxy == null) {
                return;
            }
            InputMethodManager imm = (InputMethodManager) getSystemService(Context.INPUT_METHOD_SERVICE);
            if (imm != null) {
                imm.hideSoftInputFromWindow(imeProxy.getWindowToken(), 0);
            }
            imeProxy.clearFocus();
        });
    }

    public void gpuiResetComposition(final long session) {
        runOnUiThread(() -> {
            imeSession = session;
            if (imeProxy == null) {
                return;
            }
            InputMethodManager imm = (InputMethodManager) getSystemService(Context.INPUT_METHOD_SERVICE);
            if (imm != null) {
                imm.restartInput(imeProxy);
            }
        });
    }

    private static int androidInputType(int kind) {
        switch (kind) {
            case 1:
                return InputType.TYPE_CLASS_TEXT | InputType.TYPE_TEXT_VARIATION_EMAIL_ADDRESS;
            case 2:
                return InputType.TYPE_CLASS_PHONE;
            case 3:
                return InputType.TYPE_CLASS_NUMBER;
            case 4:
                return InputType.TYPE_CLASS_TEXT | InputType.TYPE_TEXT_VARIATION_URI;
            case 5:
                return InputType.TYPE_CLASS_NUMBER | InputType.TYPE_NUMBER_FLAG_DECIMAL;
            default:
                return InputType.TYPE_CLASS_TEXT;
        }
    }

    private final class ImeProxyView extends EditText {
        ImeProxyView(Context context) {
            super(context);
        }

        @Override
        public boolean onCheckIsTextEditor() {
            return true;
        }

        @Override
        public InputConnection onCreateInputConnection(EditorInfo outAttrs) {
            outAttrs.inputType = androidInputType(inputKind);
            outAttrs.imeOptions = EditorInfo.IME_FLAG_NO_EXTRACT_UI;
            return new BaseInputConnection(this, false) {
                @Override
                public boolean setComposingText(CharSequence text, int newCursorPosition) {
                    String s = text != null ? text.toString() : "";
                    nativeIme(imeSession, IME_COMPOSING, s, s.length(), s.length());
                    return true;
                }

                @Override
                public boolean commitText(CharSequence text, int newCursorPosition) {
                    String s = text != null ? text.toString() : "";
                    nativeIme(imeSession, IME_COMMIT, s, 0, 0);
                    return true;
                }

                @Override
                public boolean deleteSurroundingText(int beforeLength, int afterLength) {
                    nativeIme(imeSession, IME_DELETE_SURROUNDING, "", beforeLength, afterLength);
                    return true;
                }

                @Override
                public boolean deleteSurroundingTextInCodePoints(int beforeLength, int afterLength) {
                    nativeIme(imeSession, IME_DELETE_CODE_POINTS, "", beforeLength, afterLength);
                    return true;
                }

                @Override
                public boolean finishComposingText() {
                    nativeIme(imeSession, IME_COMMIT, "", 0, 0);
                    return true;
                }

                @Override
                public boolean sendKeyEvent(KeyEvent event) {
                    if (event != null && event.getAction() == KeyEvent.ACTION_DOWN) {
                        int code = event.getKeyCode();
                        if (code == KeyEvent.KEYCODE_DEL) {
                            nativeIme(imeSession, IME_DELETE_SURROUNDING, "", 1, 0);
                            return true;
                        }
                        if (code == KeyEvent.KEYCODE_ENTER) {
                            nativeIme(imeSession, IME_COMMIT, "\n", 0, 0);
                            return true;
                        }
                        if (code == KeyEvent.KEYCODE_ESCAPE) {
                            nativeIme(imeSession, IME_ESCAPE, "", 0, 0);
                            return true;
                        }
                    }
                    return super.sendKeyEvent(event);
                }
            };
        }
    }
}
