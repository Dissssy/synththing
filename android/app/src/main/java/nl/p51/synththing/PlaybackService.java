package nl.p51.synththing;

import android.app.Notification;
import android.app.NotificationChannel;
import android.app.NotificationManager;
import android.app.PendingIntent;
import android.app.Service;
import android.content.BroadcastReceiver;
import android.content.Context;
import android.content.Intent;
import android.content.IntentFilter;
import android.content.pm.ServiceInfo;
import android.graphics.Bitmap;
import android.graphics.BitmapFactory;
import android.graphics.Canvas;
import android.graphics.Color;
import android.graphics.Paint;
import android.media.AudioAttributes;
import android.media.AudioFocusRequest;
import android.media.AudioManager;
import android.media.MediaMetadata;
import android.media.session.MediaSession;
import android.media.session.PlaybackState;
import android.os.Build;
import android.os.Bundle;
import android.os.IBinder;
import android.os.SystemClock;
import android.util.Log;
import android.view.KeyEvent;

import java.util.ArrayList;
import java.util.Arrays;
import java.util.List;

/**
 * Keeps music going with the app in the background (a foreground service,
 * which Android doesn't stop; it stays one, paused too, until the app's
 * swiped away), with the media session that puts what's playing in the
 * notification, on the lock screen and on Bluetooth devices, and takes
 * their buttons. Playback itself is the Rust app's: this shows what it says
 * is playing (update) and hands it the buttons pressed (Native.mediaCommand).
 * It also pauses for calls and other apps' sound (audio focus), and when
 * headphones come out.
 *
 * Beside play/pause it shows the buttons the user arranged (Preferences >
 * Playback > Media controls), most wanted first: previous, next, and
 * toggles, the song's star and tags, each given to the song or taken off
 * it with a tap (Native.setTag). The notification has room for four
 * (NOTIFICATION_BUTTONS), picture-in-picture for what the phone says.
 * Wherever they are in the list, previous and next sit either side of
 * play/pause and the toggles after (as Android 13 and later lay out the
 * session's controls anyway: its custom actions after next).
 */
public class PlaybackService extends Service {
    // Media commands, as src/android.rs numbers them.
    static final int COMMAND_PLAY = 0;
    static final int COMMAND_PAUSE = 1;
    static final int COMMAND_NEXT = 2;
    static final int COMMAND_PREVIOUS = 3;
    static final int COMMAND_SEEK = 4;

    static final String ACTION_PLAY = "nl.p51.synththing.PLAY";
    static final String ACTION_PAUSE = "nl.p51.synththing.PAUSE";
    static final String ACTION_NEXT = "nl.p51.synththing.NEXT";
    static final String ACTION_PREVIOUS = "nl.p51.synththing.PREVIOUS";
    /** A toggle's action: this, "on/" or "off/" (what to set it to), then its tag. */
    private static final String ACTION_TAG = "nl.p51.synththing.TAG/";

    /** How many buttons the notification has room for beside play/pause (src/config.rs's NOTIFICATION_BUTTONS). */
    static final int NOTIFICATION_BUTTONS = 4;

    // Android 13 and later: keep previous's and next's slots for them
    // (MediaConstants' keys), so a toggle doesn't jump into next's while
    // there's no next song.
    private static final String RESERVE_PREVIOUS = "android.media.playback.ALWAYS_RESERVE_SPACE_FOR.ACTION_SKIP_TO_PREV";
    private static final String RESERVE_NEXT = "android.media.playback.ALWAYS_RESERVE_SPACE_FOR.ACTION_SKIP_TO_NEXT";

    /** One of the buttons beside play/pause: previous, next, or a toggle. */
    static final class Button {
        static final String PREVIOUS = "previous";
        static final String NEXT = "next";
        static final String TAG = "tag";
        /** Play or pause, placed by {@link #layout}. */
        static final String PLAY = "play";

        final String kind;
        /** A toggle's tag (the star too), what it's called, its icon's name (src/song_store.rs's TAG_ICONS) and whether the song has it. */
        final String tag;
        final String label;
        final String icon;
        final boolean on;

        Button(String kind, String tag, String label, String icon, boolean on) {
            this.kind = kind;
            this.tag = tag;
            this.label = label;
            this.icon = icon;
            this.on = on;
        }

        boolean isToggle() {
            return TAG.equals(kind);
        }

        /** What tapping a toggle does: sets the tag the other way. */
        String action() {
            return ACTION_TAG + (on ? "off/" : "on/") + tag;
        }

        /** What a toggle's called, said by a screen reader. */
        String title() {
            return on ? label + " (on)" : label;
        }

