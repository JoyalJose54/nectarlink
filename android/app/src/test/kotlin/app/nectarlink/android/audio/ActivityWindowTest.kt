// SPDX-License-Identifier: GPL-3.0-or-later
package app.nectarlink.android.audio

import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class ActivityWindowTest {
    @Test
    fun tracksTrailingActivityWindow() {
        var currentTime = 10_000L
        val window = ActivityWindow(windowMs = 3_000L, now = { currentTime })

        // Initially inactive before any activity
        assertFalse(window.isActive())

        // Active right after markActive()
        window.markActive()
        assertTrue(window.isActive())

        // Still active just inside the window (2,999 ms later)
        currentTime += 2_999L
        assertTrue(window.isActive())

        // Inactive just after the window expires (3,000 ms elapsed)
        currentTime += 1L
        assertFalse(window.isActive())

        // Inactive further after the window
        currentTime += 1_000L
        assertFalse(window.isActive())

        // Active again after another markActive()
        window.markActive()
        assertTrue(window.isActive())

        // Inactive immediately after reset()
        window.reset()
        assertFalse(window.isActive())
    }
}
