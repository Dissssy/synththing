package nl.p51.synththing;

/** What Java tells the Rust app (android/src/lib.rs, then src/android.rs). */
final class Native {
    private Native() {}

    /**
     * An import is done: {@code paths} copied in, {@code skipped} the names of
     * those picked that aren't of a type it takes; or {@code error} why not.
     */
    static native void imported(int kind, String[] paths, String[] skipped, String error);

    /** A media button: one of PlaybackService's COMMAND_*s. */
    static native void mediaCommand(int command, long positionMs);

    /** What the system bars, a notch and the keyboard cover of the window, in pixels. */
    static native void insets(int left, int top, int right, int bottom);

    /**
     * The keyboard edited the text field: it now holds {@code text}, with
     * {@code selectionStart} to {@code selectionEnd} selected (in code
     * points), edited on what Rust said it held as {@code revision}.
     */
    static native void typed(int revision, String text, int selectionStart, int selectionEnd);

    /** The app went into picture-in-picture, or came out of it. */
    static native void pictureInPicture(boolean on);
}
