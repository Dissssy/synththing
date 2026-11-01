package nl.p51.synththing;

import android.annotation.TargetApi;
import android.content.Context;
import android.os.Build;
import android.text.Editable;
import android.text.InputType;
import android.text.Selection;
import android.text.SpannableStringBuilder;
import android.view.View;
import android.view.WindowInsets;
import android.view.inputmethod.BaseInputConnection;
import android.view.inputmethod.EditorInfo;
import android.view.inputmethod.InputConnection;
import android.view.inputmethod.InputMethodManager;
import android.view.inputmethod.TextAttribute;

/**
 * The keyboard's way into the app's text fields (src/android/keyboard.rs):
 * a view with an input connection, holding a copy of the field being
 * edited, so the keyboard can autocorrect, predict and gesture type in it.
 * (NativeActivity's own view has no input connection: the keyboard only
 * sends key presses to it.) The view is never drawn; the Rust app draws
 * the field.
 *
 * Rust tells it what the field holds ({@link #edit}), numbered; each edit
 * the keyboard makes goes back whole ({@code Native.typed}) with the number
 * it was made on. Positions on the Rust side are in characters (code
 * points), as egui counts them.
 */
final class KeyboardView extends View {
    private final InputMethodManager keyboard;
    private final SpannableStringBuilder text = new SpannableStringBuilder();
    private boolean editing;
    private boolean password;
    /** The keyboard has the field: it made a connection since the last
     * restart, so a request to show can't land before the field does. */
    private boolean connected;
    /** Rust asked to show the keyboard before the keyboard had the field:
     * it shows once the connection is made. */
    private boolean showPending;
    /** The number of what Rust last said the field holds. */
    private int revision;

    // What Rust has of the field, and what the keyboard was last told.
    private String sentText = "";
    private int sentStart, sentEnd;
    private int composingStart = -1, composingEnd = -1;

    KeyboardView(Context context) {
        super(context);
        keyboard = context.getSystemService(InputMethodManager.class);
        setFocusable(true);
        setFocusableInTouchMode(true);
    }

    /** From Rust: the field holds {@code value}, selected from {@code start}
     * to {@code end}; {@code restart}: the keyboard starts over with it (a
     * new field, or text it didn't type), rather than the selection just
     * moving. */
    void edit(int revision, String value, int start, int end, boolean password, boolean restart) {
        this.revision = revision;
        editing = true;
        int selectionStart = offset(value, start);
        int selectionEnd = offset(value, end);
        if (restart || password != this.password || !value.contentEquals(text)) {
            this.password = password;
            text.clearSpans();
            text.replace(0, text.length(), value);
            Selection.setSelection(text, selectionStart, selectionEnd);
            remember();
            connected = false;
            keyboard.restartInput(this);
        } else {
            BaseInputConnection.removeComposingSpans(text);
            Selection.setSelection(text, selectionStart, selectionEnd);
            remember();
            keyboard.updateSelection(this, selectionStart, selectionEnd, -1, -1);
        }
    }

    /**
     * From Rust: show the keyboard (a field's being edited). Android drops
     * a request to show for a view the keyboard isn't connected to yet,
     * which is what a field's first tap is (the view only now gains focus,
     * or was just restarted with the new field): so until the connection
     * is made, the request waits for it ({@link #onCreateInputConnection}).
     */
    void show() {
        if (connected) {
            post(this::showNow);
        } else {
            showPending = true;
        }
        if (!hasFocus()) {
            requestFocus();
        }
    }

    private void showNow() {
        if (!editing || !hasFocus()) {
            return;
        }
        if (Build.VERSION.SDK_INT >= 30) {
            // (Android's own way: it waits for the keyboard to be ready.)
            getWindowInsetsController().show(WindowInsets.Type.ime());
        } else {
            keyboard.showSoftInput(this, 0);
        }
    }

    /** From Rust: no field's being edited. */
    void hide() {
        editing = false;
        showPending = false;
        connected = false;
        text.clearSpans();
        text.clear();
        remember();
        if (Build.VERSION.SDK_INT >= 30) {
            getWindowInsetsController().hide(WindowInsets.Type.ime());
        } else {
            keyboard.hideSoftInputFromWindow(getWindowToken(), 0);
        }
        keyboard.restartInput(this);
    }

    @Override
    public boolean onCheckIsTextEditor() {
        return editing;
    }

