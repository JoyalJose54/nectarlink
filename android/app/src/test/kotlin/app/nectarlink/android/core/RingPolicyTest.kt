// SPDX-License-Identifier: GPL-3.0-or-later
package app.nectarlink.android.core

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/** Pure ring-policy tests: synthetic values only, no device audio. */
class RingPolicyTest {

    @Test
    fun overrideCapturesOnceAndRestoresExactlyOnce() {
        val tracker = RingPolicy.OverrideTracker()
        assertTrue(tracker.begin(current = 3))
        // An overlapping ring must not record the raised volume as
        // the user's original.
        assertFalse(tracker.begin(current = 7))
        assertTrue(tracker.active)
        assertEquals(3, tracker.consumeRestore())
        assertFalse(tracker.active)
    }

    @Test
    fun restoreIsIdempotent() {
        val tracker = RingPolicy.OverrideTracker()
        // Stop/timeout without a ring restores nothing.
        assertNull(tracker.consumeRestore())
        assertTrue(tracker.begin(current = 0))
        assertEquals(0, tracker.consumeRestore())
        // A second restore attempt (e.g. stop after timeout) is a no-op.
        assertNull(tracker.consumeRestore())
    }
}