        /** A toggle's icon (an ic_<name> drawable, made from Phosphor's): outlined, or filled while on. */
        int iconResource(Context context) {
            String name = "ic_" + icon.replace('-', '_') + (on ? "_on" : "");
            int id = context.getResources().getIdentifier(name, "drawable", context.getPackageName());
            return id != 0 ? id : (on ? R.drawable.ic_tag_on : R.drawable.ic_tag);
        }

        /**
         * The first {@code room} of {@code buttons}, laid out: previous,
         * play/pause, next, then the toggles in order.
         */
        static List<Button> layout(Button[] buttons, int room) {
            List<Button> shown = Arrays.asList(buttons).subList(0, Math.min(room, buttons.length));
            List<Button> out = new ArrayList<>();
            for (Button b : shown) {
                if (PREVIOUS.equals(b.kind)) {
                    out.add(b);
                }
            }
            out.add(new Button(PLAY, "", "", "", false));
            for (Button b : shown) {
                if (NEXT.equals(b.kind)) {
                    out.add(b);
                }
            }
            for (Button b : shown) {
                if (b.isToggle()) {
                    out.add(b);
                }
            }
            return out;
        }
    }

    private static final String CHANNEL = "playback";
    private static final int NOTIFICATION = 1;

    /** What's playing, as the app last said. */
    private static final class State {
        String title = "";
        String detail = "";
        boolean playing;
        long positionMs;
        long lengthMs;
        boolean hasNext;
        Button[] buttons = new Button[0];
        long updatedAt;
    }

    private static final Object lock = new Object();
    private static State state = new State();
    /** The song playing's art, as the app last said (kept across updates). */
    private static Bitmap art;
    /** The size art is drawn or scaled to. */
    private static final int ART_SIZE = 512;
    private static PlaybackService running;

    private MediaSession session;
    private AudioManager audio;
    private AudioFocusRequest focusRequest;
    private boolean hasFocus;
    /** Paused for a moment (a notification sound, a short call): play again after. */
    private boolean resumeOnFocus;
    private boolean noisyRegistered;

    /**
     * From the app: what's playing now, and the buttons beside play/pause
     * (five strings each: kind, then a toggle's tag, label, icon, and "1"
     * if on). Starts the service when something plays.
     */
    static void update(Context context, String title, String detail, boolean playing, long positionMs, long lengthMs, boolean hasNext, String[] buttons) {
        PlaybackService service;
        synchronized (lock) {
            State next = new State();
            next.title = title;
            next.detail = detail;
            next.playing = playing;
            next.positionMs = positionMs;
            next.lengthMs = lengthMs;
            next.hasNext = hasNext;
            next.buttons = new Button[buttons.length / 5];
            for (int i = 0; i < next.buttons.length; i++) {
                int at = 5 * i;
                next.buttons[i] = new Button(buttons[at], buttons[at + 1], buttons[at + 2], buttons[at + 3], "1".equals(buttons[at + 4]));
            }
            next.updatedAt = SystemClock.elapsedRealtime();
            state = next;
            service = running;
        }
        if (service != null) {
            service.apply();
        } else if (playing) {
            try {
                context.startForegroundService(new Intent(context, PlaybackService.class));
            } catch (IllegalStateException e) {
                // Played from the background (the Local API, the hub) after
                // the app was swiped away: Android won't start it from there
                // (ForegroundServiceStartNotAllowedException), so no controls
                // until the app's opened.
                Log.w("synththing", "playback service not started", e);
            }
        }
    }

    /**
     * From the app: the song playing's art, {@code picture} (a JPEG or
     * PNG), or if that's empty, the app's tile for it: {@code color}
     * (0xAARRGGBB), darker in its lower half, with {@code initials} over
     * it (a line break between two rows), as src/album_art.rs draws tiles.
     */
    static void setArt(byte[] picture, int color, String initials) {
        Bitmap bitmap = picture.length > 0 ? scaled(BitmapFactory.decodeByteArray(picture, 0, picture.length)) : null;
        if (bitmap == null) {
            bitmap = tile(color, initials);
        }
        PlaybackService service;
        synchronized (lock) {
            art = bitmap;
            service = running;
        }
        if (service != null) {
            service.apply();
        }
    }

    private static Bitmap scaled(Bitmap bitmap) {
        if (bitmap == null || Math.max(bitmap.getWidth(), bitmap.getHeight()) <= ART_SIZE) {
            return bitmap;
        }
        float scale = (float) ART_SIZE / Math.max(bitmap.getWidth(), bitmap.getHeight());
        return Bitmap.createScaledBitmap(bitmap, Math.round(bitmap.getWidth() * scale), Math.round(bitmap.getHeight() * scale), true);
    }

