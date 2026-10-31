import QtQuick
import Quickshell
import Quickshell.Io
import qs.Commons
import qs.Ui

// Bar front-end for the magiclick daemon. Holds no state of its own and makes
// no decisions: it reads four files and calls two commands.
//
//   off     nothing is being watched
//   idle    auto-copy is on, waiting for a selection
//   armed   a selection was just copied; a click would paste
//
// Seeing "armed" is the safety feature. You can tell when the gun is loaded.
//
// Left click turns auto-copy on and off. Right click opens a menu that sets how
// long an armed selection waits, in whole multiples of the configured
// `arm_timeout_ms`, for the flow where you select a thing, go looking for where
// it goes, and only then come back to it.
Panel {
  id: root
  moduleName: "dek.magiclick"
  // The shell routes summon/hide/toggle to bar widgets itself, so no second
  // handler: `omarchy shell shell summon dek.magiclick` opens the menu.
  manageIpc: false

  readonly property string state: (stateFile.text() || "off").trim()
  readonly property string window: (timeoutFile.text() || "").trim()
  readonly property bool off: state === "off"
  readonly property bool armed: state === "armed"
  readonly property string armNote: root.window.length > 0 ? " · arm " + root.window : ""

  // Published by the daemon: the scale in force, and the config value it is a
  // multiple of. Defaults match the daemon so the menu is still sensible when
  // nothing is running.
  readonly property int scale: Math.max(1, Math.min(4, parseInt((scaleFile.text() || "1").trim(), 10) || 1))
  readonly property int baseMs: Math.max(1000, parseInt((baseFile.text() || "").trim(), 10) || 30000)

  readonly property color ink: root.bar ? root.bar.foreground : Color.foreground
  readonly property color dim: Qt.darker(root.ink, 1.55)
  readonly property string family: root.bar ? root.bar.fontFamily : Style.font.family

  // Long enough to be useful, short enough to read. Every row states what it
  // costs in seconds, because "4x" alone tells you nothing about the risk.
  readonly property var options: [
    { scale: 1, note: "The default. Select the text, click where it goes." },
    { scale: 2, note: "Switch window or workspace and still make it." },
    { scale: 3, note: "A detour: open a tab, find the spot, come back." },
    { scale: 4, note: "The longest it holds. Back to 1x at logout." }
  ]

  function seconds(n) { return Math.round(root.baseMs * n / 1000) + "s" }
  function label(n) { return n + "× · " + root.seconds(n) }

  function choose(n) {
    root.close()
    setter.command = ["magiclick", "arm-timeout", String(n)]
    setter.running = true
  }

  function stopWatching() {
    root.close()
    stopper.running = true
  }

  implicitWidth: button.implicitWidth
  implicitHeight: button.implicitHeight

  FileView {
    id: stateFile
    path: Quickshell.env("XDG_RUNTIME_DIR") + "/magiclick/state"
    watchChanges: true
    onFileChanged: reload()
  }

  // Written by the daemon, so the tooltip reports the window actually in force
  // rather than what the config file says it should be.
  FileView {
    id: timeoutFile
    path: Quickshell.env("XDG_RUNTIME_DIR") + "/magiclick/timeout"
    watchChanges: true
    onFileChanged: reload()
  }

  FileView {
    id: scaleFile
    path: Quickshell.env("XDG_RUNTIME_DIR") + "/magiclick/arm_scale"
    watchChanges: true
    onFileChanged: reload()
  }

  FileView {
    id: baseFile
    path: Quickshell.env("XDG_RUNTIME_DIR") + "/magiclick/arm_base"
    watchChanges: true
    onFileChanged: reload()
  }

  Process {
    id: toggler
    command: ["magiclick", "enable"]
  }

  Process {
    id: stopper
    command: ["magiclick", "disable"]
  }

  Process {
    id: setter
    command: ["magiclick", "arm-timeout"]
  }

  BarIconButton {
    id: button
    anchors.fill: parent
    bar: root.bar
    // times / copy / clipboard
    text: root.off ? "\uF00D" : (root.armed ? "\uF0EA" : "\uF0C5")
    active: root.armed
    tooltipText: root.off
      ? "MagiClick off — click to turn auto-copy on"
      : (root.armed
        ? "Armed" + root.armNote + " — click a field to paste"
        : "Auto-copy on" + root.armNote + " — click for the auto-copy window")
    onPressed: function(b) {
      // Starting is one click. Stopping is a second, deliberate click inside
      // the menu: a stray click on the bar should not silently disarm the thing
      // whose whole job is to tell you it is armed.
      if (b === Qt.LeftButton) {
        if (root.off) {
          root.close()
          toggler.running = true
        } else {
          root.toggle()
        }
      } else if (b === Qt.RightButton) {
        root.toggle()
      }
    }
  }

  KeyboardPanel {
    id: panel
    anchorItem: button
    owner: root
    bar: root.bar
    open: root.opened
    focusTarget: keyCatcher
    contentWidth: panel.fittedContentWidth(Style.space(300))
    contentHeight: panel.fittedContentHeight(content.implicitHeight, Style.space(520))

    PanelKeyCatcher {
      id: keyCatcher
      anchors.fill: parent
      onCloseRequested: root.close()

      Column {
        id: content
        width: parent.width
        spacing: Style.space(4)

        Text {
          width: parent.width
          textFormat: Text.PlainText
          text: "Auto-Copy Window"
          color: root.ink
          font.family: root.family
          font.pixelSize: Style.font.title
          font.bold: true
        }

        Text {
          width: parent.width
          textFormat: Text.PlainText
          wrapMode: Text.Wrap
          text: "How long your copied text is held for paste, then voided. Default min. 30s. (Helps prevent accidental paste.)"
          color: root.dim
          font.family: root.family
          font.pixelSize: Style.font.body
          bottomPadding: Style.space(6)
        }

        Repeater {
          model: root.options

          delegate: Column {
            required property var modelData
            width: content.width
            spacing: 0

            Button {
              width: parent.width
              leftAlign: true
              bordered: false
              selected: root.scale === modelData.scale
              text: root.label(modelData.scale)
              foreground: root.ink
              fontFamily: root.family
              onClicked: root.choose(modelData.scale)
            }

            Text {
              width: parent.width
              leftPadding: Style.spacing.controlPaddingX
              rightPadding: Style.spacing.controlPaddingX
              bottomPadding: Style.space(4)
              textFormat: Text.PlainText
              wrapMode: Text.Wrap
              text: modelData.note
              color: root.dim
              font.family: root.family
              font.pixelSize: Style.font.bodySmall
            }
          }
        }

        PanelSeparator {
          width: parent.width
          foreground: root.ink
        }

        Text {
          width: parent.width
          textFormat: Text.PlainText
          wrapMode: Text.Wrap
          text: "Session only: Your config stays this until changed again or restart."
          color: root.dim
          font.family: root.family
          font.pixelSize: Style.font.bodySmall
          topPadding: Style.space(2)
          bottomPadding: Style.space(6)
        }

        PanelSeparator {
          width: parent.width
          foreground: root.ink
        }

        // The one destructive action in the panel. Borderless rows read as
        // text, so this one is drawn as a control and carries the theme's
        // urgent token: it should not look like another choice.
        Button {
          width: content.width
          leftAlign: true
          bordered: true
          iconText: "\uF04D"
          text: "Disable"
          foreground: Color.urgent
          fontFamily: root.family
          onClicked: root.stopWatching()
        }

        Text {
          width: parent.width
          leftPadding: Style.spacing.controlPaddingX
          rightPadding: Style.spacing.controlPaddingX
          textFormat: Text.PlainText
          wrapMode: Text.Wrap
          text: "Left click glyph turns auto-copy back on."
          color: root.dim
          font.family: root.family
          font.pixelSize: Style.font.bodySmall
        }
      }
    }
  }
}
