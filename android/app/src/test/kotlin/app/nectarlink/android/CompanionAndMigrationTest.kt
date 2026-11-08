// SPDX-License-Identifier: GPL-3.0-or-later
package app.nectarlink.android

import app.nectarlink.android.companion.PcCompanion
import app.nectarlink.android.ui.Preferences
import app.nectarlink.android.ui.theme.Appearance
import app.nectarlink.android.ui.theme.Tokens
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class CompanionAndMigrationTest {

    @Test
    fun `z-base-32 decodes like the core encodes device IDs`() {
        // The z-base-32 specification's own example.
        assertArrayEquals(
            byteArrayOf(0xF0.toByte(), 0xBF.toByte(), 0xC7.toByte()),
            PcCompanion.decodeZBase32("6n9hq"),
        )
        // The device ID whose 32 bytes are 0, 1, 2 ... 31, as the core writes it
        // (pinned on the Rust side in core/nectarlink-protocol/src/id.rs).
        assertArrayEquals(
            ByteArray(32) { it.toByte() },
            PcCompanion.decodeZBase32("yyyoryarywdyqnyjbefoadeqbhebnrounoktcfaadrpbs8y7daxo"),
        )
        assertEquals(null, PcCompanion.decodeZBase32("not-an-id"))
    }

    @Test
    fun `the scan filter matches the beacon a PC broadcasts for its device ID`() {
        // What desktop/app/src/win/ble_beacon.rs broadcasts: the magic, then the
        // ID's first 8 bytes, here A5 3C 00 FF 12 34 56 78, which no hex reading of
        // the ID could produce.
        val id = "ww6yb9a1gtm8tgw4ukpjigw4ukpjigw4ukpjigw4ukpjigw4ukpy"
        val broadcast = PcCompanion.BEACON_MAGIC + byteArrayOf(
            0xA5.toByte(), 0x3C, 0x00, 0xFF.toByte(), 0x12, 0x34, 0x56, 0x78,
        )
        assertArrayEquals(broadcast, PcCompanion.buildBeaconPayload(id))
        assertArrayEquals(broadcast, PcCompanion.buildBeaconPayload(id.uppercase()))

        val maskWithId = PcCompanion.buildBeaconMask(hasDeviceId = true)
        assertEquals(13, maskWithId.size)
        assertTrue(maskWithId.all { it == 0xFF.toByte() })

        // Not a device ID: match any NectarLink PC.
        assertArrayEquals(PcCompanion.BEACON_MAGIC, PcCompanion.buildBeaconPayload("abcd"))
        assertArrayEquals(PcCompanion.BEACON_MAGIC, PcCompanion.buildBeaconPayload("0123456789abcdef-fedcba9876543210"))
        assertArrayEquals(PcCompanion.BEACON_MAGIC, PcCompanion.buildBeaconPayload(null))
        val maskWithoutId = PcCompanion.buildBeaconMask(hasDeviceId = false)
        assertEquals(5, maskWithoutId.size)
        assertTrue(maskWithoutId.all { it == 0xFF.toByte() })
    }

    @Test
    fun `Preferences sanitizeAppearance falls back on unknown legacy theme, mode, and seed values`() {
        val legacy = Preferences.sanitizeAppearance(
            theme = "sepia",
            mode = "neon",
            seed = "cyberpunk",
            dynamicColor = false,
        )
        assertEquals("graphite", legacy.theme)
        assertEquals("system", legacy.mode)
        assertEquals(Tokens.DEFAULT_SEED, legacy.seed)
        assertFalse(legacy.dynamicColor)

        val empty = Preferences.sanitizeAppearance(
            Appearance(theme = "", mode = "", seed = "", dynamicColor = true),
        )
        assertEquals("graphite", empty.theme)
        assertEquals("system", empty.mode)
        assertEquals(Tokens.DEFAULT_SEED, empty.seed)
        assertTrue(empty.dynamicColor)

        val valid = Preferences.sanitizeAppearance(
            theme = "graphite",
            mode = "dark",
            seed = "ocean",
            dynamicColor = false,
        )
        assertEquals("graphite", valid.theme)
        assertEquals("dark", valid.mode)
        assertEquals("ocean", valid.seed)
        assertFalse(valid.dynamicColor)
    }
}
