package nl.p51.synththing;

import android.Manifest;
import android.app.NativeActivity;
import android.app.PendingIntent;
import android.app.PictureInPictureParams;
import android.app.RemoteAction;
import android.content.ClipData;
import android.content.ClipboardManager;
import android.content.Intent;
import android.content.pm.PackageInstaller;
import android.content.pm.PackageManager;
import android.content.res.Configuration;
import android.database.Cursor;
import android.graphics.Color;
import android.graphics.Insets;
import android.graphics.drawable.Icon;
import android.net.Uri;
import android.os.Build;
import android.os.Bundle;
import android.provider.OpenableColumns;
import android.provider.Settings;
import android.util.Log;
import android.util.Rational;
import android.view.DisplayCutout;
import android.view.View;
import android.view.Window;
import android.view.WindowInsets;
import android.view.WindowInsetsController;
import android.view.WindowManager;

import java.io.File;
import java.io.FileInputStream;
import java.io.FileOutputStream;
import java.io.InputStream;
import java.io.OutputStream;
import java.util.ArrayList;
import java.util.Arrays;
import java.util.List;
import java.util.Locale;

/**
 * The app's activity: a NativeActivity running the Rust app (eframe), with
 * what that can't do itself, for the Rust side to call (src/android.rs):
 * picking files to import, the playback service, picture-in-picture and
 * installing updates.
 */
public class MainActivity extends NativeActivity {
    private static final String TAG = "synththing";
    private static final String LIBRARY = "synththing_android";
    private static final int PICK_FILES = 1;

    // The import waiting on the picker.
    private int pickKind;
    private String[] pickExtensions;
    private String pickDir;

