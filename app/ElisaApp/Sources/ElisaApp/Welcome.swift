// The welcome screen: what the window shows before a project folder is open (App.swift: launched without --cwd or
// --demo). Shade says hello, a few words on what Elisa is, then the two things a first session needs: a model to talk
// to (Model Connections, on this machine or online) and a folder to work in (recent folders, or the folder picker,
// which can make a new one). Opening a folder or the demo session leaves it for the usual transcript.
import AppKit
import SwiftUI

struct WelcomeView: View {
    @EnvironmentObject var m: AppModel
    @Environment(\.pal) var p
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @StateObject private var conns = ConnectionStore()
    @State private var shown = false
    @State private var greetSince = Date()
    @State private var greeting = true
    /// with a model connected, the two setup guides fold away behind "add another model"
    @State private var guides = false
    /// the local model list (LocalModels.swift) is open under the guides
    @State private var picker = false
    @StateObject private var hub = OllamaHub()
    @ObservedObject private var evals = EvalStore.shared

    var body: some View {
        VStack(spacing: 0) {
            topBar
            ScrollView {
                VStack(spacing: 28) {
                    hero
                    VStack(spacing: 14) {
                        stepModel
                        stepFolder
                    }
                    footer
                }
                .frame(maxWidth: 760)
                .padding(.horizontal, 28)
                .padding(.top, 18).padding(.bottom, 36)
                .frame(maxWidth: .infinity)
                .opacity(shown ? 1 : 0)
                .offset(y: shown ? 0 : 10)
            }
        }
        .background(p.bg)
        .onAppear {
            conns.load()
            evals.loadOnce()
            picker = true
            // --welcome-guides (debug): the guides and the local model list open, as after "Add another model"
            if CommandLine.arguments.contains("--welcome-guides") { guides = true }
            withAnimation(reduceMotion ? nil : .easeOut(duration: 0.6)) { shown = true }
            // Shade waves hello once, then settles
            DispatchQueue.main.asyncAfter(deadline: .now() + max(1.6, ShadeKit.shared.dur("greeting") / 1000 * 2)) {
                greeting = false; greetSince = Date()
            }
        }
        // back from Model Connections (it saved, or not): show what is active now
        .onReceive(NotificationCenter.default.publisher(for: NSWindow.didBecomeKeyNotification)) { n in
            if (n.object as? NSWindow)?.title == "Elisa" { conns.load() }
        }
    }

    // MARK: - parts

    private var topBar: some View {
        HStack {
            Spacer()
            Button(action: { m.toggleTheme() }) {
                Text(m.dark ? "dark ◐" : "light ◐").font(Fonts.mono(11.5)).foregroundStyle(p.fg2)
                    .padding(.horizontal, 8).padding(.vertical, 3)
                    .overlay(RoundedRectangle(cornerRadius: 5).strokeBorder(p.sep, lineWidth: 1))
            }
            .buttonStyle(.plain).help("Toggle the theme (⌘⇧L)")
        }
        .padding(.horizontal, 14).frame(height: 38)
    }

    private var hero: some View {
        VStack(spacing: 12) {
            ShadeImage(state: greeting ? "greeting" : "idle", since: greetSince, size: 112, animate: !reduceMotion)
                .frame(width: 112, height: 112)
            Wordmark(height: 30)
            Text("A coding agent that lives on your Mac and runs on your models.")
                .font(Fonts.mono(15, .semibold)).foregroundStyle(p.tx).multilineTextAlignment(.center)
            Text("Elisa reads your code, edits it, runs your checks and tells you what it did. Reading is free; writing, editing and running commands ask you first. With a local model, nothing leaves this Mac.")
                .font(Fonts.mono(12.5)).foregroundStyle(p.fg2).multilineTextAlignment(.center).lineSpacing(3)
                .frame(maxWidth: 600)
            HStack(spacing: 8) {
                pill("reads & edits code")
                pill("asks before changes")
                pill("checks before done")
                pill("images & web search")
            }
            .padding(.top, 2)
        }
    }

    private func pill(_ t: String) -> some View {
        Text(t).font(Fonts.mono(11)).foregroundStyle(p.fg2)
            .padding(.horizontal, 9).padding(.vertical, 3)
            .background(Capsule().fill(p.bg2))
            .overlay(Capsule().strokeBorder(p.sep, lineWidth: 1))
            .fixedSize()
    }

