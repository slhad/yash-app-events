import QtQuick
import Quickshell
// The smoke runner copies this file beside StatusService.qml and Status.js.
import "." as Yash

ShellRoot {
    Image {
        width: 24
        height: 24
        source: Qt.resolvedUrl("yash-events.svg")
        onStatusChanged: {
            if (status === Image.Ready) console.log("YASH_ICON_READY");
        }
    }
    Yash.StatusService {
        socketPath: Quickshell.env("YASH_QUICKSHELL_TEST_SOCKET")
        onViewChanged: console.log("YASH_VIEW " + JSON.stringify(view))
    }
}
