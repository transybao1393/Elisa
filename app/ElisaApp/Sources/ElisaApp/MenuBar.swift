// The menu-bar item: the kit's 16 px template silhouette (05-character/shade/svg/elisa-shade-menubar-light-16.svg,
// "one-colour silhouette, eyes cut out"; fallback 06-icons/menubar/elisa-icon-menubar-light-16.svg), drawn by AppKit as
// a vector template image (black on a light bar, white on a dark one, sharp at any scale). Menu: show the window, the
// current state, interrupt, quit.
import AppKit

@MainActor
final class MenuBarItem: NSObject, NSMenuDelegate {
    static let shared = MenuBarItem()
    private var item: NSStatusItem?
    private weak var m: AppModel?
    private let stateItem = NSMenuItem(title: "", action: nil, keyEquivalent: "")
    private let interruptItem = NSMenuItem(title: "Interrupt", action: #selector(interrupt), keyEquivalent: "")

    static let iconPaths = ["05-character/shade/svg/elisa-shade-menubar-light-16.svg", "06-icons/menubar/elisa-icon-menubar-light-16.svg"]

    /// the kit's 16 px menu-bar SVG as a template image, 16 x 16 pt
    static func icon(_ kit: Kit?) -> NSImage? {
        for rel in iconPaths {
            guard let s = kit?.text(rel), let img = NSImage(data: Data(s.utf8)) else { continue }
            img.size = NSSize(width: 16, height: 16)
            img.isTemplate = true
            img.accessibilityDescription = "Elisa"
            return img
        }
        return nil
    }

    func install(model: AppModel, kit: Kit?) {
        guard item == nil else { return }
        m = model
        let it = NSStatusBar.system.statusItem(withLength: NSStatusItem.squareLength)
        if let img = MenuBarItem.icon(kit) { it.button?.image = img } else { it.button?.title = "E" }
        it.button?.toolTip = "Elisa"
        let menu = NSMenu()
        menu.delegate = self
        menu.autoenablesItems = false
        let show = NSMenuItem(title: "Show Elisa", action: #selector(showWindow), keyEquivalent: "")
        show.target = self
        menu.addItem(show)
        let conns = NSMenuItem(title: "Model Connections…", action: #selector(connections), keyEquivalent: "")
        conns.target = self
        menu.addItem(conns)
        menu.addItem(.separator())
        stateItem.isEnabled = false
        menu.addItem(stateItem)
        interruptItem.target = self
        menu.addItem(interruptItem)
        menu.addItem(.separator())
        let quit = NSMenuItem(title: "Quit Elisa", action: #selector(quit), keyEquivalent: "q")
        quit.target = self
        menu.addItem(quit)
        it.menu = menu
        item = it
        refresh()
    }

    func menuNeedsUpdate(_ menu: NSMenu) { refresh() }

    /// "ready · ~/code/ferrous" / "thinking… · ~/code/ferrous"
    static func stateLine(_ m: AppModel) -> String {
        let s: String = switch m.phase {
        case .starting: "starting"
        case .idle: "ready"
        case .thinking: "thinking…"
        case .streaming: "streaming…"
        case .tool: "running a tool…"
        case .permission: "waiting for permission"
        case .exited: "not running"
        }
        let w = m.title.isEmpty ? "" : " · \(m.title)"
        return "Elisa: \(s)\(w)"
    }

    private func refresh() {
        guard let m else { return }
        stateItem.title = MenuBarItem.stateLine(m)
        interruptItem.isEnabled = m.busy || m.approval != nil
    }

    @objc private func showWindow() {
        // the person clicked "Show Elisa" in the menu-bar item: to the front (Front.user)
        if let w = NSApp.windows.first(where: { $0.title == "Elisa" && $0.contentView != nil }) ?? NSApp.windows.first(where: { $0.canBecomeMain }) {
            if w.isMiniaturized { w.deminiaturize(nil) }
            Front.user(w)
        }
    }

    @objc private func connections() { if let m { ConnectionsWindow.shared.show(m) } }
    @objc private func interrupt() { m?.interrupt() }
    @objc private func quit() { NSApp.terminate(nil) }
}
