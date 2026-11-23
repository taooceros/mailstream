package dev.gpui.mobile;

import android.Manifest;
import android.app.NativeActivity;
import android.content.Context;
import android.content.Intent;
import android.content.SharedPreferences;
import android.content.pm.PackageManager;
import android.os.Bundle;
import android.net.ConnectivityManager;
import android.net.Network;
import android.net.NetworkCapabilities;
import android.os.SystemClock;
import android.text.Editable;
import android.text.InputType;
import android.text.Selection;
import android.text.SpannableStringBuilder;
import android.content.res.Configuration;
import android.graphics.Color;
import android.graphics.Insets;
import android.os.Build;
import android.util.Log;
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
import android.widget.FrameLayout;
import java.util.List;
/**
 * Minimal NativeActivity host bridging Android IME (including CJK composing text) to gpui-mobile's
 * {@code Java_dev_gpui_mobile_GpuiInputActivity_nativeIme} JNI entry point.
 */
public class GpuiInputActivity extends NativeActivity {
    private static final String TAG = "mailstream";
    private static final int IME_COMPOSING = 0;
    private static final int IME_COMMIT = 1;
    private static final int IME_DELETE_SURROUNDING = 2;
    private static final int IME_DELETE_CODE_POINTS = 3;
    private static final int IME_ESCAPE = 4;
    private static final int IME_FINISH_COMPOSING = 5;
    private static final int IME_KEY_ENTER = 6;
    private static final int IME_KEY_BACKSPACE = 7;
    private static final int IME_KEY_FORWARD_DELETE = 8;
    private static final int IME_KEY_TAB = 9;
    private static final int IME_KEY_NAV = 10;
    private static final int IME_SET_SELECTION = 11;
    private static final int IME_SET_COMPOSING_REGION = 12;
    private static final int REQ_POST_NOTIFICATIONS = 1001;
    private static final long NETWORK_NOTIFY_DEBOUNCE_MS = 500L;
    private static final String PREFS_PERMS = "mailstream_perm_prefs";
    private static final String KEY_ASKED_NOTIF = "asked_post_notifications";
    private volatile long imeSession = 0;
    private int inputKind = 0;
    private int inputImeAction = 0;
    private int inputFlags = 0;
    private boolean keyboardRequested = false;
    private ImeProxyView imeProxy;
    private volatile boolean backEnabled = false;
    private Object backCallback = null;
    private boolean backCallbackRegistered = false;
    private boolean imeAnimating = false;
    private int lastNavBarBottomPx = 0;
    private int imeTargetBottomPx = 0;
    private volatile boolean activityForeground = false;
    private final Object networkLock = new Object();
    private ConnectivityManager.NetworkCallback networkCallback = null;
    private boolean seenInitialNetwork = false;
    private boolean networkLostSinceLast = false;
    private long activeNetworkHandle = -1L;
    private int activeTransportMask = 0;
    private boolean activeValidated = false;
    private long lastNetworkNotifyElapsedMs = 0L;

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
    private static native void nativeSetForeground(boolean foreground);
    private static native void nativeNetworkChanged();

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        imeProxy = new ImeProxyView(this);
        imeProxy.setVisibility(View.VISIBLE);
        imeProxy.setFocusable(true);
        imeProxy.setFocusableInTouchMode(true);
        imeProxy.setClickable(false);
        imeProxy.setLongClickable(false);
        imeProxy.setWillNotDraw(true);
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
    protected void onStart() {
        super.onStart();
        activityForeground = true;
        try {
            nativeSetForeground(true);
        } catch (UnsatisfiedLinkError ignored) {
        }
        registerNetworkCallback();
    }

    @Override
    protected void onStop() {
        activityForeground = false;
        unregisterNetworkCallback();
        try {
            nativeSetForeground(false);
        } catch (UnsatisfiedLinkError ignored) {
        }
        super.onStop();
    }

