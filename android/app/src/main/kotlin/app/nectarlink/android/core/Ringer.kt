// SPDX-License-Identifier: GPL-3.0-or-later
package app.nectarlink.android.core

import android.app.NotificationManager
import android.content.Context
import android.media.AudioAttributes
import android.media.AudioManager
import android.media.MediaPlayer
import android.media.RingtoneManager
import android.os.Build
import android.os.Handler
import android.os.Looper
import android.os.VibrationEffect
import android.os.Vibrator
import android.os.VibratorManager
import android.util.Log

/**
 * Rings and vibrates for up to a minute, temporarily raising the alarm volume.
 * Restores the original volume once on stop, timeout or failure, or next launch
 * after a crash. Android's Do Not Disturb policy stays in charge.
 */
class Ringer(context: Context) {
    private val context = context.applicationContext
    private val main = Handler(Looper.getMainLooper())
    private var player: MediaPlayer? = null
    private val timeout = Runnable { stopRingingNow() }
    private val tracker = RingPolicy.OverrideTracker()

    private val audio: AudioManager? get() = context.getSystemService(AudioManager::class.java)
    private val notifications: NotificationManager?
        get() = context.getSystemService(NotificationManager::class.java)

    init {
        restoreStaleOverride()
    }

    fun startRinging() {
        main.post { startRingingNow() }
    }

    fun stopRinging() {
        main.post { stopRingingNow() }
    }

    private fun startRingingNow() {
        main.removeCallbacks(timeout)
        if (player != null) {
            // Overlapping ring request: keep the original saved
            // volume and the player, just extend the bound.
            main.postDelayed(timeout, MAX_RING_MS)
            return
        }
        beginOverride()
        val uri = RingtoneManager.getDefaultUri(RingtoneManager.TYPE_ALARM)
            ?: RingtoneManager.getDefaultUri(RingtoneManager.TYPE_RINGTONE)
        player = runCatching { MediaPlayer() }.getOrNull()?.let { candidate ->
            runCatching {
                candidate.apply {
                    setAudioAttributes(
                        AudioAttributes.Builder()
                            .setUsage(AudioAttributes.USAGE_ALARM)
                            .setContentType(AudioAttributes.CONTENT_TYPE_SONIFICATION)
                            .build(),
                    )
                    setDataSource(context, uri)
                    isLooping = true
                    prepare()
                    start()
                }
            }.onFailure {
                Log.w(TAG, "alarm player failed to start", it)
                runCatching { candidate.release() }
            }.getOrNull()
        }
        runCatching { vibrator()?.vibrate(VibrationEffect.createWaveform(longArrayOf(0, 600, 400), 0)) }
        // Vibration must also stop on the bound when the audio player fails.
        main.postDelayed(timeout, MAX_RING_MS)
        if (player == null) {
            // Nothing is sounding: never leave the volume raised.
            Log.w(TAG, "no alarm player could start; restoring the alarm volume")
            restoreOverride()
        }
    }

    private fun stopRingingNow() {
        main.removeCallbacks(timeout)
        player?.let { current ->
            runCatching { current.stop() }
            runCatching { current.release() }
        }
        player = null
        runCatching { vibrator()?.cancel() }
        restoreOverride()
    }

    /** Saves the original volume before raising it, including any unfinished restore. */
    private fun beginOverride() {
        val filter = runCatching { notifications?.currentInterruptionFilter }.getOrNull()
        if (filter == NotificationManager.INTERRUPTION_FILTER_NONE) {
            Log.w(TAG, "Do Not Disturb total silence suppresses alarm audio; ringing by vibration only")
        }
        val am = audio ?: return
        val current = runCatching { am.getStreamVolume(AudioManager.STREAM_ALARM) }.getOrNull() ?: return
        val original = readPersisted() ?: current
        if (tracker.begin(original) && !persistSaved(original)) {
            tracker.consumeRestore()
            Log.w(TAG, "can't persist the original alarm volume; not raising it")
            return
        }
        val max = runCatching { am.getStreamMaxVolume(AudioManager.STREAM_ALARM) }.getOrDefault(current)
        runCatching {
            am.setStreamVolume(AudioManager.STREAM_ALARM, if (max > 0) max else current.coerceAtLeast(0), 0)
        }.onFailure { Log.w(TAG, "can't raise the alarm volume", it) }
    }

    /** Restores the captured alarm volume, exactly once. */
    private fun restoreOverride() {
        val saved = tracker.consumeRestore() ?: return
        if (setAlarmVolume(saved)) {
            clearPersisted()
        }
        // If setting it failed, the persisted copy stays, so the next
        // launch's stale-override restore retries instead of losing it.
    }

    /** True only when an AudioManager exists and accepted [volume]. */
    private fun setAlarmVolume(volume: Int): Boolean {
        val am = audio ?: return false
        return runCatching { am.setStreamVolume(AudioManager.STREAM_ALARM, volume, 0) }
            .onFailure { Log.w(TAG, "can't restore the alarm volume", it) }
            .isSuccess
    }

    /**
     * If a previous process died while a ring had the volume raised,
     * its persisted saved volume is restored now and forgotten. This
     * never runs while this instance has an active override.
     */
    private fun restoreStaleOverride() {
        if (tracker.active) return
        val prefs = context.getSharedPreferences(PREFS, Context.MODE_PRIVATE)
        if (!prefs.contains(KEY_SAVED_ALARM_VOLUME)) return
        val saved = readPersisted()
        if (saved == null) {
            clearPersisted()
        } else if (setAlarmVolume(saved)) {
            clearPersisted()
        }
    }

    private fun readPersisted(): Int? =
        context.getSharedPreferences(PREFS, Context.MODE_PRIVATE)
            .getInt(KEY_SAVED_ALARM_VOLUME, -1)
            .takeIf { it >= 0 }

    /** Synchronous, so the saved volume is on disk before any raise. */
    private fun persistSaved(volume: Int): Boolean =
        runCatching {
            context.getSharedPreferences(PREFS, Context.MODE_PRIVATE)
                .edit().putInt(KEY_SAVED_ALARM_VOLUME, volume).commit()
        }.getOrDefault(false)

    private fun clearPersisted() {
        context.getSharedPreferences(PREFS, Context.MODE_PRIVATE)
            .edit().remove(KEY_SAVED_ALARM_VOLUME).apply()
    }

    private fun vibrator(): Vibrator? =
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
            context.getSystemService(VibratorManager::class.java)?.defaultVibrator
        } else {
            @Suppress("DEPRECATION")
            context.getSystemService(Vibrator::class.java)
        }

    private companion object {
        const val TAG = "Ringer"
        const val MAX_RING_MS = 60_000L
        const val PREFS = "ringer"
        const val KEY_SAVED_ALARM_VOLUME = "saved_alarm_volume"
    }
}
