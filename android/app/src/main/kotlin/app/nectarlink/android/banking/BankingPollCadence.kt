// SPDX-License-Identifier: GPL-3.0-or-later
package app.nectarlink.android.banking

import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.withTimeoutOrNull

/** A long screen-off wait needs a registered receiver that can end it. */
internal fun bankingPollWaitMs(screenInteractive: Boolean, wakeAvailable: Boolean, pollMs: Long, idleMs: Long): Long =
    if (screenInteractive || !wakeAvailable) pollMs else idleMs

/** Returns true when a screen return ended the wait. */
internal suspend fun awaitScreenWake(waitMs: Long, wake: Channel<Unit>): Boolean =
    withTimeoutOrNull(waitMs) { wake.receive(); true } ?: false
