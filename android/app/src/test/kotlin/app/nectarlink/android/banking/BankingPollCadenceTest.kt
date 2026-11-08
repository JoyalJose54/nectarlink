// SPDX-License-Identifier: GPL-3.0-or-later
package app.nectarlink.android.banking

import kotlinx.coroutines.CoroutineStart
import kotlinx.coroutines.async
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.runBlocking
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class BankingPollCadenceTest {
    private val pollMs = 800L
    private val idleMs = 15_000L

    @Test
    fun screenOffIdlesWithAReceiverAndScreenReturnEndsTheWait() = runBlocking {
        assertEquals(idleMs, bankingPollWaitMs(false, true, pollMs, idleMs))
        assertEquals(pollMs, bankingPollWaitMs(true, true, pollMs, idleMs))
        val wake = Channel<Unit>(Channel.CONFLATED)
        val waiting = async(start = CoroutineStart.UNDISPATCHED) { awaitScreenWake(idleMs, wake) }
        assertFalse(waiting.isCompleted)
        wake.trySend(Unit)
        assertTrue(waiting.await())
        assertTrue(wake.tryReceive().isFailure)
    }

    @Test
    fun receiverFailureKeepsTheShortPollAndTimeoutDoesNotReportAWake() = runBlocking {
        assertEquals(pollMs, bankingPollWaitMs(false, false, pollMs, idleMs))
        assertEquals(pollMs, bankingPollWaitMs(true, false, pollMs, idleMs))
        assertFalse(awaitScreenWake(1L, Channel(Channel.CONFLATED)))
    }
}
