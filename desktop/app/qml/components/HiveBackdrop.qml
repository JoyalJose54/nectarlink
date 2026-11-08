// SPDX-License-Identifier: GPL-3.0-or-later
import QtQuick

// The Hive theme's living honeycomb (shaders/hive.frag), behind the setup
// window and, in the Hive theme, the whole app: it grows out of `origin`,
// breathes slowly, and sends a soft pulse of light outward every few seconds.
//
// Light on resources: one fragment shader and no textures. Its clock ticks
// every `frameMs` (about 30 frames a second; the motion is slow enough that
// more looks the same). In the app every comb shares Theme's clock, so two
// combs never draw twice as often; the setup window runs its own. `paused`
// holds the current frame; `animate` false draws a still comb with no
// breathing or pulses (reduced motion, battery saver, the user's setting).
ShaderEffect {
    id: hive

    // Breathing and pulses (false: a still comb).
    property bool animate: true
    // Hold the current frame (its own clock only).
    property bool paused: false
    // Keep time itself (setup), rather than following `time` set by the app.
    property bool ownClock: true
    // Grows out from the origin when it first appears (when it animates).
    property bool growIn: true
    property real cell: 24
    property real strength: 1
    property color lineColor
    property color glowColor
    property point origin: Qt.point(width / 2, height / 3)
    property int frameMs: 33
    property real breathSeconds: 6.0
    property real pulseSeconds: 4.5
    // Rounded-rectangle mask radius (0: none), for the phone preview.
    property real corner: 0
    // How far the comb carries before it fades (a fraction of the diagonal;
    // larger spreads it evenly), and how far each pulse travels.
    property real reach: 0.62
    property real pulseReach: 0.75

    readonly property size itemSize: Qt.size(width, height)
    readonly property real pulse: animate ? 1 : 0
    readonly property real breathPeriod: breathSeconds
    readonly property real pulsePeriod: pulseSeconds
    property real time: 0
    // 0 to 1 as it grows out; the shader gets it in pixels.
    property real grow: 1
    readonly property real reveal: grow * Math.hypot(width, height)

    blending: true
    fragmentShader: "qrc:/shaders/hive.frag.qsb"

    // Its own clock: seconds carried across pauses, so it picks up where it
    // stopped rather than jumping.
    property real _base: 0
    property double _since: 0
    Timer {
        interval: hive.frameMs
        repeat: true
        running: hive.ownClock && hive.animate && !hive.paused && hive.visible && hive.width > 0
        onRunningChanged: {
            if (running)
                hive._since = Date.now()
            else
                hive._base = hive.time
        }
        onTriggered: hive.time = hive._base + (Date.now() - hive._since) / 1000
    }

    // Grows out once, when it first appears; never again after that.
    NumberAnimation {
        id: growAnimation
        target: hive
        property: "grow"
        from: 0
        to: 1
        duration: 1800
        easing.type: Easing.OutCubic
    }
    Component.onCompleted: {
        if (growIn && animate) {
            grow = 0
            growAnimation.start()
        }
    }
}
