// SPDX-License-Identifier: GPL-3.0-or-later
import QtQuick
import QtTest
import app.nectarlink

// A transfer shows one bar with its percent, sizes, speed and time left, and
// folds the bar away once it's done.
Item {
    id: root
    width: 480
    height: 120

    TransferItem {
        id: transfer
        width: 460
        transferId: "t1"
        deviceName: "Pixel"
        incoming: false
        recording: false
        title: "holiday.mp4"
        total: 100 * 1024 * 1024
        done: 0
        progress: 0
        rate: 0
        status: "running"
        reason: ""
        savedPath: ""
    }

    TestCase {
        name: "TransferItem"
        when: windowShown

        function init() {
            transfer.status = "running"
            transfer.total = 100 * 1024 * 1024
            transfer.done = 0
            transfer.progress = 0
            transfer.rate = 0
        }

        function run(done, rate) {
            transfer.done = done
            transfer.progress = done / transfer.total
            transfer.rate = rate
        }

        function test_one_line_has_sizes_speed_and_time_left() {
            run(80 * 1024 * 1024, 1024 * 1024)
            compare(transfer.detail(), "80 MB of 100 MB · 1.0 MB/s · 20 s left")
        }

        function test_time_left_is_rounded() {
            compare(transfer.timeLeft(), "", "no speed yet, no guess")
            run(0, 1024 * 1024)
            compare(transfer.timeLeft(), "2 min left")
            transfer.total = 4 * 1024 * 1024 * 1024
            run(0, 1024 * 1024)
            compare(transfer.timeLeft(), "1 h 8 min left")
            transfer.total = 100 * 1024 * 1024
            run(99 * 1024 * 1024, 1024 * 1024)
            compare(transfer.timeLeft(), "A few seconds left")
            transfer.status = "paused"
            compare(transfer.timeLeft(), "", "paused transfers have no estimate")
        }

        function test_the_percent_follows_the_bar() {
            run(42 * 1024 * 1024, 1024 * 1024)
            tryCompare(transfer, "shown", 0.42, 1000)
            verify(hasText(transfer, "42%"))
        }

        function test_the_bar_folds_away_when_done() {
            run(50 * 1024 * 1024, 1024 * 1024)
            tryVerify(() => hasText(transfer, "50%"), 1000)
            transfer.status = "done"
            compare(transfer.shown, 1, "a finished transfer is drawn full")
            verify(!hasText(transfer, "100%"), "no percent once it's done")
            compare(transfer.detail(), "Sent to Pixel · 100 MB")
        }

        // Whether a visible text item under `item` shows `text`.
        function hasText(item, text) {
            if (item.visible === false) return false
            if (item.text === text) return true
            for (let i = 0; i < item.children.length; i++) {
                if (hasText(item.children[i], text)) return true
            }
            return false
        }
    }
}
