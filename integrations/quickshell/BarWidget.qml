import QtQuick
import Quickshell
import qs.Ui

BarWidget {
    id: root
    moduleName: "io.github.yash-app-events.status"
    implicitWidth: button.implicitWidth
    implicitHeight: button.implicitHeight

    StatusService {
        id: service
        socketPath: String(root.setting("socketPath", ""))
    }

    BarIconButton {
        id: button
        anchors.fill: parent
        bar: root.bar
        active: service.view.alarming
        dimmed: service.view.label === "Offline" || service.view.label === "Idle"
            || service.view.label === "Stopped"
        iconComponent: Item {
            Image {
                anchors.fill: parent
                source: Qt.resolvedUrl("yash-events.svg")
                sourceSize: Qt.size(64, 64)
                fillMode: Image.PreserveAspectFit
            }
            Rectangle {
                anchors.right: parent.right
                anchors.bottom: parent.bottom
                width: 6
                height: 6
                radius: 3
                color: service.view.alarming ? button.activeColor
                    : service.view.label === "Processing" ? "#75d69c"
                    : service.view.label === "Waiting" ? "#edc36c"
                    : service.view.label === "Capturing" ? "#82d8ff"
                    : button.foreground
            }
        }
        tooltipText: service.view.tooltip
        onPressed: function(mouseButton) {
            if (mouseButton === Qt.RightButton) service.refresh();
            else if (mouseButton === Qt.LeftButton)
                Quickshell.execDetached([String(root.setting("guiPath", "yash-app-events"))]);
        }
    }
}
