import QtQuick
import Quickshell
import Quickshell.Io
import "Status.js" as Status

QtObject {
    id: root
    property string socketPath: ""
    property var view: Status.presentation(null, "", 0, "Connecting to daemon")
    property var client: null
    readonly property string effectivePath: socketPath || Quickshell.env("YASH_APP_EVENTS_SOCKET")
        || (Quickshell.env("XDG_RUNTIME_DIR")
            ? Quickshell.env("XDG_RUNTIME_DIR") + "/yash-app-events/control.sock" : "")

    function refresh() {
        if (client) client.refresh(Date.now());
        if (!socket.connected) reconnect.restart();
    }

    onEffectivePathChanged: {
        socket.connected = false;
        if (client) client.disconnected("Control socket changed");
        reconnect.restart();
    }

    Component.onCompleted: {
        client = Status.createClient({
            send: function(line) { socket.write(line); socket.flush(); },
            changed: function() { root.view = root.client.view(Date.now()); },
            disconnect: function() { socket.connected = false; reconnect.restart(); }
        });
        reconnect.restart();
    }

    property Socket transport: Socket {
        id: socket
        path: root.effectivePath
        onConnectedChanged: {
            if (!root.client) return;
            if (connected) {
                reconnect.stop();
                root.client.connected(Date.now());
            } else {
                root.client.disconnected();
                reconnect.restart();
            }
        }
        onError: {
            if (root.client) root.client.disconnected("Cannot connect to daemon control socket");
            reconnect.restart();
        }
        parser: SplitParser {
            onRead: function(line) {
                if (root.client) root.client.receive(line, Date.now());
            }
        }
    }

    property Timer reconnectTimer: Timer {
        id: reconnect
        interval: Status.pollMs
        onTriggered: {
            if (root.effectivePath) socket.connected = true;
            else {
                root.client.disconnected("XDG_RUNTIME_DIR is unset; configure socketPath");
                restart();
            }
        }
    }

    property Timer pollTimer: Timer {
        interval: Status.pollMs
        running: true
        repeat: true
        onTriggered: if (root.client) root.client.poll(Date.now())
    }

    property Timer watchdog: Timer {
        interval: 1000
        running: true
        repeat: true
        onTriggered: if (root.client) root.client.tick(Date.now())
    }
}