    @Override
    public InputConnection onCreateInputConnection(EditorInfo info) {
        if (!editing) {
            return null;
        }
        info.inputType = password
                ? InputType.TYPE_CLASS_TEXT | InputType.TYPE_TEXT_VARIATION_PASSWORD
                : InputType.TYPE_CLASS_TEXT | InputType.TYPE_TEXT_FLAG_AUTO_CORRECT;
        // Enter is a key press for the app: it ends a line's editing, or
        // starts a new line in a box.
        info.imeOptions = EditorInfo.IME_ACTION_UNSPECIFIED
                | EditorInfo.IME_FLAG_NO_FULLSCREEN | EditorInfo.IME_FLAG_NO_EXTRACT_UI;
        info.initialSelStart = Selection.getSelectionStart(text);
        info.initialSelEnd = Selection.getSelectionEnd(text);
        if (Build.VERSION.SDK_INT >= 30) {
            info.setInitialSurroundingText(text);
        }
        connected = true;
        if (showPending) {
            showPending = false;
            // Once the keyboard has the connection.
            post(this::showNow);
        }
        return new Connection();
    }

    /** What's now in the field, as Rust and the keyboard have it. */
    private void remember() {
        sentText = text.toString();
        sentStart = Selection.getSelectionStart(text);
        sentEnd = Selection.getSelectionEnd(text);
        composingStart = BaseInputConnection.getComposingSpanStart(text);
        composingEnd = BaseInputConnection.getComposingSpanEnd(text);
    }

    /** The keyboard edited the field: tell it where things are now, and
     * Rust what the field holds. */
    private void edited() {
        String now = text.toString();
        int start = Selection.getSelectionStart(text);
        int end = Selection.getSelectionEnd(text);
        int composeStart = BaseInputConnection.getComposingSpanStart(text);
        int composeEnd = BaseInputConnection.getComposingSpanEnd(text);
        boolean changed = !now.equals(sentText) || start != sentStart || end != sentEnd;
        if (changed || composeStart != composingStart || composeEnd != composingEnd) {
            keyboard.updateSelection(this, start, end, composeStart, composeEnd);
        }
        remember();
        if (changed) {
            Native.typed(revision, now, codePoints(now, start), codePoints(now, end));
        }
    }

    /** The code points in {@code s} before {@code index} (a UTF-16 offset). */
    private static int codePoints(String s, int index) {
        return s.codePointCount(0, Math.max(0, Math.min(index, s.length())));
    }

    /** The UTF-16 offset of code point {@code index} in {@code s}. */
    private static int offset(String s, int index) {
        return s.offsetByCodePoints(0, Math.max(0, Math.min(index, s.codePointCount(0, s.length()))));
    }

    /**
     * The keyboard's connection: BaseInputConnection edits the text (as a
     * full editor) and turns key presses and Enter into key events, which
     * go to the Rust app like any other; each edit, or batch of them, goes
     * to Rust once done.
     */
    private final class Connection extends BaseInputConnection {
        private int batch;

        Connection() {
            super(KeyboardView.this, true);
        }

        @Override
        public Editable getEditable() {
            return text;
        }

        @Override
        public boolean beginBatchEdit() {
            batch++;
            return true;
        }

        @Override
        public boolean endBatchEdit() {
            if (batch > 0 && --batch == 0) {
                edited();
            }
            return batch > 0;
        }

        private boolean done(boolean result) {
            if (batch == 0) {
                edited();
            }
            return result;
        }

        @Override
        public boolean commitText(CharSequence text, int newCursorPosition) {
            return done(super.commitText(text, newCursorPosition));
        }

        @Override
        public boolean setComposingText(CharSequence text, int newCursorPosition) {
            return done(super.setComposingText(text, newCursorPosition));
        }

        @Override
        public boolean setComposingRegion(int start, int end) {
            return done(super.setComposingRegion(start, end));
        }

        @Override
        public boolean finishComposingText() {
            return done(super.finishComposingText());
        }

        @Override
        public boolean deleteSurroundingText(int beforeLength, int afterLength) {
            return done(super.deleteSurroundingText(beforeLength, afterLength));
        }

        @Override
        public boolean deleteSurroundingTextInCodePoints(int beforeLength, int afterLength) {
            return done(super.deleteSurroundingTextInCodePoints(beforeLength, afterLength));
        }

        @Override
        public boolean setSelection(int start, int end) {
            return done(super.setSelection(start, end));
        }

        @Override
        @TargetApi(34)
        public boolean replaceText(int start, int end, CharSequence text, int newCursorPosition, TextAttribute textAttribute) {
            return done(super.replaceText(start, end, text, newCursorPosition, textAttribute));
        }
    }
}
