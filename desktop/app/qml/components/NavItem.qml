// SPDX-License-Identifier: GPL-3.0-or-later
import QtQuick
import app.nectarlink

// A navigation rail destination: an icon in a pill indicator above a label.
FocusScope {
    id: item
    property string iconPath
    property string text
    property bool selected: false
    readonly property alias hovered: mouse.containsMouse
    property bool activeHover: mouse.containsMouse
    signal clicked

    implicitWidth: 72
    implicitHeight: 56
    activeFocusOnTab: true

    Accessible.role: Accessible.PageTab
    Accessible.name: text
    Accessible.selected: selected
    Accessible.onPressAction: item.clicked()
    Keys.onPressed: (event) => {
        if (event.key === Qt.Key_Return || event.key === Qt.Key_Enter || event.key === Qt.Key_Space) {
            item.clicked()
            event.accepted = true
        }
    }

    Rectangle {
        id: pill
        anchors.horizontalCenter: parent.horizontalCenter
        y: 2
        width: 52; height: 30
        radius: Theme.pill(height)
        color: "transparent"
        scale: mouse.pressed ? Theme.pressScale : 1.0
        Behavior on scale { enabled: !Theme.reduceMotion; NumberAnimation { duration: 120; easing.type: Easing.OutCubic } }

        Rectangle {
            anchors.centerIn: parent
            height: parent.height
            width: item.selected ? parent.width : (item.activeHover ? 42 : 24)
            radius: parent.radius
            color: item.selected
                ? (Theme.glass
                    ? Qt.tint(Qt.rgba(1, 1, 1, Theme.dark ? 0.14 : 0.62), Qt.rgba(Theme.primary.r, Theme.primary.g, Theme.primary.b, Theme.dark ? 0.28 : 0.22))
                    : Theme.indicator)
                : Theme.surfaceContent
            opacity: item.selected ? 1.0 : (item.activeHover ? 0.08 : 0.0)
            border.width: Theme.glass && item.selected ? 1 : 0
            border.color: Qt.rgba(1, 1, 1, Theme.dark ? 0.28 : 0.78)
            Behavior on width {
                enabled: !Theme.reduceMotion
                NumberAnimation { duration: 180; easing.type: Easing.OutCubic }
            }
            Behavior on opacity {
                enabled: !Theme.reduceMotion
                NumberAnimation { duration: Theme.fadeFast; easing.type: Easing.OutCubic }
            }
            Behavior on color {
                enabled: !Theme.reduceMotion
                ColorAnimation { duration: Theme.fadeFast }
            }
        }

        Icon {
            anchors.centerIn: parent
            path: item.iconPath
            color: item.selected ? Theme.indicatorContent : (item.activeHover ? Theme.surfaceContent : Theme.surfaceContentVariant)
            scale: item.selected ? 1.05 : (item.activeHover ? 1.02 : 1.0)
            Behavior on scale {
                enabled: !Theme.reduceMotion
                NumberAnimation { duration: 160; easing.type: Easing.OutCubic }
            }
            Behavior on color {
                enabled: !Theme.reduceMotion
                ColorAnimation { duration: Theme.fadeFast }
            }
        }
    }
    Txt {
        anchors.horizontalCenter: parent.horizontalCenter
        anchors.top: pill.bottom
        anchors.topMargin: 4
        text: item.text
        role: "caption"
        weight: Theme.graphite ? 400 : (item.selected ? 600 : 500)
        color: item.selected || item.activeHover ? Theme.surfaceContent : Theme.surfaceContentVariant
        Behavior on color {
            enabled: !Theme.reduceMotion
            ColorAnimation { duration: Theme.fadeFast }
        }
    }
    Rectangle {
        anchors.fill: pill
        anchors.margins: -3
        radius: pill.radius + 3
        color: "transparent"
        border.width: 2
        border.color: Theme.primary
        visible: Theme.focusVisible(item)
    }
    MouseArea {
        id: mouse
        anchors.fill: parent
        hoverEnabled: true
        cursorShape: Qt.PointingHandCursor
        onClicked: item.clicked()
    }
}
