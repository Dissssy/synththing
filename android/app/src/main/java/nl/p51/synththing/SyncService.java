package nl.p51.synththing;

import android.app.Notification;
import android.app.NotificationChannel;
import android.app.NotificationManager;
import android.app.PendingIntent;
import android.app.Service;
import android.content.Context;
import android.content.Intent;
import android.content.pm.ServiceInfo;
import android.os.Build;
import android.os.IBinder;
import android.util.Log;

/**
 * Keeps songs moving between the user's devices with the app in the
 * background (a foreground service, so Android neither stops nor freezes
 * the app meanwhile), with a notification showing how far it's got. The
 * moving itself is the Rust app's: this shows what it says is done
 * (update), and goes once it says it's finished.
 */
public class SyncService extends Service {
    private static final String CHANNEL = "sync";
    private static final int NOTIFICATION = 2;

    private static final Object lock = new Object();
    /** Songs moved, of how many, as the app last said; none to move: 0 of 0. */
    private static int done;
    private static int total;
    private static SyncService running;

    /** From the app: songs moved so far, of {@code total} (0 once finished). */
    static void update(Context context, int doneNow, int totalNow) {
        SyncService service;
        synchronized (lock) {
            done = doneNow;
            total = totalNow;
            service = running;
        }
        if (service != null) {
            service.apply();
        } else if (totalNow > 0) {
            try {
                context.startForegroundService(new Intent(context, SyncService.class));
            } catch (IllegalStateException e) {
                // Started in the background (Android 12 and up won't): the
                // songs still move while the app's left running.
                Log.i("synththing", "couldn't start the sync service: " + e);
            }
        }
    }

    @Override
    public void onCreate() {
        super.onCreate();
        NotificationManager notifications = getSystemService(NotificationManager.class);
        NotificationChannel channel = new NotificationChannel(CHANNEL, "Song sync", NotificationManager.IMPORTANCE_LOW);
        channel.setShowBadge(false);
        notifications.createNotificationChannel(channel);
        synchronized (lock) {
            running = this;
        }
        // Android wants it in the foreground straight away, even if the
        // songs are done by now (apply then stops it).
        if (Build.VERSION.SDK_INT >= 29) {
            startForeground(NOTIFICATION, notification(), ServiceInfo.FOREGROUND_SERVICE_TYPE_DATA_SYNC);
        } else {
            startForeground(NOTIFICATION, notification());
        }
        apply();
    }

    @Override
    public int onStartCommand(Intent intent, int flags, int startId) {
        apply();
        return START_NOT_STICKY;
    }

    @Override
    public void onDestroy() {
        synchronized (lock) {
            if (running == this) {
                running = null;
            }
        }
        super.onDestroy();
    }

    @Override
    public IBinder onBind(Intent intent) {
        return null;
    }

    /** Show how far it's got, or go once it's finished. */
    private synchronized void apply() {
        int totalNow;
        synchronized (lock) {
            totalNow = total;
            if (totalNow == 0) {
                running = null;
            }
        }
        if (totalNow == 0) {
            stopForeground(STOP_FOREGROUND_REMOVE);
            stopSelf();
            return;
        }
        getSystemService(NotificationManager.class).notify(NOTIFICATION, notification());
    }

    private Notification notification() {
        int doneNow;
        int totalNow;
        synchronized (lock) {
            doneNow = done;
            totalNow = total;
        }
        Intent open = new Intent(this, MainActivity.class).addFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP);
        return new Notification.Builder(this, CHANNEL)
                .setSmallIcon(R.drawable.ic_notification)
                .setContentTitle("Syncing songs with your other devices")
                .setContentText(doneNow + " of " + totalNow)
                .setProgress(totalNow, doneNow, false)
                .setContentIntent(PendingIntent.getActivity(this, 0, open, PendingIntent.FLAG_IMMUTABLE))
                .setOngoing(true)
                .setOnlyAlertOnce(true)
                .setCategory(Notification.CATEGORY_PROGRESS)
                .build();
    }
}
