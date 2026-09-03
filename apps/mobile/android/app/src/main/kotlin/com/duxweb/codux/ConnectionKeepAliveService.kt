package com.duxweb.codux

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.os.Build
import android.os.IBinder

/**
 * Holds this process at foreground importance while the app is in the
 * background, so the remote transport keeps sending its heartbeats and the host
 * link survives an app switch. The service itself does no work: the connection
 * lives in the Flutter process it is pinning.
 *
 * Started and stopped explicitly from Dart, so a killed process must not bring
 * it back on its own (`START_NOT_STICKY`).
 */
class ConnectionKeepAliveService : Service() {
    override fun onBind(intent: Intent?): IBinder? = null

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        if (intent?.action == ACTION_STOP) {
            stopForegroundCompat()
            stopSelf()
            return START_NOT_STICKY
        }
        val title = intent?.getStringExtra(EXTRA_TITLE).orEmpty()
        val body = intent?.getStringExtra(EXTRA_BODY).orEmpty()
        startForeground(NOTIFICATION_ID, buildNotification(title, body))
        return START_NOT_STICKY
    }

    private fun buildNotification(title: String, body: String): Notification {
        ensureChannel()
        val contentIntent = PendingIntent.getActivity(
            this,
            0,
            Intent(this, MainActivity::class.java).apply {
                flags = Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_CLEAR_TOP
            },
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )
        val builder = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            Notification.Builder(this, CHANNEL_ID)
        } else {
            @Suppress("DEPRECATION")
            Notification.Builder(this)
        }
        return builder
            .setContentTitle(title)
            .setContentText(body)
            .setSmallIcon(android.R.drawable.stat_sys_download_done)
            .setContentIntent(contentIntent)
            .setOngoing(true)
            .build()
    }

    private fun ensureChannel() {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.O) return
        val manager = getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager
        if (manager.getNotificationChannel(CHANNEL_ID) != null) return
        manager.createNotificationChannel(
            NotificationChannel(
                CHANNEL_ID,
                CHANNEL_NAME,
                // Silent: the notification is the price of staying connected,
                // not something to interrupt anyone with.
                NotificationManager.IMPORTANCE_LOW,
            ),
        )
    }

    private fun stopForegroundCompat() {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.N) {
            stopForeground(STOP_FOREGROUND_REMOVE)
        } else {
            @Suppress("DEPRECATION")
            stopForeground(true)
        }
    }

    companion object {
        const val ACTION_STOP = "com.duxweb.codux.action.STOP_KEEP_ALIVE"
        const val EXTRA_TITLE = "title"
        const val EXTRA_BODY = "body"

        private const val CHANNEL_ID = "codux_connection_keepalive"
        private const val CHANNEL_NAME = "Connection"
        private const val NOTIFICATION_ID = 1001
    }
}
