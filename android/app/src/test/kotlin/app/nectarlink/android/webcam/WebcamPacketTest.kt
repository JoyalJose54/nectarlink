// SPDX-License-Identifier: GPL-3.0-or-later
package app.nectarlink.android.webcam

import java.nio.ByteBuffer
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Test

class WebcamPacketTest {
    private val avcAud = byteArrayOf(0, 0, 0, 1, 9, 0xF0.toByte())
    private val hevcAud = byteArrayOf(0, 0, 0, 1, 0x46, 1, 0x50)

    private data class CodecFixture(
        val codec: String,
        val aud: ByteArray,
        val slice: ByteArray,
        val parameters: ByteArray,
    )

    @Test
    fun `keyframes carry all codec parameter sets after one codec specific AUD`() {
        val h264Parameters = byteArrayOf(
            0, 0, 0, 1, 0x67, 0x42,
            0, 0, 0, 1, 0x68, 0x11,
        )
        val h264Keyframe = byteArrayOf(0, 0, 0, 1, 0x65, 0x55)
        assertArrayEquals(
            avcAud + h264Parameters + h264Keyframe,
            WebcamPacket.frame(ByteBuffer.wrap(h264Keyframe), 0, h264Keyframe.size, h264Parameters, true, "h264"),
        )

        val hevcParameters = byteArrayOf(
            0, 0, 0, 1, 0x40, 1, 0x11,
            0, 0, 0, 1, 0x42, 1, 0x22,
            0, 0, 0, 1, 0x44, 1, 0x33,
        )
        val hevcKeyframe = byteArrayOf(0, 0, 0, 1, 0x26, 1, 0x55)
        assertArrayEquals(
            hevcAud + hevcParameters + hevcKeyframe,
            WebcamPacket.frame(ByteBuffer.wrap(hevcKeyframe), 0, hevcKeyframe.size, hevcParameters, true, "h265"),
        )
    }

    @Test
    fun `copies only the codec range from direct read-only buffers without changing cursor`() {
        val payload = byteArrayOf(0, 0, 0, 1, 0x26, 1, 0x55)
        val source = ByteBuffer.allocateDirect(64)
        source.position(7)
        source.put(payload)
        source.put(0x7F)
        source.limit(30)
        source.position(2)
        val readOnly = source.asReadOnlyBuffer()
        val packet = WebcamPacket.frame(readOnly, 7, payload.size, byteArrayOf(), false, "h265")
        assertArrayEquals(hevcAud + payload, packet)
        assertEquals(2, readOnly.position())
        assertEquals(30, readOnly.limit())
        source.put(7, 0x7E)
        assertArrayEquals(hevcAud + payload, packet)
    }

    @Test
    fun `strips only leading AUDs for both codecs including nonzero offsets`() {
        for (fixture in listOf(
            CodecFixture(
                "h264", avcAud,
                byteArrayOf(0, 0, 1, 0x65, 0x22),
                byteArrayOf(0, 0, 1, 0x67, 0x44),
            ),
            CodecFixture(
                "h265", hevcAud,
                byteArrayOf(0, 0, 1, 0x26, 1, 0x22),
                byteArrayOf(0, 0, 1, 0x40, 1, 0x44),
            ),
        )) {
            val shortAud = fixture.aud.copyOfRange(1, fixture.aud.size)
            val source = ByteBuffer.wrap(byteArrayOf(99, 98) + shortAud + fixture.slice + byteArrayOf(97))
            assertArrayEquals(
                fixture.aud + fixture.parameters + fixture.slice,
                WebcamPacket.frame(
                    source,
                    2,
                    shortAud.size + fixture.slice.size,
                    fixture.aud + fixture.parameters,
                    true,
                    fixture.codec,
                ),
            )
            assertArrayEquals(
                fixture.aud + fixture.slice,
                WebcamPacket.frame(
                    source,
                    2,
                    shortAud.size + fixture.slice.size,
                    fixture.parameters,
                    false,
                    fixture.codec,
                ),
            )
            assertArrayEquals(
                fixture.aud,
                WebcamPacket.frame(
                    ByteBuffer.wrap(fixture.aud),
                    0,
                    fixture.aud.size,
                    byteArrayOf(),
                    false,
                    fixture.codec,
                ),
            )
        }
    }

    @Test
    fun `supports empty input and sliced buffers`() {
        val slice = byteArrayOf(0, 0, 0, 1, 0x41, 0x55)
        val source = ByteBuffer.wrap(byteArrayOf(99, 98) + slice).apply { position(2) }.slice()
        assertArrayEquals(avcAud + slice, WebcamPacket.frame(source, 0, slice.size, byteArrayOf(), false, "h264"))
        assertArrayEquals(avcAud, WebcamPacket.frame(source, source.limit(), 0, byteArrayOf(), false, "h264"))
    }

    @Test(expected = IllegalArgumentException::class)
    fun `rejects a range extending beyond the codec buffer limit`() {
        WebcamPacket.frame(ByteBuffer.allocate(20).apply { limit(8) }, 7, 2, byteArrayOf(), false, "h264")
    }
}
