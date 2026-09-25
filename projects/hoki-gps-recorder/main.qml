// SPDX-License-Identifier: GPL-3.0-only
import QtQuick
import org.asteroid.controls
import Hoki.Gps 1.0

Application {
    id: app
    centerColor: "#b04d1c"
    outerColor: "#421c0a"
    GeoClueRecorder { id: recorder }
    Column {
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
            text: recorder.status
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
            text: recorder.fixAge < 0 ? qsTr("No fresh fix") : (recorder.recording ? qsTr("Last fix %1s ago") : qsTr("Final fix age: %1s")).arg(recorder.fixAge)
            font.pixelSize: app.width * 0.040
        }
        Label {
            width: parent.width
            horizontalAlignment: Text.AlignHCenter
            text: recorder.detail
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
            text: recorder.error || (recorder.fileName ? (recorder.recording ? qsTr("Saving all GeoClue events") : qsTr("Saved on watch")) : qsTr("Records all GeoClue events"))
            color: recorder.error ? "#ffaaaa" : "#ffd5bd"
            font.pixelSize: app.width * 0.033
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
}
