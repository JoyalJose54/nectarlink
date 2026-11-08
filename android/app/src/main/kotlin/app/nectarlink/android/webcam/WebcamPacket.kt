// SPDX-License-Identifier: GPL-3.0-or-later
package app.nectarlink.android.webcam

import java.nio.ByteBuffer

/** Copies codec output directly into the final owned Annex B packet. */
internal object WebcamPacket {
    fun frame(
        source: ByteBuffer,
        offset: Int,
        size: Int,
        parameters: ByteArray,
        keyframe: Boolean,
        codec: String,
    ): ByteArray {
        require(offset >= 0 && size >= 0 && offset <= source.limit() - size)
        val hevc = codec == "h265"
        val aud = if (hevc) HEVC_AUD_NAL else AVC_AUD_NAL
        val end = offset + size
        val dataStart = afterAud(source, offset, end, hevc)
        val params = if (keyframe && parameters.isNotEmpty()) ByteBuffer.wrap(parameters) else null
        val paramsStart = if (params != null) afterAud(params, 0, params.limit(), hevc) else 0
        val paramsSize = if (params != null) params.limit() - paramsStart else 0
        return ByteArray(aud.size + paramsSize + end - dataStart).also { packet ->
            System.arraycopy(aud, 0, packet, 0, aud.size)
            if (paramsSize > 0) System.arraycopy(parameters, paramsStart, packet, aud.size, paramsSize)
            // A duplicate preserves the MediaCodec buffer's position and limit.
            source.duplicate().apply { position(dataStart); limit(end) }
                .get(packet, aud.size + paramsSize, end - dataStart)
        }
    }

    private fun afterAud(data: ByteBuffer, start: Int, end: Int, hevc: Boolean): Int {
        fun startCode(at: Int): Int {
            if (at + 3 > end || data.get(at) != 0.toByte() || data.get(at + 1) != 0.toByte()) return 0
            if (data.get(at + 2) == 1.toByte()) return 3
            return if (at + 4 <= end && data.get(at + 2) == 0.toByte() && data.get(at + 3) == 1.toByte()) 4 else 0
        }
        val prefix = startCode(start)
        if (prefix == 0 || start + prefix >= end) return start
        val type = if (hevc) (data.get(start + prefix).toInt() ushr 1) and 0x3F else data.get(start + prefix).toInt() and 0x1F
        if (type != if (hevc) 35 else 9) return start
        for (at in start + prefix + 1 until end) if (startCode(at) != 0) return at
        return end
    }

    private val AVC_AUD_NAL = byteArrayOf(0, 0, 0, 1, 9, 0xF0.toByte())
    private val HEVC_AUD_NAL = byteArrayOf(0, 0, 0, 1, 0x46, 1, 0x50)
}