    /// step 1: the model Elisa talks to, with the active one when there is one
    private var stepModel: some View {
        let active = conns.items.first { $0.name == conns.active }
        return card(n: 1, title: "Connect a model", done: active != nil) {
            VStack(alignment: .leading, spacing: 12) {
                if let a = active {
                    HStack(spacing: 8) {
                        Circle().fill(p.ok).frame(width: 7, height: 7)
                        Text(a.model.isEmpty ? a.name : a.model).font(Fonts.mono(13, .semibold)).foregroundStyle(p.fg).fixedSize()
                        Text(a.leaves.map { "· sends to \($0)" } ?? "· stays on this machine").font(Fonts.mono(12)).foregroundStyle(p.fg2).lineLimit(1)
                        if let score = evals.scoreLine(a.model) {
                            Button { ScorecardWindow.shared.show(m) } label: { Text(verbatim: "· \(score)").font(Fonts.mono(12)).foregroundStyle(p.code).lineLimit(1).fixedSize() }
                                .buttonStyle(.plain).help("Measured on this Mac by Elisa's test cases · open the scorecard")
                        }
                        Spacer()
                        WelcomeButton(title: guides ? "Hide guides" : "Add another model") {
                            withAnimation(reduceMotion ? nil : .spring(response: 0.4, dampingFraction: 0.85)) { guides.toggle() }
                        }
                        .fixedSize()
                        WelcomeButton(title: "Change…") { ConnectionsWindow.shared.show(m) }.fixedSize()
                    }
                } else {
                    Text("Elisa needs a model to think with. Pick one of these; you can add more and switch any time from Elisa › Model Connections (⌘,).")
                        .font(Fonts.mono(12)).foregroundStyle(p.fg2).lineSpacing(2).fixedSize(horizontal: false, vertical: true)
                }
                if active == nil || guides {
                HStack(alignment: .top, spacing: 12) {
                    choice(icon: "desktopcomputer", title: "On this machine", sub: "Ollama · LM Studio · llama.cpp · MLX",
                           lines: ["Start your model server (e.g. open Ollama).",
                                   "Choose a model below: download it, or use one you have.",
                                   "Elisa saves the connection for you."],
                           note: "Private and free: nothing leaves your Mac.", button: picker ? "Hide the model list" : "Choose a local model", preset: "ollama")
                    choice(icon: "globe", title: "Online", sub: "Claude · OpenAI · OpenRouter · any compatible API",
                           lines: ["Get an API key from the provider.",
                                   "Add connection › pick the provider.",
                                   "Paste the key (or name its environment variable), then Test."],
                           note: "Your conversation goes to that provider.", button: "Connect an API", preset: "claude")
                }
                .transition(.opacity.combined(with: .move(edge: .top)))
                }
                if picker && (active == nil || guides) {
                    LocalModelPicker(hub: hub, current: active?.leaves == nil ? active?.model ?? "" : "", use: useLocal,
                                     otherServer: { ConnectionsWindow.shared.show(m, openAdd: "lm-studio") }, conns: conns.items)
                        .transition(.opacity.combined(with: .move(edge: .top)))
                }
            }
        }
    }

    /// Use on a local model: the saved Ollama connection gets this model (one is added when there is none), made active
    private func useLocal(_ name: String) {
        if let i = conns.items.firstIndex(where: { $0.type == "openai" && $0.baseURL.contains(":11434") }) {
            conns.items[i].model = name
            conns.active = conns.items[i].name
        } else if let pr = Connection.localPresets.first(where: { $0.name == "ollama" }) {
            conns.add(pr)
            conns.items[conns.items.count - 1].model = name
            conns.active = conns.items[conns.items.count - 1].name
        }
        conns.changed()
        conns.save { withAnimation { guides = false; picker = false } }
    }

