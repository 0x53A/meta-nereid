// SPDX-License-Identifier: GPL-3.0-only
import QtQuick
import org.asteroid.controls
import Hoki.Gps 1.0

Application {
    id: app
    centerColor: "#b04d1c"
    outerColor: "#421c0a"
    property bool reviewingClock: false
    GeoClueRecorder { id: recorder }
    Column {
        visible: !app.reviewingClock
        anchors.centerIn: parent
        width: parent.width * 0.72
        spacing: parent.height * 0.013
        Label {
            anchors.horizontalCenter: parent.horizontalCenter
            text: recorder.recording ? qsTr("● RECORDING") : qsTr("GPS RECORDER")
            color: recorder.recording ? "#ffb093" : "white"
            font.pixelSize: app.width * 0.055
        }
        Label {
            width: parent.width
            horizontalAlignment: Text.AlignHCenter
            text: recorder.clockMismatch ? qsTr("Clock mismatch") : recorder.status
            font.pixelSize: app.width * 0.049
        }
        Label {
            width: parent.width
            horizontalAlignment: Text.AlignHCenter
            text: qsTr("Signals %1 · Used %2 / %3").arg(recorder.detected).arg(recorder.used).arg(recorder.visible)
            font.pixelSize: app.width * 0.040
        }
        Label {
            width: parent.width
            horizontalAlignment: Text.AlignHCenter
            text: recorder.clockMismatch ? qsTr("Position received · check time") : recorder.fixAge < 0 ? qsTr("No fresh fix") : (recorder.recording ? qsTr("Last fix %1s ago") : qsTr("Final fix age: %1s")).arg(recorder.fixAge)
            font.pixelSize: app.width * 0.040
        }
        Label {
            width: parent.width
            horizontalAlignment: Text.AlignHCenter
            text: recorder.clockMismatch ? recorder.clockDifference : recorder.detail
            font.pixelSize: app.width * 0.038
        }
        Label {
            width: parent.width
            horizontalAlignment: Text.AlignHCenter
            text: Math.floor(recorder.elapsed / 60) + ":" + ("0" + recorder.elapsed % 60).slice(-2)
                  + " · " + qsTr("%1 events").arg(recorder.events)
            font.pixelSize: app.width * 0.040
        }
        Label {
            width: parent.width
            horizontalAlignment: Text.AlignHCenter
            wrapMode: Text.Wrap
            maximumLineCount: 2
            elide: Text.ElideRight
            text: recorder.error || recorder.clockSyncMessage || (recorder.fileName ? (recorder.recording ? qsTr("Saving all GeoClue events") : qsTr("Saved on watch")) : qsTr("Records all GeoClue events"))
            color: recorder.error ? "#ffaaaa" : "#ffd5bd"
            font.pixelSize: app.width * 0.033
        }
        Rectangle {
            visible: recorder.clockMismatch
            anchors.horizontalCenter: parent.horizontalCenter
            width: app.width * 0.52
            height: app.height * 0.116
            radius: height / 2
            color: clockNotice.pressed ? "#996546" : "#633b24"
            border.color: "#ffd5bd"
            Label {
                anchors.centerIn: parent
                text: qsTr("Review GPS time")
                font.pixelSize: app.width * 0.040
            }
            MouseArea { id: clockNotice; anchors.fill: parent; onClicked: app.reviewingClock = true }
        }
        Rectangle {
            anchors.horizontalCenter: parent.horizontalCenter
            width: app.width * 0.40
            height: app.height * 0.105
            radius: height / 2
            color: button.pressed ? "#996546" : "#633b24"
            border.color: "#ffd5bd"
            Label {
                anchors.centerIn: parent
                text: recorder.recording ? qsTr("Stop & save") : qsTr("Start recording")
                font.pixelSize: app.width * 0.039
            }
            MouseArea {
                id: button
                anchors.fill: parent
                onClicked: recorder.recording ? recorder.stop() : recorder.start()
            }
        }
    }
    Column {
        visible: app.reviewingClock
        anchors.centerIn: parent
        width: parent.width * 0.72
        spacing: app.height * 0.016
        Label {
            width: parent.width
            horizontalAlignment: Text.AlignHCenter
            text: qsTr("Sync watch time?")
            font.pixelSize: app.width * 0.052
        }
        Label {
            width: parent.width
            horizontalAlignment: Text.AlignHCenter
            wrapMode: Text.Wrap
            text: qsTr("Watch: %1\nGPS: %2").arg(recorder.localTimeText).arg(recorder.gpsTimeText)
            font.pixelSize: app.width * 0.036
        }
        Label {
            width: parent.width
            horizontalAlignment: Text.AlignHCenter
            wrapMode: Text.Wrap
            text: recorder.clockSyncMessage || (recorder.clockMismatch ? recorder.clockDifference : qsTr("No current clock mismatch"))
            maximumLineCount: 3
            elide: Text.ElideRight
            font.pixelSize: app.width * 0.039
        }
        Label {
            width: parent.width
            horizontalAlignment: Text.AlignHCenter
            wrapMode: Text.Wrap
            text: qsTr("GPS time is unauthenticated.\nSync changes the whole watch’s clock.")
            font.pixelSize: app.width * 0.034
        }
        Rectangle {
            anchors.horizontalCenter: parent.horizontalCenter
            width: app.width * 0.52
            height: app.height * 0.116
            radius: height / 2
            opacity: syncButton.enabled ? 1 : 0.45
            color: syncButton.pressed ? "#996546" : "#633b24"
            border.color: "#ffd5bd"
            Label {
                anchors.centerIn: parent
                text: recorder.syncingClock ? qsTr("Syncing…") : qsTr("Sync from GPS")
                font.pixelSize: app.width * 0.040
            }
            MouseArea {
                id: syncButton
                anchors.fill: parent
                enabled: recorder.clockMismatch && !recorder.syncingClock
                onClicked: recorder.syncClock()
            }
        }
        Rectangle {
            anchors.horizontalCenter: parent.horizontalCenter
            width: app.width * 0.40
            height: app.height * 0.116
            radius: height / 2
            color: backButton.pressed ? "#996546" : "#633b24"
            Label {
                anchors.centerIn: parent
                text: qsTr("Back")
                font.pixelSize: app.width * 0.040
            }
            MouseArea { id: backButton; anchors.fill: parent; onClicked: app.reviewingClock = false }
        }
    }
}