    private static Bitmap tile(int color, String initials) {
        Bitmap bitmap = Bitmap.createBitmap(ART_SIZE, ART_SIZE, Bitmap.Config.ARGB_8888);
        Canvas canvas = new Canvas(bitmap);
        canvas.drawColor(color);
        Paint shade = new Paint();
        shade.setColor(Color.argb(40, 0, 0, 0));
        canvas.drawRect(0, ART_SIZE / 2f, ART_SIZE, ART_SIZE, shade);
        String[] lines = initials.split("\n");
        int letters = initials.replace("\n", "").codePointCount(0, initials.replace("\n", "").length());
        Paint text = new Paint(Paint.ANTI_ALIAS_FLAG);
        text.setColor(Color.argb(220, 255, 255, 255));
        text.setTextAlign(Paint.Align.CENTER);
        float size = lines.length > 1 ? 0.26f : letters <= 1 ? 0.5f : letters == 2 ? 0.32f : 0.24f;
        text.setTextSize(ART_SIZE * size);
        Paint.FontMetrics metrics = text.getFontMetrics();
        float middle = -(metrics.ascent + metrics.descent) / 2f;
        for (int i = 0; i < lines.length; i++) {
            float y = ART_SIZE / 2f + (lines.length > 1 ? (i == 0 ? -1 : 1) * ART_SIZE * 0.14f : 0);
            canvas.drawText(lines[i], ART_SIZE / 2f, y + middle, text);
        }
        return bitmap;
    }

    static boolean isPlaying() {
        synchronized (lock) {
            return state.playing;
        }
    }

    /** The buttons beside play/pause, for picture-in-picture. */
    static Button[] buttons() {
        synchronized (lock) {
            return state.buttons;
        }
    }

    /** A toggle's action, if {@code action} is one: set its tag. Whether it was. */
    static boolean setTag(String action) {
        if (action == null || !action.startsWith(ACTION_TAG)) {
            return false;
        }
        String rest = action.substring(ACTION_TAG.length());
        int slash = rest.indexOf('/');
        if (slash > 0) {
            Native.setTag(rest.substring(slash + 1), rest.substring(0, slash).equals("on"));
        }
        return true;
    }

    private static void command(int command, long positionMs) {
        Native.mediaCommand(command, positionMs);
    }

    @Override
    public void onCreate() {
        super.onCreate();
        audio = getSystemService(AudioManager.class);
        NotificationManager notifications = getSystemService(NotificationManager.class);
        NotificationChannel channel = new NotificationChannel(CHANNEL, "Playback", NotificationManager.IMPORTANCE_LOW);
        channel.setShowBadge(false);
        notifications.createNotificationChannel(channel);

        session = new MediaSession(this, "synththing");
        session.setCallback(new MediaSession.Callback() {
            @Override public void onPlay() { command(COMMAND_PLAY, 0); }
            @Override public void onPause() { command(COMMAND_PAUSE, 0); }
            @Override public void onSkipToNext() { command(COMMAND_NEXT, 0); }
            @Override public void onSkipToPrevious() { command(COMMAND_PREVIOUS, 0); }
            @Override public void onSeekTo(long positionMs) { command(COMMAND_SEEK, positionMs); }
            @Override public void onStop() { command(COMMAND_PAUSE, 0); }
            @Override public void onCustomAction(String action, Bundle extras) { setTag(action); }
            // Headphones' and Bluetooth's previous and next work whether
            // or not the buttons are shown (the session takes only the
            // actions it lists).
            @Override public boolean onMediaButtonEvent(Intent intent) {
                KeyEvent key = intent.getParcelableExtra(Intent.EXTRA_KEY_EVENT);
                if (key != null && key.getAction() == KeyEvent.ACTION_DOWN) {
                    if (key.getKeyCode() == KeyEvent.KEYCODE_MEDIA_NEXT) {
                        command(COMMAND_NEXT, 0);
                        return true;
                    }
                    if (key.getKeyCode() == KeyEvent.KEYCODE_MEDIA_PREVIOUS) {
                        command(COMMAND_PREVIOUS, 0);
                        return true;
                    }
                }
                return super.onMediaButtonEvent(intent);
            }
        });
        session.setSessionActivity(openApp());
        session.setActive(true);

        focusRequest = new AudioFocusRequest.Builder(AudioManager.AUDIOFOCUS_GAIN)
                .setAudioAttributes(new AudioAttributes.Builder()
                        .setUsage(AudioAttributes.USAGE_MEDIA)
                        .setContentType(AudioAttributes.CONTENT_TYPE_MUSIC)
                        .build())
                .setOnAudioFocusChangeListener(this::onFocusChange)
                .build();

        // Started to play (startForegroundService): Android wants it in the
        // foreground straight away, whatever's happened since.
        State now;
        synchronized (lock) {
            running = this;
            now = state;
        }
        if (Build.VERSION.SDK_INT >= 29) {
            startForeground(NOTIFICATION, notification(now), ServiceInfo.FOREGROUND_SERVICE_TYPE_MEDIA_PLAYBACK);
        } else {
            startForeground(NOTIFICATION, notification(now));
        }
        apply();
    }

