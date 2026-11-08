// SPDX-License-Identifier: GPL-3.0-or-later
package app.nectarlink.android.audio

import android.os.SystemClock

/**
 * Tracks whether activity occurred within a trailing time window.
 */
class ActivityWindow(
    private val windowMs: Long,
    private val now: () -> Long = { SystemClock.elapsedRealtime() },
) {
    @Volatile
    private var lastActiveTime: Long = 0L

    fun markActive() {
        lastActiveTime = now()
    }

    fun reset() {
        lastActiveTime = 0L
    }

    fun isActive(): Boolean {
        val last = lastActiveTime
        if (last == 0L) return false
        val elapsed = now() - last
        return elapsed in 0 until windowMs
    }
}
