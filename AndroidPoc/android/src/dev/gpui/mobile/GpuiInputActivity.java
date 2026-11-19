package dev.gpui.mobile;

import android.app.NativeActivity;
import android.content.Context;
import android.os.Bundle;
import android.text.InputType;
import android.content.res.Configuration;
import android.graphics.Insets;
import android.os.Build;
import android.view.KeyEvent;
import android.view.ViewGroup;
import android.view.inputmethod.BaseInputConnection;
import android.view.inputmethod.EditorInfo;
import android.view.inputmethod.InputConnection;
import android.view.View;
import android.view.WindowInsets;
import android.view.WindowInsetsController;
import android.view.inputmethod.InputMethodManager;
import android.widget.EditText;
import android.widget.FrameLayout;

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

    private volatile long imeSession = 0;
    private int inputKind = 0;
    private ImeProxyView imeProxy;

    static {
        System.loadLibrary("mailstream_android_poc");
    }

    private static native void nativeIme(long session, int kind, String text, int start, int end);
    private static native void nativeInsets(int top, int bottom, int left, int right, float density);

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
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
            boolean night = (getResources().getConfiguration().uiMode & Configuration.UI_MODE_NIGHT_MASK)
                    == Configuration.UI_MODE_NIGHT_YES;
            WindowInsetsController controller = getWindow().getInsetsController();
            if (controller != null) {
                int lightFlags = WindowInsetsController.APPEARANCE_LIGHT_STATUS_BARS
                        | WindowInsetsController.APPEARANCE_LIGHT_NAVIGATION_BARS;
                controller.setSystemBarsAppearance(night ? 0 : lightFlags, lightFlags);
            }
        }
        decor.setOnApplyWindowInsetsListener((v, insets) -> {
            int top, bottom, left, right;
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
                Insets bars = insets.getInsets(WindowInsets.Type.systemBars() | WindowInsets.Type.displayCutout());
                top = bars.top;
                bottom = bars.bottom;
                left = bars.left;
                right = bars.right;
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
            } catch (UnsatisfiedLinkError ignored) {
            }
            return v.onApplyWindowInsets(insets);
        });
        decor.requestApplyInsets();
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
