import QtQuick
import Quickshell
import Quickshell.Io
import qs.Ui

// Bar front-end for the noclickcopy daemon. Holds no state of its own and makes
// no decisions: it reads two files and calls one command.
//
//   off     nothing is being watched
//   idle    watching for drags
//   armed   a drag just happened; a confirm would paste
//
// Seeing "armed" is the safety feature. You can tell when the gun is loaded.
BarWidget {
  id: root
  moduleName: "dek.noclickcopy"

  readonly property string state: (stateFile.text() || "off").trim()
  readonly property bool off: state === "off"
  readonly property bool armed: state === "armed"

  implicitWidth: button.implicitWidth
  implicitHeight: button.implicitHeight

  FileView {
    id: stateFile
    path: Quickshell.env("XDG_RUNTIME_DIR") + "/noclickcopy/state"
    watchChanges: true
    onFileChanged: reload()
  }

  Process {
    id: toggler
    command: ["noclickcopy", "toggle"]
  }

  BarIconButton {
    id: button
    anchors.fill: parent
    bar: root.bar
    // times / copy / clipboard
    text: root.off ? "\uF00D" : (root.armed ? "\uF0EA" : "\uF0C5")
    active: root.armed
    tooltipText: root.off
      ? "NoClickCopy off — click to start watching"
      : (root.armed
        ? "Armed — click a field, then confirm to paste"
        : "Watching for drags — click to turn off")
    onPressed: function(b) {
      if (b === Qt.LeftButton || b === Qt.RightButton) toggler.running = true
    }
  }
}
