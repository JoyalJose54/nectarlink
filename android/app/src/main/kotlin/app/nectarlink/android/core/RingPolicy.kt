// SPDX-License-Identifier: GPL-3.0-or-later
package app.nectarlink.android.core

/** Captures the original alarm volume once, even across overlapping rings. */
object RingPolicy {
    class OverrideTracker {
        private var saved: Int? = null

        val active: Boolean get() = saved != null

        /** True when this call captured [current] (no override was active). */
        fun begin(current: Int): Boolean {
            if (saved != null) return false
            saved = current
            return true
        }

        /** The captured volume on the first restore attempt, then nothing. */
        fun consumeRestore(): Int? {
            val value = saved
            saved = null
            return value
        }
    }
}