    @Override
    protected void onDestroy() {
        activityForeground = false;
        unregisterNetworkCallback();
        super.onDestroy();
    }

    private void registerNetworkCallback() {
        synchronized (networkLock) {
            if (networkCallback != null) {
                return;
            }
            final ConnectivityManager cm =
                (ConnectivityManager) getApplicationContext().getSystemService(Context.CONNECTIVITY_SERVICE);
            if (cm == null) {
                return;
            }
            seenInitialNetwork = false;
            networkLostSinceLast = false;
            activeNetworkHandle = -1L;
            activeTransportMask = 0;
            activeValidated = false;
            ConnectivityManager.NetworkCallback cb = new ConnectivityManager.NetworkCallback() {
                @Override
                public void onAvailable(Network network) {
                    if (network == null) {
                        return;
                    }
                    NetworkCapabilities caps = null;
                    try {
                        caps = cm.getNetworkCapabilities(network);
                    } catch (Throwable ignored) {
                    }
                    handleNetworkUpdate(network.getNetworkHandle(), caps, true);
                }

                @Override
                public void onCapabilitiesChanged(Network network, NetworkCapabilities caps) {
                    if (network == null || caps == null) {
                        return;
                    }
                    handleNetworkUpdate(network.getNetworkHandle(), caps, false);
                }

                @Override
                public void onLost(Network network) {
                    synchronized (networkLock) {
                        if (network == null || activeNetworkHandle == -1L
                                || network.getNetworkHandle() == activeNetworkHandle) {
                            networkLostSinceLast = true;
                            activeNetworkHandle = -1L;
                            activeTransportMask = 0;
                            activeValidated = false;
                        }
                    }
                }
            };
            try {
                cm.registerDefaultNetworkCallback(cb);
                networkCallback = cb;
            } catch (Throwable t) {
                Log.w(TAG, "Failed to register ConnectivityManager callback", t);
            }
        }
    }

    private void unregisterNetworkCallback() {
        synchronized (networkLock) {
            ConnectivityManager.NetworkCallback cb = networkCallback;
            networkCallback = null;
            if (cb == null) {
                return;
            }
            try {
                ConnectivityManager cm =
                    (ConnectivityManager) getApplicationContext().getSystemService(Context.CONNECTIVITY_SERVICE);
                if (cm != null) {
                    cm.unregisterNetworkCallback(cb);
                }
            } catch (Throwable t) {
                Log.w(TAG, "Failed to unregister ConnectivityManager callback", t);
            }
        }
    }

    private void handleNetworkUpdate(long handle, NetworkCapabilities caps, boolean fromAvailable) {
        int transportMask = computeTransportMask(caps);
        boolean validated = caps != null
            && caps.hasCapability(NetworkCapabilities.NET_CAPABILITY_VALIDATED);
        boolean shouldNotify = false;
        synchronized (networkLock) {
            if (!seenInitialNetwork) {
                seenInitialNetwork = true;
                networkLostSinceLast = false;
                activeNetworkHandle = handle;
                if (caps != null) {
                    activeTransportMask = transportMask;
                    activeValidated = validated;
                }
                return;
            }
            boolean handleChanged = activeNetworkHandle != -1L && handle != activeNetworkHandle;
            boolean recoveredAfterLoss = networkLostSinceLast;
            boolean transportChanged = caps != null && activeTransportMask != 0 && transportMask != activeTransportMask;
            boolean becameValidated = caps != null && !activeValidated && validated;
            if (handleChanged || recoveredAfterLoss || transportChanged || becameValidated) {
                long now = SystemClock.elapsedRealtime();
                if (now - lastNetworkNotifyElapsedMs >= NETWORK_NOTIFY_DEBOUNCE_MS) {
                    lastNetworkNotifyElapsedMs = now;
                    shouldNotify = activityForeground;
                }
            }
            if (fromAvailable || handle != activeNetworkHandle) {
                activeNetworkHandle = handle;
            }
            networkLostSinceLast = false;
            if (caps != null) {
                activeTransportMask = transportMask;
                activeValidated = validated;
            }
        }
        if (shouldNotify) {
            try {
                nativeNetworkChanged();
            } catch (UnsatisfiedLinkError ignored) {
            }
        }
    }

