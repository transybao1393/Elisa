// What Elisa is doing in the background, in plain words (`activity` events, elisa/src/activity.rs): one live line under
// the last message while a turn runs ("Searching the web for “Apple news”…"), and, when a reply is done, one quiet
// line ("Searched the web · read 2 pages · 3.1 s") that opens to its steps and the detailed tool rows.
//
// App-side work uses the same line: `m.showLocalActivity("Transcribing your voice…")` … `m.clearLocalActivity()`.
import SwiftUI

/// one finished step of a reply (`activity` phase "done")
struct ActivityItem: Identifiable {
    let id: Int
    var kind: String
    var text: String
    var summary = ""
    var ms: Int?
    var error = false
}

/// the step on the live line (`activity` phase "start" / "update")
struct LiveActivity: Equatable {
    var id: Int
    var kind: String
    var text: String
    var since = Date()
    /// an approval for this step is open
    var waitingOk = false
}

extension AppModel {
    /// Show app-side work on the live line (it wins over Elisa's own steps while set), e.g. "Transcribing your voice…".
    func showLocalActivity(_ text: String) { localAct = LiveActivity(id: -1, kind: "local", text: text); scrollTick &+= 1 }
    /// Take the app-side line away again.
    func clearLocalActivity() { localAct = nil }

    /// what the live line shows now (nil = nothing)
    var liveLine: LiveActivity? {
        if let l = localAct { return l }
        guard busy || phase == .permission else { return nil }
        return liveAct
    }

    /// one `activity` event
    func activityEvent(_ e: [String: Any]) {
        // the check of a file just attached (`prefetch`, before Enter) belongs to no reply: its chip shows the result
        // (`doc_info`); taken as a step, its "done" opened an empty ELISA reply block that nothing ever filled
        if (e["background"] as? Bool) == true { return }
        activitySeen = true
        let id = (e["id"] as? NSNumber)?.intValue ?? 0
        let kind = e["kind"] as? String ?? ""
        let text = e["text"] as? String ?? ""
        switch e["phase"] as? String ?? "" {
        case "start":
            let a = LiveActivity(id: id, kind: kind, text: text)
            if kind == "model" { nextAct = nil; liveAct = a; return }
            // a quick step (under 300 ms) never takes the line: it shows only once it has run that long
            nextAct = a
            DispatchQueue.main.asyncAfter(deadline: .now() + 0.3) { [weak self] in
                guard let self, let n = self.nextAct, n.id == id else { return }
                self.liveAct = n
                self.nextAct = nil
            }
        case "update":
            let waiting = (e["waiting_ok"] as? Bool) ?? false
            if nextAct?.id == id && !waiting { nextAct?.text = text; return }
            let since = liveAct?.id == id && kind != "model" ? liveAct?.since ?? Date() : Date()
            if nextAct?.id == id { nextAct = nil }
            liveAct = LiveActivity(id: id, kind: kind, text: text, since: since, waitingOk: waiting)
        case "done":
            if nextAct?.id == id { nextAct = nil }
            if liveAct?.id == id { liveAct?.waitingOk = false }
            let r = currentReply()
            blocks[r].acts.append(ActivityItem(id: id, kind: kind, text: text, summary: e["summary"] as? String ?? "",
                                               ms: (e["ms"] as? NSNumber)?.intValue, error: (e["error"] as? Bool) ?? false))
        case "summary":
            if let r = currentBlock { blocks[r].actSummary = text }
        default:
            break
        }
    }

    /// the turn ended: the live line goes; a reply whose summary never came (an interrupted turn) gets a plain one
    func activityTurnDone() {
        liveAct = nil; nextAct = nil
        if let r = currentBlock, !blocks[r].acts.isEmpty, blocks[r].actSummary == nil {
            let n = blocks[r].acts.count
            blocks[r].actSummary = "\(n) step\(n == 1 ? "" : "s")"
        }
    }
}

/// the live line under the last message: a soft pulse and what Elisa is doing; a long wait shows its seconds
struct LiveActivityLine: View {
    @Environment(\.pal) var p
    let a: LiveActivity
    var body: some View {
        TimelineView(.periodic(from: .now, by: 0.5)) { tl in
            let secs = tl.date.timeIntervalSince(a.since)
            HStack(spacing: 8) {
                Circle().fill(a.waitingOk ? p.warn : p.acc).frame(width: 6, height: 6)
                    .opacity(0.45 + 0.55 * abs(sin(tl.date.timeIntervalSinceReferenceDate * 2.2)))
                Text(a.text).foregroundStyle(a.waitingOk ? p.warn : p.fg2)
                if secs >= 3 && !a.text.hasPrefix("Writing") {
                    Text(dur(secs, 0)).foregroundStyle(p.fg2.opacity(0.6)).monospacedDigit()
                }
            }
            .font(Fonts.mono(12))
            .lineLimit(1)
        }
        .padding(.leading, 36)
        .transition(.opacity)
        .accessibilityElement(children: .combine)
        .accessibilityLabel(a.text)
    }
}

/// a finished reply's steps: one quiet line that opens to each step and, under them, the detailed tool rows
struct ActivitySummaryView: View {
    @EnvironmentObject var m: AppModel
    @Environment(\.pal) var p
    let b: Block

    var body: some View {
        let open = m.expanded.contains(b.id)
        VStack(alignment: .leading, spacing: 6) {
            Button {
                withAnimation(.spring(response: 0.34, dampingFraction: 0.88)) { m.toggleExpand(b.id) }
            } label: {
                HStack(spacing: 6) {
                    Image(systemName: "chevron.right").font(.system(size: 9, weight: .semibold)).rotationEffect(.degrees(open ? 90 : 0))
                    Text(b.actSummary ?? "")
                    if b.acts.contains(where: \.error) { Text("· something didn't work").foregroundStyle(p.warn) }
                }
                .font(Fonts.mono(11.5)).foregroundStyle(p.fg2)
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .help(open ? "Hide the steps" : "Show what Elisa did")
            if open {
                VStack(alignment: .leading, spacing: 4) {
                    ForEach(b.acts) { a in
                        HStack(alignment: .firstTextBaseline, spacing: 6) {
                            Text(a.error ? "✗" : "✓").foregroundStyle(a.error ? p.err : p.ok)
                            Text(a.text).foregroundStyle(p.fg)
                            if !a.summary.isEmpty { Text("· \(a.summary)").foregroundStyle(p.fg2) }
                            if let ms = a.ms { Text("· \(dur(Double(ms) / 1000))").foregroundStyle(p.fg2.opacity(0.7)).monospacedDigit() }
                        }
                        .font(Fonts.mono(11.5))
                        .textSelection(.enabled)
                    }
                    let tools = b.parts.filter(\.isTool)
                    if !tools.isEmpty {
                        VStack(alignment: .leading, spacing: 8) {
                            ForEach(tools) { part in PartView(part: part) }
                        }
                        .padding(.top, 6)
                    }
                }
                .padding(.leading, 15)
                .transition(.opacity.combined(with: .move(edge: .top)))
            }
        }
        .frame(maxWidth: 860, alignment: .leading)
    }
}

extension Part {
    /// a detailed tool row (kept inside a reply's step list once Elisa reports steps in words)
    var isTool: Bool { if case .tool = kind { return true }; return false }
}
