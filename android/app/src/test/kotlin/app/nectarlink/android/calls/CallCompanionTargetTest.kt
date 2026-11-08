// SPDX-License-Identifier: GPL-3.0-or-later
package app.nectarlink.android.calls

import android.telecom.Call
import org.junit.Assert.assertEquals
import org.junit.Test

class CallCompanionTargetTest {
    @Test
    fun waitingCallDoesNotTakeOverTheActiveOne() {
        assertEquals(0, controlledIndex(listOf(Call.STATE_ACTIVE, Call.STATE_RINGING)))
        assertEquals(0, controlledIndex(listOf(Call.STATE_HOLDING, Call.STATE_RINGING)))
        assertEquals(0, controlledIndex(listOf(Call.STATE_RINGING)))
    }

    @Test
    fun noCallToControlWhenAllAreGone() {
        assertEquals(-1, controlledIndex(emptyList()))
        assertEquals(-1, controlledIndex(listOf(Call.STATE_DISCONNECTED)))
    }
}