    private static int computeTransportMask(NetworkCapabilities caps) {
        if (caps == null) {
            return 0;
        }
        int mask = 0;
        if (caps.hasTransport(NetworkCapabilities.TRANSPORT_WIFI)) mask |= 1;
        if (caps.hasTransport(NetworkCapabilities.TRANSPORT_CELLULAR)) mask |= 2;
        if (caps.hasTransport(NetworkCapabilities.TRANSPORT_ETHERNET)) mask |= 4;
        if (caps.hasTransport(NetworkCapabilities.TRANSPORT_VPN)) mask |= 8;
        return mask;
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

    @Override
    public void onWindowFocusChanged(boolean hasFocus) {
        super.onWindowFocusChanged(hasFocus);
        if (hasFocus && keyboardRequested && imeProxy != null) {
            imeProxy.post(() -> {
                if (keyboardRequested) {
                    showImeNow(false);
                }
            });
        }
    }

    public void gpuiShowKeyboard(final int kind, final long session) {
        gpuiShowKeyboard(kind, 0, 0, session, true);
    }

    public void gpuiShowKeyboard(
            final int kind,
            final int imeAction,
            final int flags,
            final long session,
            final boolean restart) {
        runOnUiThread(() -> {
            boolean configChanged = (inputKind != kind)
                    || (inputImeAction != imeAction)
                    || (inputFlags != flags);
            imeSession = session;
            inputKind = kind;
            inputImeAction = imeAction;
            inputFlags = flags;
            keyboardRequested = true;
            if (imeProxy == null) {
                return;
            }
            if (restart) {
                imeProxy.clearMirror();
            }
            showImeNow(restart || configChanged);
        });
    }

    public void gpuiUpdateInputState(
            final long session,
            final String text,
            final int selStart,
            final int selEnd,
            final int composingStart,
            final int composingEnd) {
        runOnUiThread(() -> {
            if (imeProxy == null || session != imeSession) {
                return;
            }
            String safeText = text != null ? text : "";
            if (!imeProxy.syncMirror(safeText, selStart, selEnd, composingStart, composingEnd)) {
                return;
            }
            InputMethodManager imm = (InputMethodManager) getSystemService(Context.INPUT_METHOD_SERVICE);
            if (imm != null && imm.isActive(imeProxy)) {
                Editable ed = imeProxy.getMirrorEditable();
                int sStart = Selection.getSelectionStart(ed);
                int sEnd = Selection.getSelectionEnd(ed);
                int cStart = BaseInputConnection.getComposingSpanStart(ed);
                int cEnd = BaseInputConnection.getComposingSpanEnd(ed);
                imm.updateSelection(imeProxy, sStart, sEnd, cStart, cEnd);
            }
        });
    }

    public void gpuiHideKeyboard(final long session) {
        runOnUiThread(() -> {
            imeSession = session;
            keyboardRequested = false;
            if (imeProxy == null) {
                return;
            }
            imeProxy.clearComposing();
            InputMethodManager imm = (InputMethodManager) getSystemService(Context.INPUT_METHOD_SERVICE);
            if (imm != null) {
                imm.hideSoftInputFromWindow(imeProxy.getWindowToken(), 0);
            }
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
                WindowInsetsController controller = imeProxy.getWindowInsetsController();
                if (controller != null) {
                    controller.hide(WindowInsets.Type.ime());
                }
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
            imeProxy.clearComposing();
            InputMethodManager imm = (InputMethodManager) getSystemService(Context.INPUT_METHOD_SERVICE);
            if (imm != null && imm.isActive(imeProxy)) {
                imm.restartInput(imeProxy);
            }
        });
    }

    private void showImeNow(final boolean restart) {
        if (imeProxy == null) {
            return;
        }
        if (!imeProxy.hasFocus()) {
            imeProxy.requestFocus();
        }
        InputMethodManager imm = (InputMethodManager) getSystemService(Context.INPUT_METHOD_SERVICE);
        boolean shown = false;
        if (imm != null) {
            if (restart && imm.isActive(imeProxy)) {
                imm.restartInput(imeProxy);
            }
            shown = imm.showSoftInput(imeProxy, 0);
        }
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
            WindowInsetsController controller = imeProxy.getWindowInsetsController();
            if (controller != null) {
                controller.show(WindowInsets.Type.ime());
            }
        }
        // On Android 11-15, `requestFocus()` binds `ImeFocusController` asynchronously
        // on the next traversal; if `showSoftInput` returned false before focus settled,
        // retry once posted on the view's message queue (and again after a short delay).
        if (!shown) {
            imeProxy.post(() -> {
                if (!keyboardRequested || imeProxy == null) {
                    return;
                }
                if (!imeProxy.hasFocus()) {
                    imeProxy.requestFocus();
                }
                InputMethodManager retryImm =
                        (InputMethodManager) getSystemService(Context.INPUT_METHOD_SERVICE);
                boolean retryShown = false;
                if (retryImm != null) {
                    if (restart && retryImm.isActive(imeProxy)) {
                        retryImm.restartInput(imeProxy);
                    }
                    retryShown = retryImm.showSoftInput(imeProxy, 0);
                }
                if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
                    WindowInsetsController controller = imeProxy.getWindowInsetsController();
                    if (controller != null) {
                        controller.show(WindowInsets.Type.ime());
                    }
                }
                if (!retryShown) {
                    imeProxy.postDelayed(() -> {
                        if (!keyboardRequested || imeProxy == null) {
                            return;
                        }
                        if (!imeProxy.hasFocus()) {
                            imeProxy.requestFocus();
                        }
                        InputMethodManager lateImm =
                                (InputMethodManager) getSystemService(Context.INPUT_METHOD_SERVICE);
                        if (lateImm != null) {
                            lateImm.showSoftInput(imeProxy, 0);
                        }
                        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
                            WindowInsetsController controller = imeProxy.getWindowInsetsController();
                            if (controller != null) {
                                controller.show(WindowInsets.Type.ime());
                            }
                        }
                    }, 60L);
                }
            });
        }
    }

    private static int androidInputType(int kind, int flags) {
        int base;
        switch (kind) {
            case 1:
                base = InputType.TYPE_CLASS_TEXT | InputType.TYPE_TEXT_VARIATION_EMAIL_ADDRESS;
                break;
            case 2:
                return InputType.TYPE_CLASS_PHONE;
            case 3:
                return InputType.TYPE_CLASS_NUMBER;
            case 4:
                base = InputType.TYPE_CLASS_TEXT | InputType.TYPE_TEXT_VARIATION_URI;
                break;
            case 5:
                return InputType.TYPE_CLASS_NUMBER | InputType.TYPE_NUMBER_FLAG_DECIMAL;
            default:
                base = InputType.TYPE_CLASS_TEXT;
                break;
        }
        if ((flags & 1) != 0) {
            base |= InputType.TYPE_TEXT_FLAG_AUTO_CORRECT;
        }
        int cap = (flags >> 2) & 3;
        if (cap == 1) {
            base |= InputType.TYPE_TEXT_FLAG_CAP_WORDS;
        } else if (cap == 2) {
            base |= InputType.TYPE_TEXT_FLAG_CAP_SENTENCES;
        } else if (cap == 3) {
            base |= InputType.TYPE_TEXT_FLAG_CAP_CHARACTERS;
        }
        return base;
    }

    private static int androidImeOptions(int action) {
        int actionFlag;
        switch (action) {
            case 1:
                actionFlag = EditorInfo.IME_ACTION_NONE;
                break;
            case 2:
                actionFlag = EditorInfo.IME_ACTION_DONE;
                break;
            case 3:
                actionFlag = EditorInfo.IME_ACTION_GO;
                break;
            case 4:
                actionFlag = EditorInfo.IME_ACTION_NEXT;
                break;
            case 5:
                actionFlag = EditorInfo.IME_ACTION_PREVIOUS;
                break;
            case 6:
                actionFlag = EditorInfo.IME_ACTION_SEARCH;
                break;
            case 7:
                actionFlag = EditorInfo.IME_ACTION_SEND;
                break;
            default:
                actionFlag = EditorInfo.IME_ACTION_UNSPECIFIED;
                break;
        }
        return actionFlag
                | EditorInfo.IME_FLAG_NO_EXTRACT_UI
                | EditorInfo.IME_FLAG_NO_FULLSCREEN;
    }

    private void sendNativeIme(int kind, String text, int start, int end) {
        try {
            nativeIme(imeSession, kind, text, start, end);
        } catch (UnsatisfiedLinkError e) {
            Log.e(TAG, "nativeIme link error", e);
        }
    }

    private final class ImeProxyView extends View {
        private final SpannableStringBuilder editable = new SpannableStringBuilder();

        ImeProxyView(Context context) {
            super(context);
            Selection.setSelection(editable, 0, 0);
        }

        Editable getMirrorEditable() {
            return editable;
        }

        void clearMirror() {
            editable.clear();
            BaseInputConnection.removeComposingSpans(editable);
            Selection.setSelection(editable, 0, 0);
        }

        void clearComposing() {
            BaseInputConnection.removeComposingSpans(editable);
        }

        boolean syncMirror(String text, int selStart, int selEnd, int composingStart, int composingEnd) {
            int len = text.length();
            int clampedSelStart = Math.max(0, Math.min(selStart, len));
            int clampedSelEnd = Math.max(clampedSelStart, Math.min(selEnd, len));
            int clampedCompStart = composingStart >= 0 ? Math.max(0, Math.min(composingStart, len)) : -1;
            int clampedCompEnd = composingEnd > clampedCompStart
                    ? Math.max(clampedCompStart, Math.min(composingEnd, len))
                    : -1;
            if (clampedCompEnd <= clampedCompStart) {
                clampedCompStart = -1;
                clampedCompEnd = -1;
            }

            boolean sameText = editable.toString().equals(text);
            boolean sameSel = Selection.getSelectionStart(editable) == clampedSelStart
                    && Selection.getSelectionEnd(editable) == clampedSelEnd;
            boolean sameComp = BaseInputConnection.getComposingSpanStart(editable) == clampedCompStart
                    && BaseInputConnection.getComposingSpanEnd(editable) == clampedCompEnd;
            if (sameText && sameSel && (sameComp || clampedCompStart >= 0)) {
                return false;
            }

            if (!sameText) {
                editable.replace(0, editable.length(), text);
            }
            BaseInputConnection.removeComposingSpans(editable);
            Selection.setSelection(editable, clampedSelStart, clampedSelEnd);
            return true;
        }

        @Override
        public boolean onCheckIsTextEditor() {
            return true;
        }

        @Override
        public InputConnection onCreateInputConnection(EditorInfo outAttrs) {
            outAttrs.inputType = androidInputType(inputKind, inputFlags);
            outAttrs.imeOptions = androidImeOptions(inputImeAction);
            int selStart = Math.max(0, Selection.getSelectionStart(editable));
            int selEnd = Math.max(selStart, Selection.getSelectionEnd(editable));
            outAttrs.initialSelStart = selStart;
            outAttrs.initialSelEnd = selEnd;
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
                outAttrs.setInitialSurroundingText(editable);
            }
            return new BaseInputConnection(this, true) {
                @Override
                public Editable getEditable() {
                    return editable;
                }

                @Override
                public boolean setComposingText(CharSequence text, int newCursorPosition) {
                    super.setComposingText(text, newCursorPosition);
                    String s = text != null ? text.toString() : "";
                    int cursor = newCursorPosition <= 0 ? 0 : s.length();
                    sendNativeIme(IME_COMPOSING, s, cursor, cursor);
                    return true;
                }

                @Override
                public boolean setComposingRegion(int start, int end) {
                    super.setComposingRegion(start, end);
                    int s = Math.max(0, Math.min(start, end));
                    int e = Math.max(s, Math.max(start, end));
                    sendNativeIme(IME_SET_COMPOSING_REGION, "", s, e);
                    return true;
                }

                @Override
                public boolean commitText(CharSequence text, int newCursorPosition) {
                    super.commitText(text, newCursorPosition);
                    String s = text != null ? text.toString() : "";
                    sendNativeIme(IME_COMMIT, s, 0, 0);
                    return true;
                }

                @Override
                public boolean deleteSurroundingText(int beforeLength, int afterLength) {
                    super.deleteSurroundingText(beforeLength, afterLength);
                    sendNativeIme(
                            IME_DELETE_SURROUNDING,
                            "",
                            Math.max(0, beforeLength),
                            Math.max(0, afterLength));
                    return true;
                }

                @Override
                public boolean deleteSurroundingTextInCodePoints(int beforeLength, int afterLength) {
                    if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.N) {
                        super.deleteSurroundingTextInCodePoints(beforeLength, afterLength);
                    }
                    sendNativeIme(
                            IME_DELETE_CODE_POINTS,
                            "",
                            Math.max(0, beforeLength),
                            Math.max(0, afterLength));
                    return true;
                }

                @Override
                public boolean finishComposingText() {
                    super.finishComposingText();
                    // Keep committed composing text intact — only clear the marked span.
                    sendNativeIme(IME_FINISH_COMPOSING, "", 0, 0);
                    return true;
                }

                @Override
                public boolean setSelection(int start, int end) {
                    super.setSelection(start, end);
                    int s = Math.max(0, start);
                    int e = Math.max(0, end);
                    sendNativeIme(IME_SET_SELECTION, "", s, e);
                    return true;
                }

                @Override
                public boolean performEditorAction(int actionCode) {
                    super.finishComposingText();
                    if (actionCode == EditorInfo.IME_ACTION_NEXT) {
                        sendNativeIme(IME_KEY_TAB, "", 0, 0);
                        return true;
                    }
                    if (actionCode == EditorInfo.IME_ACTION_PREVIOUS) {
                        sendNativeIme(IME_KEY_TAB, "", 1, 0);
                        return true;
                    }
                    sendNativeIme(IME_KEY_ENTER, "", 0, 0);
                    return true;
                }

                private boolean isHandledKeyCode(int code) {
                    switch (code) {
                        case KeyEvent.KEYCODE_DEL:
                        case KeyEvent.KEYCODE_FORWARD_DEL:
                        case KeyEvent.KEYCODE_ENTER:
                        case KeyEvent.KEYCODE_NUMPAD_ENTER:
                        case KeyEvent.KEYCODE_ESCAPE:
                        case KeyEvent.KEYCODE_TAB:
                        case KeyEvent.KEYCODE_DPAD_LEFT:
                        case KeyEvent.KEYCODE_DPAD_RIGHT:
                        case KeyEvent.KEYCODE_DPAD_UP:
                        case KeyEvent.KEYCODE_DPAD_DOWN:
                        case KeyEvent.KEYCODE_MOVE_HOME:
                        case KeyEvent.KEYCODE_MOVE_END:
                            return true;
                        default:
                            return false;
                    }
                }

                @Override
                public boolean sendKeyEvent(KeyEvent event) {
                    if (event == null) {
                        return false;
                    }
                    int action = event.getAction();
                    int code = event.getKeyCode();
                    if (action == KeyEvent.ACTION_MULTIPLE) {
                        String chars = event.getCharacters();
                        if (chars != null && !chars.isEmpty()) {
                            super.commitText(chars, 1);
                            sendNativeIme(IME_COMMIT, chars, 0, 0);
                            return true;
                        }
                    }
                    if (action == KeyEvent.ACTION_DOWN) {
                        if (code == KeyEvent.KEYCODE_DEL) {
                            int s = Selection.getSelectionStart(editable);
                            int e = Selection.getSelectionEnd(editable);
                            if (s >= 0 && e >= 0) {
                                int a = Math.min(s, e);
                                int b = Math.max(s, e);
                                if (a != b) {
                                    editable.delete(a, b);
                                } else if (a > 0) {
                                    int prev = Character.isLowSurrogate(editable.charAt(a - 1)) && a > 1
                                            ? a - 2
                                            : a - 1;
                                    editable.delete(prev, a);
                                }
                            }
                            BaseInputConnection.removeComposingSpans(editable);
                            sendNativeIme(IME_KEY_BACKSPACE, "", 0, 0);
                            return true;
                        }
                        if (code == KeyEvent.KEYCODE_FORWARD_DEL) {
                            sendNativeIme(IME_KEY_FORWARD_DELETE, "", 0, 0);
                            return true;
                        }
                        if (code == KeyEvent.KEYCODE_ENTER || code == KeyEvent.KEYCODE_NUMPAD_ENTER) {
                            super.finishComposingText();
                            sendNativeIme(IME_KEY_ENTER, "", 0, 0);
                            return true;
                        }
                        if (code == KeyEvent.KEYCODE_ESCAPE) {
                            super.finishComposingText();
                            sendNativeIme(IME_ESCAPE, "", 0, 0);
                            return true;
                        }
                        if (code == KeyEvent.KEYCODE_TAB) {
                            super.finishComposingText();
                            sendNativeIme(IME_KEY_TAB, "", event.isShiftPressed() ? 1 : 0, 0);
                            return true;
                        }
                        int nav = -1;
                        if (code == KeyEvent.KEYCODE_DPAD_LEFT) nav = 0;
                        else if (code == KeyEvent.KEYCODE_DPAD_RIGHT) nav = 1;
                        else if (code == KeyEvent.KEYCODE_DPAD_UP) nav = 2;
                        else if (code == KeyEvent.KEYCODE_DPAD_DOWN) nav = 3;
                        else if (code == KeyEvent.KEYCODE_MOVE_HOME) nav = 4;
                        else if (code == KeyEvent.KEYCODE_MOVE_END) nav = 5;
                        if (nav >= 0) {
                            super.finishComposingText();
                            sendNativeIme(IME_KEY_NAV, "", nav, 0);
                            return true;
                        }
                        if (!event.isCtrlPressed() && !event.isMetaPressed() && !event.isAltPressed()) {
                            int unicode = event.getUnicodeChar();
                            if (unicode > 0 && !Character.isISOControl(unicode)) {
                                String s = new String(Character.toChars(unicode));
                                super.commitText(s, 1);
                                sendNativeIme(IME_COMMIT, s, 0, 0);
                                return true;
                            }
                        }
                    }
                    if (action == KeyEvent.ACTION_UP) {
                        if (isHandledKeyCode(code)) {
                            return true;
                        }
                        if (!event.isCtrlPressed() && !event.isMetaPressed() && !event.isAltPressed()) {
                            int unicode = event.getUnicodeChar();
                            if (unicode > 0 && !Character.isISOControl(unicode)) {
                                return true;
                            }
                        }
                    }
                    return super.sendKeyEvent(event);
                }
            };
        }
    }
}