    private func choice(icon: String, title: String, sub: String, lines: [String], note: String, button: String, preset: String) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(spacing: 8) {
                Image(systemName: icon).font(.system(size: 13, weight: .medium)).foregroundStyle(p.acc)
                Text(title).font(Fonts.mono(13, .semibold)).foregroundStyle(p.fg)
            }
            Text(sub).font(Fonts.mono(11)).foregroundStyle(p.fg2)
            VStack(alignment: .leading, spacing: 4) {
                ForEach(lines.indices, id: \.self) { i in
                    HStack(alignment: .firstTextBaseline, spacing: 6) {
                        Text("\(i + 1).").font(Fonts.mono(11.5)).foregroundStyle(p.gut)
                        Text(lines[i]).font(Fonts.mono(11.5)).foregroundStyle(p.tx).fixedSize(horizontal: false, vertical: true)
                    }
                }
            }
            .padding(.top, 2)
            Text(note).font(Fonts.mono(11)).foregroundStyle(p.fg2).italic()
            Spacer(minLength: 0)
            WelcomeButton(title: button) {
                if preset == "ollama" { withAnimation(reduceMotion ? nil : .spring(response: 0.4, dampingFraction: 0.85)) { picker.toggle() } }
                else { ConnectionsWindow.shared.show(m, openAdd: preset) }
            }
        }
        .padding(14)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(RoundedRectangle(cornerRadius: 8).fill(p.bg))
        .overlay(RoundedRectangle(cornerRadius: 8).strokeBorder(p.sep, lineWidth: 1))
    }

    /// step 2: the folder Elisa works in
    private var stepFolder: some View {
        let recent = m.recentFolders
        return card(n: 2, title: "Choose a project folder", done: false) {
            VStack(alignment: .leading, spacing: 12) {
                Text("Elisa works inside one folder: it can read there freely and asks before it touches anything outside it. Start a new project with New Folder in the picker.")
                    .font(Fonts.mono(12)).foregroundStyle(p.fg2).lineSpacing(2).fixedSize(horizontal: false, vertical: true)
                if !recent.isEmpty {
                    VStack(alignment: .leading, spacing: 2) {
                        Pixel("Recent", size: 9, color: p.fg2).padding(.bottom, 4)
                        ForEach(recent, id: \.self) { f in RecentRow(path: f) { m.open(folder: f) } }
                    }
                }
                HStack(spacing: 10) {
                    WelcomeButton(title: "Open a folder…", primary: true) { openFolderPanel(m) }
                        .keyboardShortcut(.defaultAction)
                    Text("⌘O").font(Fonts.mono(11)).foregroundStyle(p.gut)
                    Spacer()
                    WelcomeButton(title: "Try the demo (no model)") { m.demo = true; m.folder = nil; m.start() }
                }
            }
        }
    }

    private var footer: some View {
        Text("Tip: in a session, type / for commands · ⌃O shows the edits · Esc interrupts")
            .font(Fonts.mono(11)).foregroundStyle(p.gut)
    }

    private func card<C: View>(n: Int, title: String, done: Bool, @ViewBuilder _ content: () -> C) -> some View {
        HStack(alignment: .top, spacing: 14) {
            ZStack {
                Circle().fill(done ? p.ok.opacity(0.18) : p.sel)
                if done { Image(systemName: "checkmark").font(.system(size: 11, weight: .bold)).foregroundStyle(p.ok) }
                else { Text("\(n)").font(Fonts.mono(12, .semibold)).foregroundStyle(p.acc) }
            }
            .frame(width: 26, height: 26)
            VStack(alignment: .leading, spacing: 10) {
                Text(title).font(Fonts.mono(14, .semibold)).foregroundStyle(p.fg).padding(.top, 3)
                content()
            }
        }
        .padding(18)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(RoundedRectangle(cornerRadius: 10).fill(p.bg2))
        .overlay(RoundedRectangle(cornerRadius: 10).strokeBorder(p.sep, lineWidth: 1))
    }
}

private struct RecentRow: View {
    @Environment(\.pal) var p
    let path: String
    let action: () -> Void
    @State private var hover = false
    var body: some View {
        Button(action: action) {
            HStack(spacing: 8) {
                Image(systemName: "folder").font(.system(size: 12)).foregroundStyle(hover ? p.acc : p.fg2)
                Text((path as NSString).lastPathComponent).font(Fonts.mono(12.5, .semibold)).foregroundStyle(p.fg)
                Text((path as NSString).abbreviatingWithTildeInPath).font(Fonts.mono(11)).foregroundStyle(p.fg2)
                    .lineLimit(1).truncationMode(.head)
                Spacer()
                if hover { Text("open ›").font(Fonts.mono(11)).foregroundStyle(p.acc) }
            }
            .padding(.horizontal, 8).padding(.vertical, 5)
            .background(RoundedRectangle(cornerRadius: 5).fill(hover ? p.sel : .clear))
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .onHover { hover = $0 }
    }
}

private struct WelcomeButton: View {
    @Environment(\.pal) var p
    let title: String
    var primary = false
    let action: () -> Void
    @State private var hover = false
    var body: some View {
        Button(action: action) {
            Text(verbatim: title).font(Fonts.mono(12.5, primary ? .semibold : .regular))
                .foregroundStyle(primary ? p.bg : (hover ? p.fg : p.tx))
                .padding(.horizontal, primary ? 16 : 12).padding(.vertical, primary ? 7 : 5)
                .background(RoundedRectangle(cornerRadius: 6).fill(primary ? p.acc.opacity(hover ? 0.88 : 1) : (hover ? p.sel : .clear)))
                .overlay(RoundedRectangle(cornerRadius: 6).strokeBorder(primary ? .clear : (hover ? p.acc : p.sep), lineWidth: 1))
                .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .onHover { hover = $0 }
        .animation(.easeOut(duration: 0.12), value: hover)
    }
}
