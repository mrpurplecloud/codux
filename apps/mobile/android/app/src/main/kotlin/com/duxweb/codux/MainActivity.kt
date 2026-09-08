package com.duxweb.codux

import android.app.ForegroundServiceStartNotAllowedException
import android.content.Intent
import android.os.Build
import android.os.Bundle
import android.util.Log
import android.view.ViewGroup
import androidx.core.content.ContextCompat
import io.flutter.embedding.android.FlutterActivity
import io.flutter.embedding.engine.FlutterEngine
import io.flutter.plugin.common.MethodChannel

class MainActivity : FlutterActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        disableDefaultFocusHighlight(window.decorView)
    }

    override fun configureFlutterEngine(flutterEngine: FlutterEngine) {
        super.configureFlutterEngine(flutterEngine)
        MethodChannel(
            flutterEngine.dartExecutor.binaryMessenger,
            KEEP_ALIVE_CHANNEL,
        ).setMethodCallHandler { call, result ->
            when (call.method) {
                "start" -> result.success(
                    startKeepAlive(
                        call.argument<String>("title").orEmpty(),
                        call.argument<String>("body").orEmpty(),
                    ),
                )
                "stop" -> {
                    stopKeepAlive()
                    result.success(null)
                }
                "requestBackgroundExecutionSettings" -> {
                    requestBackgroundExecutionSettings()
                    result.success(null)
                }
                else -> result.notImplemented()
            }
        }
    }

    /**
     * Returns whether the service was accepted. Android 12+ refuses a
     * foreground service started from the background; the caller then keeps the
     * pre-change behaviour (the link drops and the app reconnects on resume)
     * instead of believing the connection is held.
     */
    private fun startKeepAlive(title: String, body: String): Boolean {
        val intent = Intent(this, ConnectionKeepAliveService::class.java).apply {
            putExtra(ConnectionKeepAliveService.EXTRA_TITLE, title)
            putExtra(ConnectionKeepAliveService.EXTRA_BODY, body)
        }
        return try {
            ContextCompat.startForegroundService(this, intent)
            true
        } catch (error: Exception) {
            val refused = Build.VERSION.SDK_INT >= Build.VERSION_CODES.S &&
                error is ForegroundServiceStartNotAllowedException
            Log.w(
                TAG,
                "keep-alive service refused=$refused error=${error.javaClass.simpleName}: ${error.message}",
            )
            false
        }
    }

    private fun requestBackgroundExecutionSettings() {
        if (!Build.MANUFACTURER.equals(HONOR_MANUFACTURER, ignoreCase = true)) return
        val preferences = getPreferences(MODE_PRIVATE)
        // ponytail: Honor exposes no public status API; replace this flag if one appears.
        if (preferences.getBoolean(HONOR_STARTUP_SETTINGS_OPENED, false)) return
        try {
            startActivity(
                Intent().setClassName(HONOR_SYSTEM_MANAGER, HONOR_STARTUP_MANAGER),
            )
            preferences.edit().putBoolean(HONOR_STARTUP_SETTINGS_OPENED, true).apply()
        } catch (error: Exception) {
            Log.w(TAG, "Honor startup settings unavailable: ${error.message}")
        }
    }

    private fun stopKeepAlive() {
        try {
            // Stop through the service's own action: a plain `stopService` can
            // race a start that has not reached `startForeground` yet.
            startService(
                Intent(this, ConnectionKeepAliveService::class.java).apply {
                    action = ConnectionKeepAliveService.ACTION_STOP
                },
            )
        } catch (error: Exception) {
            Log.w(TAG, "keep-alive stop failed: ${error.message}")
            stopService(Intent(this, ConnectionKeepAliveService::class.java))
        }
    }

    private fun disableDefaultFocusHighlight(view: android.view.View) {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            view.defaultFocusHighlightEnabled = false
        }
        if (view is ViewGroup) {
            for (index in 0 until view.childCount) {
                disableDefaultFocusHighlight(view.getChildAt(index))
            }
        }
    }

    private companion object {
        const val KEEP_ALIVE_CHANNEL = "codux/background_keepalive"
        const val TAG = "CoduxKeepAlive"
        const val HONOR_MANUFACTURER = "HONOR"
        const val HONOR_SYSTEM_MANAGER = "com.hihonor.systemmanager"
        const val HONOR_STARTUP_MANAGER =
            "com.hihonor.systemmanager.startupmgr.ui.StartupNormalAppListActivity"
        const val HONOR_STARTUP_SETTINGS_OPENED = "honorStartupSettingsOpened"
    }
}
