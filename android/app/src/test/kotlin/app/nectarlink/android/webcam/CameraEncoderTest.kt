// SPDX-License-Identifier: GPL-3.0-or-later
package app.nectarlink.android.webcam

import org.junit.Assert.assertEquals
import org.junit.Test

class CameraEncoderTest {
    @Test
    fun `normalizes resolution height and selects matching width and bitrate`() {
        assertEquals(720, CameraEncoder.normalizeHeight(720, supports4k = false))
        assertEquals(1080, CameraEncoder.normalizeHeight(1080, supports4k = false))
        assertEquals(1080, CameraEncoder.normalizeHeight(2160, supports4k = false))
        assertEquals(2160, CameraEncoder.normalizeHeight(2160, supports4k = true))

        assertEquals(1280, CameraEncoder.widthForHeight(720))
        assertEquals(1920, CameraEncoder.widthForHeight(1080))
        assertEquals(3840, CameraEncoder.widthForHeight(2160))

        assertEquals(4_000_000, CameraEncoder.bitrateForHeight(720))
        assertEquals(8_000_000, CameraEncoder.bitrateForHeight(1080))
        assertEquals(20_000_000, CameraEncoder.bitrateForHeight(2160))
    }
}
