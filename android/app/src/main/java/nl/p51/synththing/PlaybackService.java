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
import android.media.AudioAttributes;
import android.media.AudioFocusRequest;
import android.media.AudioManager;
import android.media.MediaMetadata;
import android.media.session.MediaSession;
import android.media.session.PlaybackState;
import android.os.Build;
import android.os.IBinder;
import android.os.SystemClock;

/**
 * Keeps music going with the app in the background (a foreground service,
 * which Android doesn't stop), with the media session that puts what's
 * playing in the notification, on the lock screen and on Bluetooth
 * devices, and takes their buttons. Playback itself is the Rust app's: this
 * shows what it says is playing (update) and hands it the buttons pressed
 * (Native.mediaCommand). It also pauses for calls and other apps' sound
 * (audio focus), and when headphones come out.
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
        long updatedAt;
    }

    private static final Object lock = new Object();
    private static State state = new State();
    private static PlaybackService running;

    private MediaSession session;
    private AudioManager audio;
    private AudioFocusRequest focusRequest;
    private boolean hasFocus;
    /** Paused for a moment (a notification sound, a short call): play again after. */
    private boolean resumeOnFocus;
    private boolean noisyRegistered;
    private boolean foreground;

    /** From the app: what's playing now. Starts the service when something plays. */
    static void update(Context context, String title, String detail, boolean playing, long positionMs, long lengthMs, boolean hasNext) {
        PlaybackService service;
        synchronized (lock) {
            State next = new State();
            next.title = title;
            next.detail = detail;
            next.playing = playing;
            next.positionMs = positionMs;
            next.lengthMs = lengthMs;
            next.hasNext = hasNext;
            next.updatedAt = SystemClock.elapsedRealtime();
            state = next;
            service = running;
        }
        if (service != null) {
            service.apply();
        } else if (playing) {
            context.startForegroundService(new Intent(context, PlaybackService.class));
        }
    }

    static boolean isPlaying() {
        synchronized (lock) {
            return state.playing;
        }
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
        foreground = true;
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
        synchronized (lock) {
            now = state;
        }
        session.setMetadata(new MediaMetadata.Builder()
                .putString(MediaMetadata.METADATA_KEY_TITLE, now.title)
                .putString(MediaMetadata.METADATA_KEY_ARTIST, now.detail)
                .putLong(MediaMetadata.METADATA_KEY_DURATION, now.lengthMs)
                .build());
        long actions = PlaybackState.ACTION_PLAY | PlaybackState.ACTION_PAUSE | PlaybackState.ACTION_PLAY_PAUSE
                | PlaybackState.ACTION_SKIP_TO_PREVIOUS | PlaybackState.ACTION_SEEK_TO;
        if (now.hasNext) {
            actions |= PlaybackState.ACTION_SKIP_TO_NEXT;
        }
        session.setPlaybackState(new PlaybackState.Builder()
                .setActions(actions)
                .setState(now.playing ? PlaybackState.STATE_PLAYING : PlaybackState.STATE_PAUSED,
                        now.positionMs, now.playing ? 1f : 0f, now.updatedAt)
                .build());

        Notification notification = notification(now);
        if (now.playing) {
            if (Build.VERSION.SDK_INT >= 29) {
                startForeground(NOTIFICATION, notification, ServiceInfo.FOREGROUND_SERVICE_TYPE_MEDIA_PLAYBACK);
            } else {
                startForeground(NOTIFICATION, notification);
            }
            foreground = true;
            takeFocus();
        } else {
            if (foreground) {
                // Paused: the notification stays (to play again from), but can be swiped away.
                stopForeground(STOP_FOREGROUND_DETACH);
                foreground = false;
            }
            getSystemService(NotificationManager.class).notify(NOTIFICATION, notification);
            if (!resumeOnFocus) {
                releaseFocus();
            }
        }
    }

    private Notification notification(State now) {
        Notification.Builder builder = new Notification.Builder(this, CHANNEL)
                .setSmallIcon(R.drawable.ic_notification)
                .setContentTitle(now.title)
                .setContentText(now.detail)
                .setContentIntent(openApp())
                .setOngoing(now.playing)
                .setVisibility(Notification.VISIBILITY_PUBLIC)
                .addAction(action(android.R.drawable.ic_media_previous, "Previous", ACTION_PREVIOUS));
        if (now.playing) {
            builder.addAction(action(android.R.drawable.ic_media_pause, "Pause", ACTION_PAUSE));
        } else {
            builder.addAction(action(android.R.drawable.ic_media_play, "Play", ACTION_PLAY));
        }
        builder.addAction(action(android.R.drawable.ic_media_next, "Next", ACTION_NEXT));
        builder.setStyle(new Notification.MediaStyle()
                .setMediaSession(session.getSessionToken())
                .setShowActionsInCompactView(0, 1, 2));
        return builder.build();
    }

    private Notification.Action action(int icon, String title, String action) {
        Intent intent = new Intent(this, PlaybackService.class).setAction(action);
        PendingIntent pending = PendingIntent.getService(this, action.hashCode(), intent, PendingIntent.FLAG_IMMUTABLE);
        return new Notification.Action.Builder(android.graphics.drawable.Icon.createWithResource(this, icon), title, pending).build();
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
