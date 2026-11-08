// SPDX-License-Identifier: GPL-3.0-or-later
import QtQuick
import QtQuick.Effects
import app.nectarlink

// One file transfer: direction, name, progress, speed and time left, and
// what can be done with it (cancel while it runs; open or show what arrived).
Item {
    id: item
    required property string transferId
    required property string deviceName
    required property bool incoming
    required property bool recording
    required property string title
    required property real total
    required property real done
    required property real progress
    required property real rate
    required property string status
    required property string reason
    required property string savedPath

    readonly property bool active: status === "requested" || status === "waiting" || status === "running" || status === "paused"
    readonly property bool showBar: status === "waiting" || status === "running" || status === "paused"
    readonly property bool motion: !Theme.reduceMotion && item.visible
    // Progress as drawn. Updates arrive a few times a second; easing each one
    // over about the same time keeps the bar gliding instead of stepping.
    property real shown: status === "done" ? 1 : Math.max(0, Math.min(1, progress))
    Behavior on shown { enabled: item.motion; NumberAnimation { duration: 260 } }

    implicitHeight: column.height + 20

    function size(bytes) {
        if (bytes < 1024) return qsTr("%1 B").arg(Math.round(bytes))
        const units = [qsTr("%1 KB"), qsTr("%1 MB"), qsTr("%1 GB"), qsTr("%1 TB")]
        let value = bytes / 1024
        let unit = 0
        while (value >= 1024 && unit < units.length - 1) { value /= 1024; unit++ }
        return units[unit].arg(value < 10 ? Qt.locale().toString(value, "f", 1) : Math.round(value))
    }

    // Rounded so it doesn't flicker with every speed sample.
    function timeLeft() {
        if (status !== "running" || rate <= 0 || total <= done) return ""
        const seconds = (total - done) / rate
        if (seconds < 10) return qsTr("A few seconds left")
        if (seconds < 60) return qsTr("%1 s left").arg(Math.ceil(seconds / 5) * 5)
        if (seconds < 3600) return qsTr("%1 min left").arg(Math.ceil(seconds / 60))
        return qsTr("%1 h %2 min left").arg(Math.floor(seconds / 3600)).arg(Math.floor((seconds % 3600) / 60))
    }

    function detail() {
        switch (status) {
        case "requested":
            return qsTr("%1 wants to send this · %2")
                .arg(item.deviceName.length > 0 ? item.deviceName : qsTr("Nearby device"))
                .arg(size(total))
        case "waiting":
            return item.incoming ? qsTr("Paused; continues when %1 is back").arg(item.deviceName)
                                 : qsTr("Waiting for %1").arg(item.deviceName)
        case "running": {
            const left = timeLeft()
            const sizes = rate > 0
                ? qsTr("%1 of %2 · %3/s").arg(size(done)).arg(size(total)).arg(size(rate))
                : qsTr("%1 of %2").arg(size(done)).arg(size(total))
            return left.length > 0 ? sizes + " · " + left : sizes
        }
        case "paused":
            return qsTr("Paused · %1 of %2").arg(size(done)).arg(size(total))
        case "done":
            return item.incoming ? qsTr("From %1 · %2").arg(item.deviceName).arg(size(total))
                                 : qsTr("Sent to %1 · %2").arg(item.deviceName).arg(size(total))
        case "cancelled":
            return qsTr("Cancelled")
        default:
            switch (reason) {
            case "denied": return qsTr("%1 doesn't accept files from this PC").arg(item.deviceName)
            case "unreachable": return qsTr("Couldn't reach %1").arg(item.deviceName)
            case "noSpace": return qsTr("Not enough space on %1").arg(item.deviceName)
            case "integrity": return qsTr("Integrity check failed · partial file kept to resume")
            default: return qsTr("Didn't go through")
            }
        }
    }

    Row {
        id: row
        x: 4; y: 10
        width: parent.width - 8
        spacing: 12

        Rectangle {
            width: 32; height: 32
            radius: Theme.graphite ? Theme.radiusSm : 16
            color: Theme.graphite ? "transparent" : Theme.secondaryContainer
            border.width: Theme.graphite ? 1 : 0
            border.color: Theme.outlineVariant
            Icon {
                anchors.centerIn: parent
                width: 18; height: 18
                path: item.incoming ? (item.recording ? Icons.mic : Icons.folder) : Icons.send
                color: Theme.graphite ? Theme.surfaceContent : Theme.secondaryContainerContent
            }
        }

        Column {
            id: column
            width: row.width - 32 - actions.width - row.spacing * 2
            Item {
                width: parent.width
                height: titleText.height
                Txt {
                    id: titleText
                    width: parent.width - (percent.visible ? percent.width + 8 : 0)
                    role: "title"
                    size: 14
                    text: item.title
                }
                Txt {
                    id: percent
                    anchors.right: parent.right
                    anchors.verticalCenter: titleText.verticalCenter
                    visible: item.showBar && item.total > 0
                    role: "bodySmall"
                    weight: 600
                    color: item.status === "running" ? Theme.primary : Theme.surfaceContentVariant
                    text: qsTr("%1%").arg(Math.floor(item.shown * 100))
                }
            }
            // One bar for the whole transfer. A highlight sweeps along what's
            // been sent while it moves, waiting breathes, paused holds still,
            // and when it's done the bar fills up, then folds away.
            Item {
                id: barSlot
                width: parent.width
                height: item.showBar ? 16 : 0
                opacity: item.showBar ? 1 : 0
                clip: true
                Accessible.role: Accessible.ProgressBar
                Accessible.name: item.title
                Accessible.description: percent.text
                Behavior on height {
                    enabled: item.motion
                    SequentialAnimation {
                        PauseAnimation { duration: item.showBar ? 0 : 360 }
                        NumberAnimation { duration: 220; easing.type: Easing.OutCubic }
                    }
                }
                Behavior on opacity {
                    enabled: item.motion
                    SequentialAnimation {
                        PauseAnimation { duration: item.showBar ? 0 : 360 }
                        NumberAnimation { duration: 180 }
                    }
                }

                Rectangle {
                    id: track
                    width: parent.width
                    height: 6
                    radius: 3
                    anchors.verticalCenter: parent.verticalCenter
                    color: Theme.surfaceContainerHighest
                }
                // The filled part and its highlight, cut to the fill's rounded
                // shape (a plain clip only cuts rectangles).
                Item {
                    id: fill
                    anchors.fill: track
                    readonly property real reach: item.shown <= 0 ? 0 : Math.max(track.height, track.width * item.shown)
                    property real breath: 0.55
                    opacity: item.status === "waiting" ? breath : item.status === "paused" ? 0.45 : 1
                    SequentialAnimation on breath {
                        running: item.status === "waiting" && item.motion
                        loops: Animation.Infinite
                        NumberAnimation { to: 0.8; duration: 900; easing.type: Easing.InOutSine }
                        NumberAnimation { to: 0.4; duration: 900; easing.type: Easing.InOutSine }
                    }
                    layer.enabled: width > 0 && height > 0
                    layer.smooth: true
                    layer.effect: MultiEffect {
                        maskEnabled: true
                        maskSource: fillMask
                        maskThresholdMin: 0.5
                        maskSpreadAtMin: 1.0
                    }
                    Rectangle {
                        width: fill.reach
                        height: parent.height
                        color: Theme.primary
                    }
                    Rectangle {
                        id: sheen
                        readonly property real span: Math.min(72, Math.max(24, fill.reach * 0.6))
                        property real at: 0
                        visible: item.status === "running" && item.motion && fill.reach > span
                        x: -span + (fill.reach + span) * at
                        width: span
                        height: parent.height
                        gradient: Gradient {
                            orientation: Gradient.Horizontal
                            GradientStop { position: 0; color: "transparent" }
                            GradientStop { position: 0.5; color: Qt.rgba(1, 1, 1, Theme.dark ? 0.55 : 0.45) }
                            GradientStop { position: 1; color: "transparent" }
                        }
                        NumberAnimation on at {
                            running: sheen.visible
                            loops: Animation.Infinite
                            from: 0; to: 1
                            duration: 1500
                            easing.type: Easing.InOutQuad
                        }
                    }
                }
                Item {
                    id: fillMask
                    anchors.fill: track
                    visible: false
                    layer.enabled: true
                    layer.smooth: true
                    Rectangle {
                        width: fill.reach
                        height: parent.height
                        radius: height / 2
                    }
                }
            }
            Txt {
                width: parent.width
                role: "bodySmall"
                muted: true
                color: item.status === "failed" ? Theme.error : Theme.surfaceContentVariant
                text: item.detail()
            }
        }

        Row {
            id: actions
            anchors.verticalCenter: parent.verticalCenter
            spacing: 4
            Button {
                visible: item.status === "requested"
                variant: "fill"
                size: "sm"
                text: qsTr("Accept")
                onClicked: TransferList.accept(item.transferId)
            }
            Button {
                visible: item.status === "requested"
                variant: "outline"
                size: "sm"
                text: qsTr("Decline")
                onClicked: TransferList.cancel(item.transferId)
            }
            Button {
                visible: item.status === "done" && item.incoming && item.savedPath.length > 0
                variant: "text"
                size: "sm"
                text: qsTr("Open")
                onClicked: TransferList.open(item.transferId)
            }
            IconButton {
                visible: item.status === "done" && item.incoming && item.savedPath.length > 0
                iconPath: Icons.folder
                label: qsTr("Show in folder")
                onClicked: TransferList.showInFolder(item.transferId)
            }
            IconButton {
                visible: item.status === "waiting" || item.status === "running"
                iconPath: Icons.pause
                label: qsTr("Pause")
                onClicked: TransferList.pause(item.transferId)
            }
            IconButton {
                visible: item.status === "paused"
                iconPath: Icons.play
                label: qsTr("Resume")
                onClicked: TransferList.resume(item.transferId)
            }
            IconButton {
                visible: item.status === "waiting" || item.status === "running" || item.status === "paused"
                iconPath: Icons.close
                label: qsTr("Cancel")
                onClicked: TransferList.cancel(item.transferId)
            }
        }
    }
}
