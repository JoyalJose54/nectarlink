// SPDX-License-Identifier: GPL-3.0-or-later
import QtQuick
import app.nectarlink

// A full-width row with an icon, a title, an optional caption and a switch
// (Phone as speaker, Play on phone). It fills in while on; `enabled: false`
// greys it out until it can be used.
Rectangle {
    id: bar
    property string iconPath
    property string title
    // Shown under the title when not empty.
    property string caption
    property bool checked: false
    signal toggled(bool on)

    readonly property color ink: checked
        ? (Theme.graphite ? Theme.surface : Theme.secondaryContainerContent)
        : Theme.surfaceContent

    height: 44
    radius: Theme.radiusMd
    color: checked
        ? (Theme.graphite ? Theme.surfaceContent : Theme.secondaryContainer)
        : (Theme.graphite ? "transparent" : Theme.surfaceContainerHigh)
    border.width: Theme.graphite || checked ? 1 : 0
    border.color: checked ? Theme.primary : Theme.outlineVariant

    Row {
        anchors.left: parent.left
        anchors.leftMargin: 12
        anchors.right: barSwitch.left
        anchors.rightMargin: 8
        anchors.verticalCenter: parent.verticalCenter
        spacing: 10
        opacity: bar.enabled ? 1 : 0.38
        Icon {
            anchors.verticalCenter: parent.verticalCenter
            width: 17; height: 17
            path: bar.iconPath
            color: bar.ink
        }
        Column {
            width: parent.width - 27
            anchors.verticalCenter: parent.verticalCenter
            spacing: 1
            Txt {
                width: parent.width
                role: "label"
                elide: Text.ElideRight
                color: bar.ink
                text: bar.title
            }
            Txt {
                visible: bar.caption.length > 0
                width: parent.width
                role: "caption"
                elide: Text.ElideRight
                color: bar.ink
                opacity: 0.85
                text: bar.caption
            }
        }
    }
    Toggle {
        id: barSwitch
        anchors.right: parent.right
        anchors.rightMargin: 10
        anchors.verticalCenter: parent.verticalCenter
        label: bar.title
        checked: bar.checked
        onToggled: (on) => bar.toggled(on)
    }
}