    @Override
    public int onStartCommand(Intent intent, int flags, int startId) {
        String action = intent == null ? null : intent.getAction();
        if (ACTION_PLAY.equals(action)) {
            command(COMMAND_PLAY, 0);
        } else if (ACTION_PAUSE.equals(action)) {
            command(COMMAND_PAUSE, 0);
        } else if (ACTION_NEXT.equals(action)) {
            command(COMMAND_NEXT, 0);
        } else if (ACTION_PREVIOUS.equals(action)) {
            command(COMMAND_PREVIOUS, 0);
        } else {
            setTag(action);
        }
        apply();
        return START_NOT_STICKY;
    }

    @Override
    public void onTaskRemoved(Intent rootIntent) {
        // Swiped away from recent apps: stop, as the app's gone.
        command(COMMAND_PAUSE, 0);
        stopForeground(STOP_FOREGROUND_REMOVE);
        stopSelf();
    }

    @Override
    public void onDestroy() {
        synchronized (lock) {
            running = null;
        }
        releaseFocus();
        session.release();
        super.onDestroy();
    }

    @Override
    public IBinder onBind(Intent intent) {
        return null;
    }

    /** Show the current state: the session, the notification, focus. */
    private synchronized void apply() {
        State now;
        Bitmap picture;
        synchronized (lock) {
            now = state;
            picture = art;
        }
        MediaMetadata.Builder metadata = new MediaMetadata.Builder()
                .putString(MediaMetadata.METADATA_KEY_TITLE, now.title)
                .putString(MediaMetadata.METADATA_KEY_ARTIST, now.detail)
                .putLong(MediaMetadata.METADATA_KEY_DURATION, now.lengthMs);
        if (picture != null) {
            metadata.putBitmap(MediaMetadata.METADATA_KEY_ALBUM_ART, picture);
        }
        session.setMetadata(metadata.build());
        List<Button> shown = Button.layout(now.buttons, NOTIFICATION_BUTTONS);
        long actions = PlaybackState.ACTION_PLAY | PlaybackState.ACTION_PAUSE | PlaybackState.ACTION_PLAY_PAUSE
                | PlaybackState.ACTION_SEEK_TO;
        Bundle reserve = new Bundle();
        PlaybackState.Builder playbackState = new PlaybackState.Builder()
                .setState(now.playing ? PlaybackState.STATE_PLAYING : PlaybackState.STATE_PAUSED,
                        now.positionMs, now.playing ? 1f : 0f, now.updatedAt);
        for (Button button : shown) {
            if (Button.PREVIOUS.equals(button.kind)) {
                actions |= PlaybackState.ACTION_SKIP_TO_PREVIOUS;
                reserve.putBoolean(RESERVE_PREVIOUS, true);
            } else if (Button.NEXT.equals(button.kind)) {
                if (now.hasNext) {
                    actions |= PlaybackState.ACTION_SKIP_TO_NEXT;
                }
                reserve.putBoolean(RESERVE_NEXT, true);
            } else if (button.isToggle()) {
                playbackState.addCustomAction(new PlaybackState.CustomAction.Builder(button.action(), button.title(), button.iconResource(this)).build());
            }
        }
        session.setExtras(reserve);
        session.setPlaybackState(playbackState.setActions(actions).build());

        // Stays in the foreground while paused too: once out of it, Android
        // stops the service in the background, and won't let it start again
        // from there (ForegroundServiceStartNotAllowedException), so the
        // controls would be gone until the app's opened.
        getSystemService(NotificationManager.class).notify(NOTIFICATION, notification(now));
        if (now.playing) {
            takeFocus();
        } else if (!resumeOnFocus) {
            releaseFocus();
        }
    }