    private volatile boolean pictureInPictureAllowed;

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        // NativeActivity loads the library for its own use only; loading it
        // here as well is what lets Native's methods find their code.
        System.loadLibrary(LIBRARY);
        super.onCreate(savedInstanceState);
        drawEdgeToEdge();
        if (Build.VERSION.SDK_INT >= 33
                && checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) != PackageManager.PERMISSION_GRANTED) {
            requestPermissions(new String[] {Manifest.permission.POST_NOTIFICATIONS}, 0);
        }
    }

    // ---- The system bars ----

    /**
     * Draw under the status and navigation bars (and a notch), telling the
     * Rust side what they cover (Native.insets) to keep its UI clear of;
     * the same on every Android version, and what lets it hide them.
     */
    @SuppressWarnings("deprecation")
    private void drawEdgeToEdge() {
        Window window = getWindow();
        View decor = window.getDecorView();
        if (Build.VERSION.SDK_INT >= 30) {
            window.setDecorFitsSystemWindows(false);
        } else {
            decor.setSystemUiVisibility(View.SYSTEM_UI_FLAG_LAYOUT_STABLE
                    | View.SYSTEM_UI_FLAG_LAYOUT_FULLSCREEN
                    | View.SYSTEM_UI_FLAG_LAYOUT_HIDE_NAVIGATION);
        }
        window.setStatusBarColor(Color.TRANSPARENT);
        window.setNavigationBarColor(Color.TRANSPARENT);
        if (Build.VERSION.SDK_INT >= 29) {
            window.setNavigationBarContrastEnforced(false);
        }
        if (Build.VERSION.SDK_INT >= 28) {
            WindowManager.LayoutParams attributes = window.getAttributes();
            attributes.layoutInDisplayCutoutMode = WindowManager.LayoutParams.LAYOUT_IN_DISPLAY_CUTOUT_MODE_SHORT_EDGES;
            window.setAttributes(attributes);
        }
        decor.setOnApplyWindowInsetsListener((view, insets) -> {
            int left, top, right, bottom;
            if (Build.VERSION.SDK_INT >= 30) {
                // Tappable elements: the status bar, and the navigation
                // bar's buttons; with gesture navigation, not the handle
                // (the app goes under it, as Android intends).
                Insets covered = insets.getInsets(WindowInsets.Type.tappableElement()
                        | WindowInsets.Type.displayCutout() | WindowInsets.Type.ime());
                left = covered.left;
                top = covered.top;
                right = covered.right;
                bottom = covered.bottom;
            } else {
                left = insets.getSystemWindowInsetLeft();
                top = insets.getSystemWindowInsetTop();
                right = insets.getSystemWindowInsetRight();
                bottom = insets.getSystemWindowInsetBottom();
                if (Build.VERSION.SDK_INT >= 28 && insets.getDisplayCutout() != null) {
                    DisplayCutout cutout = insets.getDisplayCutout();
                    left = Math.max(left, cutout.getSafeInsetLeft());
                    top = Math.max(top, cutout.getSafeInsetTop());
                    right = Math.max(right, cutout.getSafeInsetRight());
                    bottom = Math.max(bottom, cutout.getSafeInsetBottom());
                }
            }
            Native.insets(left, top, right, bottom);
            return view.onApplyWindowInsets(insets);
        });
    }

    /**
     * From Rust: hide the status and navigation bars (the visualizer filling
     * the screen), or show them again. Hidden, a swipe from the edge shows
     * them for a moment.
     */
    @SuppressWarnings("deprecation")
    public void setFullscreen(boolean on) {
        runOnUiThread(() -> {
            if (Build.VERSION.SDK_INT >= 30) {
                WindowInsetsController controller = getWindow().getInsetsController();
                if (controller == null) {
                    return;
                }
                controller.setSystemBarsBehavior(WindowInsetsController.BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE);
                if (on) {
                    controller.hide(WindowInsets.Type.systemBars());
                } else {
                    controller.show(WindowInsets.Type.systemBars());
                }
            } else {
                int layout = View.SYSTEM_UI_FLAG_LAYOUT_STABLE
                        | View.SYSTEM_UI_FLAG_LAYOUT_FULLSCREEN
                        | View.SYSTEM_UI_FLAG_LAYOUT_HIDE_NAVIGATION;
                int hidden = View.SYSTEM_UI_FLAG_FULLSCREEN
                        | View.SYSTEM_UI_FLAG_HIDE_NAVIGATION
                        | View.SYSTEM_UI_FLAG_IMMERSIVE_STICKY;
                getWindow().getDecorView().setSystemUiVisibility(on ? layout | hidden : layout);
            }
        });
    }

    // ---- Importing ----

    /** From Rust: let the user pick files to copy into {@code dir}. */
    public void pickFiles(int kind, String[] extensions, String dir) {
        runOnUiThread(() -> {
            pickKind = kind;
            pickExtensions = extensions;
            pickDir = dir;
            Intent intent = new Intent(Intent.ACTION_OPEN_DOCUMENT);
            intent.addCategory(Intent.CATEGORY_OPENABLE);
            intent.setType("*/*");
            intent.putExtra(Intent.EXTRA_ALLOW_MULTIPLE, true);
            try {
                startActivityForResult(intent, PICK_FILES);
            } catch (Exception e) {
                Native.imported(kind, new String[0], "couldn't open the file picker: " + e.getMessage());
            }
        });
    }

    @Override
    protected void onActivityResult(int requestCode, int resultCode, Intent data) {
        if (requestCode != PICK_FILES) {
            super.onActivityResult(requestCode, resultCode, data);
            return;
        }
        List<Uri> uris = new ArrayList<>();
        if (resultCode == RESULT_OK && data != null) {
            ClipData clip = data.getClipData();
            if (clip != null) {
                for (int i = 0; i < clip.getItemCount(); i++) {
                    uris.add(clip.getItemAt(i).getUri());
                }
            } else if (data.getData() != null) {
                uris.add(data.getData());
            }
        }
        int kind = pickKind;
        List<String> extensions = Arrays.asList(pickExtensions);
        File dir = new File(pickDir);
        // Copying can take a while (a big soundfont): not on the UI thread.
        new Thread(() -> {
            List<String> copied = new ArrayList<>();
            List<String> skipped = new ArrayList<>();
            String error = null;
            for (Uri uri : uris) {
                String name = displayName(uri);
                int dot = name.lastIndexOf('.');
                String ext = dot < 0 ? "" : name.substring(dot + 1).toLowerCase(Locale.ROOT);
                if (!extensions.contains(ext)) {
                    skipped.add(name);
                    continue;
                }
                try {
                    copied.add(copyInto(uri, name, dir).getAbsolutePath());
                } catch (Exception e) {
                    error = "couldn't copy " + name + ": " + e.getMessage();
                    break;
                }
            }
            if (error == null && copied.isEmpty() && !skipped.isEmpty()) {
                error = "synththing can't play " + String.join(", ", skipped);
            }
            Native.imported(kind, copied.toArray(new String[0]), error);
        }).start();
    }

    private String displayName(Uri uri) {
        try (Cursor cursor = getContentResolver().query(uri, new String[] {OpenableColumns.DISPLAY_NAME}, null, null, null)) {
            if (cursor != null && cursor.moveToFirst() && !cursor.isNull(0)) {
                return cursor.getString(0);
            }
        } catch (Exception e) {
            Log.w(TAG, "no name for " + uri, e);
        }
        String last = uri.getLastPathSegment();
        return last != null ? last : "imported";
    }

    /**
     * Copy {@code uri} into {@code dir} as {@code name}; a different file
     * already there by that name gets the copy a number ("song (2).mid"),
     * and the same file imported again isn't copied twice (as import.rs).
     */
    private File copyInto(Uri uri, String name, File dir) throws Exception {
        File temp = File.createTempFile("import", null, getCacheDir());
        try {
            try (InputStream in = getContentResolver().openInputStream(uri);
                    OutputStream out = new FileOutputStream(temp)) {
                if (in == null) {
                    throw new Exception("it couldn't be opened");
                }
                byte[] buffer = new byte[64 * 1024];
                int n;
                while ((n = in.read(buffer)) > 0) {
                    out.write(buffer, 0, n);
                }
            }
            int dot = name.lastIndexOf('.');
            String stem = dot < 0 ? name : name.substring(0, dot);
            String ext = dot < 0 ? "" : name.substring(dot);
            for (int n = 1; ; n++) {
                File target = new File(dir, n == 1 ? name : stem + " (" + n + ")" + ext);
                if (!target.exists()) {
                    if (!temp.renameTo(target)) {
                        throw new Exception("it couldn't be moved into place");
                    }
                    return target;
                }
                if (sameContents(target, temp)) {
                    return target;
                }
            }
        } finally {
            temp.delete();
        }
    }

    private static boolean sameContents(File a, File b) throws Exception {
        if (a.length() != b.length()) {
            return false;
        }
        try (InputStream x = new FileInputStream(a); InputStream y = new FileInputStream(b)) {
            byte[] bx = new byte[64 * 1024];
            byte[] by = new byte[64 * 1024];
            int n;
            while ((n = x.read(bx)) > 0) {
                int read = 0;
                while (read < n) {
                    int m = y.read(by, read, n - read);
                    if (m <= 0) {
                        return false;
                    }
                    read += m;
                }
                if (!Arrays.equals(Arrays.copyOf(bx, n), Arrays.copyOf(by, n))) {
                    return false;
                }
            }
        }
        return true;
    }

    // ---- Playback ----

    /** From Rust: what's playing, for the playback service. */
    public void setPlayback(String title, String detail, boolean playing, long positionMs, long lengthMs, boolean hasNext) {
        PlaybackService.update(this, title, detail, playing, positionMs, lengthMs, hasNext);
        // (Its play/pause button follows.)
        runOnUiThread(this::updatePictureInPicture);
    }

    // ---- Picture-in-picture ----

    /**
     * From Rust: whether leaving the app should shrink it into a floating
     * window (while something's playing).
     */
    public void setPictureInPictureAllowed(boolean allowed) {
        pictureInPictureAllowed = allowed;
        if (Build.VERSION.SDK_INT >= 31) {
            runOnUiThread(this::updatePictureInPicture);
        }
    }

    private void updatePictureInPicture() {
        try {
            setPictureInPictureParams(pictureInPictureParams());
        } catch (Exception e) {
            Log.w(TAG, "picture-in-picture", e);
        }
    }

    private PictureInPictureParams pictureInPictureParams() {
        PictureInPictureParams.Builder builder = new PictureInPictureParams.Builder()
                .setAspectRatio(new Rational(1, 1))
                .setActions(pictureInPictureActions());
        if (Build.VERSION.SDK_INT >= 31) {
            builder.setAutoEnterEnabled(pictureInPictureAllowed);
        }
        return builder.build();
    }

    private List<RemoteAction> pictureInPictureActions() {
        List<RemoteAction> actions = new ArrayList<>();
        actions.add(action(android.R.drawable.ic_media_previous, "Previous", PlaybackService.ACTION_PREVIOUS));
        if (PlaybackService.isPlaying()) {
            actions.add(action(android.R.drawable.ic_media_pause, "Pause", PlaybackService.ACTION_PAUSE));
        } else {
            actions.add(action(android.R.drawable.ic_media_play, "Play", PlaybackService.ACTION_PLAY));
        }
        actions.add(action(android.R.drawable.ic_media_next, "Next", PlaybackService.ACTION_NEXT));
        return actions;
    }

    private RemoteAction action(int icon, String title, String serviceAction) {
        Intent intent = new Intent(this, PlaybackService.class).setAction(serviceAction);
        PendingIntent pending = PendingIntent.getService(this, serviceAction.hashCode(), intent, PendingIntent.FLAG_IMMUTABLE);
        return new RemoteAction(Icon.createWithResource(this, icon), title, title, pending);
    }

    @Override
    protected void onUserLeaveHint() {
        super.onUserLeaveHint();
        // (Android 12 and later go into it by themselves: setAutoEnterEnabled.)
        if (Build.VERSION.SDK_INT < 31 && pictureInPictureAllowed) {
            try {
                enterPictureInPictureMode(pictureInPictureParams());
            } catch (Exception e) {
                Log.w(TAG, "picture-in-picture", e);
            }
        }
    }

    @Override
    public void onPictureInPictureModeChanged(boolean inPictureInPictureMode, Configuration newConfig) {
        super.onPictureInPictureModeChanged(inPictureInPictureMode, newConfig);
        Native.pictureInPicture(inPictureInPictureMode);
    }

    // ---- Clipboard and sharing ----

    /** From Rust: put {@code text} on the clipboard (eframe has none of its own here). */
    public void copyText(String text) {
        runOnUiThread(() -> getSystemService(ClipboardManager.class).setPrimaryClip(ClipData.newPlainText("synththing", text)));
    }

    /** From Rust: share {@code text} with an app the user picks. */
    public void shareText(String subject, String text) {
        Intent send = new Intent(Intent.ACTION_SEND)
                .setType("text/plain")
                .putExtra(Intent.EXTRA_SUBJECT, subject)
                .putExtra(Intent.EXTRA_TEXT, text);
        runOnUiThread(() -> startActivity(Intent.createChooser(send, subject)));
    }

    // ---- Updates ----

    /** From Rust: whether Android lets this app install APKs. */
    public boolean canInstallPackages() {
        return getPackageManager().canRequestPackageInstalls();
    }

    /** From Rust: the setting that lets this app install APKs. */
    public void openInstallSettings() {
        runOnUiThread(() -> startActivity(new Intent(Settings.ACTION_MANAGE_UNKNOWN_APP_SOURCES, Uri.parse("package:" + getPackageName()))));
    }

    /** From Rust: install the downloaded APK at {@code path} (Android asks the user first). */
    public void installApk(String path) {
        new Thread(() -> {
            try {
                PackageInstaller installer = getPackageManager().getPackageInstaller();
                PackageInstaller.SessionParams params = new PackageInstaller.SessionParams(PackageInstaller.SessionParams.MODE_FULL_INSTALL);
                int id = installer.createSession(params);
                try (PackageInstaller.Session session = installer.openSession(id)) {
                    File apk = new File(path);
                    try (InputStream in = new FileInputStream(apk); OutputStream out = session.openWrite("synththing.apk", 0, apk.length())) {
                        byte[] buffer = new byte[64 * 1024];
                        int n;
                        while ((n = in.read(buffer)) > 0) {
                            out.write(buffer, 0, n);
                        }
                        session.fsync(out);
                    }
                    Intent intent = new Intent(this, InstallReceiver.class);
                    int flags = PendingIntent.FLAG_UPDATE_CURRENT | (Build.VERSION.SDK_INT >= 31 ? PendingIntent.FLAG_MUTABLE : 0);
                    PendingIntent status = PendingIntent.getBroadcast(this, id, intent, flags);
                    session.commit(status.getIntentSender());
                }
            } catch (Exception e) {
                Log.e(TAG, "installing the update", e);
            }
        }).start();
    }
}