    private Notification notification(State now) {
        // (Android 12 and earlier show these; later ones the session's.)
        Notification.Builder builder = new Notification.Builder(this, CHANNEL)
                .setSmallIcon(R.drawable.ic_notification)
                .setContentTitle(now.title)
                .setContentText(now.detail)
                .setContentIntent(openApp())
                .setOngoing(true)
                .setVisibility(Notification.VISIBILITY_PUBLIC);
        synchronized (lock) {
            if (art != null) {
                builder.setLargeIcon(art);
            }
        }
        List<Button> shown = Button.layout(now.buttons, NOTIFICATION_BUTTONS);
        for (Button button : shown) {
            builder.addAction(notificationAction(this, button, now.playing));
        }
        int[] compact = new int[Math.min(3, shown.size())];
        for (int i = 0; i < compact.length; i++) {
            compact[i] = i;
        }
        builder.setStyle(new Notification.MediaStyle()
                .setMediaSession(session.getSessionToken())
                .setShowActionsInCompactView(compact));
        return builder.build();
    }

    private static Notification.Action notificationAction(Context context, Button button, boolean playing) {
        return new Notification.Action.Builder(icon(context, button, playing), title(button, playing), pending(context, button, playing)).build();
    }

    // ---- A button's look and what it does, for the notification and picture-in-picture ----

    static android.graphics.drawable.Icon icon(Context context, Button button, boolean playing) {
        int id;
        switch (button.kind) {
            case Button.PREVIOUS: id = android.R.drawable.ic_media_previous; break;
            case Button.NEXT: id = android.R.drawable.ic_media_next; break;
            case Button.PLAY: id = playing ? android.R.drawable.ic_media_pause : android.R.drawable.ic_media_play; break;
            default: id = button.iconResource(context); break;
        }
        return android.graphics.drawable.Icon.createWithResource(context, id);
    }

    static String title(Button button, boolean playing) {
        switch (button.kind) {
            case Button.PREVIOUS: return "Previous";
            case Button.NEXT: return "Next";
            case Button.PLAY: return playing ? "Pause" : "Play";
            default: return button.title();
        }
    }

    static PendingIntent pending(Context context, Button button, boolean playing) {
        String action;
        switch (button.kind) {
            case Button.PREVIOUS: action = ACTION_PREVIOUS; break;
            case Button.NEXT: action = ACTION_NEXT; break;
            case Button.PLAY: action = playing ? ACTION_PAUSE : ACTION_PLAY; break;
            default: action = button.action(); break;
        }
        Intent intent = new Intent(context, PlaybackService.class).setAction(action);
        return PendingIntent.getService(context, action.hashCode(), intent, PendingIntent.FLAG_IMMUTABLE);
    }

    private PendingIntent openApp() {
        Intent intent = new Intent(this, MainActivity.class).addFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP);
        return PendingIntent.getActivity(this, 0, intent, PendingIntent.FLAG_IMMUTABLE);
    }

    // ---- Audio focus, and headphones coming out ----

    private void takeFocus() {
        resumeOnFocus = false;
        if (!hasFocus) {
            hasFocus = audio.requestAudioFocus(focusRequest) == AudioManager.AUDIOFOCUS_REQUEST_GRANTED;
        }
        if (!noisyRegistered) {
            IntentFilter filter = new IntentFilter(AudioManager.ACTION_AUDIO_BECOMING_NOISY);
            if (Build.VERSION.SDK_INT >= 33) {
                registerReceiver(noisy, filter, Context.RECEIVER_NOT_EXPORTED);
            } else {
                registerReceiver(noisy, filter);
            }
            noisyRegistered = true;
        }
    }

    private void releaseFocus() {
        if (hasFocus) {
            audio.abandonAudioFocusRequest(focusRequest);
            hasFocus = false;
        }
        if (noisyRegistered) {
            unregisterReceiver(noisy);
            noisyRegistered = false;
        }
    }

    private void onFocusChange(int change) {
        switch (change) {
            case AudioManager.AUDIOFOCUS_LOSS:
                hasFocus = false;
                resumeOnFocus = false;
                command(COMMAND_PAUSE, 0);
                break;
            case AudioManager.AUDIOFOCUS_LOSS_TRANSIENT:
            case AudioManager.AUDIOFOCUS_LOSS_TRANSIENT_CAN_DUCK:
                if (isPlaying()) {
                    resumeOnFocus = true;
                    command(COMMAND_PAUSE, 0);
                }
                break;
            case AudioManager.AUDIOFOCUS_GAIN:
                hasFocus = true;
                if (resumeOnFocus) {
                    resumeOnFocus = false;
                    command(COMMAND_PLAY, 0);
                }
                break;
            default:
                break;
        }
    }

    private final BroadcastReceiver noisy = new BroadcastReceiver() {
        @Override
        public void onReceive(Context context, Intent intent) {
            command(COMMAND_PAUSE, 0);
        }
    };
}
