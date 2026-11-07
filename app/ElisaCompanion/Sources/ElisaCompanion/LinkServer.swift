// The link to Elisa: a Unix domain socket (no network port). This app listens; each Elisa connects as a client.
//
// Protocol (protocol version 1, UTF-8 JSON, one object per line):
//   Elisa -> companion  {"v":1,"type":"hello","app":"elisa","pid":123}
//                       {"v":1,"type":"state","state":"editing","verb":"EDITING","target":"split.rs","tone":"accent",
//                        "pop":false,"theme":"dark"}
//                       {"v":1,"type":"bye"}
//   companion -> Elisa  {"v":1,"type":"care","action":"poke"|"water"|"food"|"toy"}
// The newest client to send a state line is the one shown and the one that gets care actions.
import Darwin
import Foundation

final class LinkServer {
    let path: String
    private var listenFd: Int32 = -1
    private var acceptSource: DispatchSourceRead?
    private var clients: [Int32: Client] = [:]
    /// the client whose state is shown
    private(set) var active: Int32?

    var onState: (([String: Any]) -> Void)?
    /// true when at least one Elisa is connected
    var onConnected: ((Bool) -> Void)?
    var log: (String) -> Void = { print($0) }

    private final class Client {
        let fd: Int32
        let source: DispatchSourceRead
        var buf = Data()
        var pid = 0
        init(fd: Int32, source: DispatchSourceRead) { self.fd = fd; self.source = source }
    }

    /// ELISA_COMPANION_SOCK, else ~/.elisa/companion.sock (the same rule as elisa/src/shade/link.rs)
    static func defaultPath() -> String {
        if let p = ProcessInfo.processInfo.environment["ELISA_COMPANION_SOCK"], !p.isEmpty { return p }
        return (NSHomeDirectory() as NSString).appendingPathComponent(".elisa/companion.sock")
    }

    init(path: String) { self.path = path }

    var connected: Bool { !clients.isEmpty }

    /// bind and listen. Fails if another companion already serves this path.
    func start() throws {
        let dir = (path as NSString).deletingLastPathComponent
        try FileManager.default.createDirectory(atPath: dir, withIntermediateDirectories: true)
        if FileManager.default.fileExists(atPath: path) {
            if LinkServer.probe(path) { throw KitError.missing("another companion is already listening on \(path)") }
            unlink(path) // a stale socket from a crash
        }
        let fd = socket(AF_UNIX, SOCK_STREAM, 0)
        guard fd >= 0 else { throw KitError.missing("socket(): \(String(cString: strerror(errno)))") }
        var addr = LinkServer.address(path)
        let ok = withUnsafePointer(to: &addr) {
            $0.withMemoryRebound(to: sockaddr.self, capacity: 1) { bind(fd, $0, socklen_t(MemoryLayout<sockaddr_un>.size)) }
        }
        guard ok == 0 else { close(fd); throw KitError.missing("bind \(path): \(String(cString: strerror(errno)))") }
        chmod(path, 0o600)
        guard listen(fd, 8) == 0 else { close(fd); throw KitError.missing("listen: \(String(cString: strerror(errno)))") }
        _ = fcntl(fd, F_SETFL, fcntl(fd, F_GETFL) | O_NONBLOCK)
        listenFd = fd
        let s = DispatchSource.makeReadSource(fileDescriptor: fd, queue: .main)
        s.setEventHandler { [weak self] in self?.acceptAll() }
        s.resume()
        acceptSource = s
        log("listening on \(path)")
    }

    func stop() {
        if let a = active { _ = write(line: ["v": 1, "type": "bye"], to: a) }
        for c in clients.values { c.source.cancel() }
        clients.removeAll()
        acceptSource?.cancel()
        if listenFd >= 0 { close(listenFd); listenFd = -1; unlink(path) }
    }

    /// a care action to the active Elisa; false = nobody connected (the caller plays it locally)
    @discardableResult
    func sendCare(_ action: String) -> Bool {
        guard let fd = active ?? clients.keys.max() else { return false }
        return write(line: ["v": 1, "type": "care", "action": action], to: fd)
    }

    // MARK: - internals

    private func acceptAll() {
        while true {
            let fd = accept(listenFd, nil, nil)
            if fd < 0 { return }
            _ = fcntl(fd, F_SETFL, fcntl(fd, F_GETFL) | O_NONBLOCK)
            var one: Int32 = 1
            setsockopt(fd, SOL_SOCKET, SO_NOSIGPIPE, &one, socklen_t(MemoryLayout<Int32>.size))
            let s = DispatchSource.makeReadSource(fileDescriptor: fd, queue: .main)
            let c = Client(fd: fd, source: s)
            s.setEventHandler { [weak self] in self?.readFrom(c) }
            s.setCancelHandler { close(fd) }
            clients[fd] = c
            s.resume()
            log("elisa connected (fd \(fd))")
            if clients.count == 1 { onConnected?(true) }
        }
    }

    private func readFrom(_ c: Client) {
        var tmp = [UInt8](repeating: 0, count: 8192)
        while true {
            let n = read(c.fd, &tmp, tmp.count)
            if n > 0 { c.buf.append(tmp, count: n); continue }
            if n == 0 || (errno != EAGAIN && errno != EWOULDBLOCK) { drop(c); return }
            break
        }
        // a client that never sends a newline cannot grow this without bound
        if c.buf.count > 1 << 20 { drop(c); return }
        while let i = c.buf.firstIndex(of: 0x0A) {
            let line = c.buf.subdata(in: c.buf.startIndex..<i)
            c.buf.removeSubrange(c.buf.startIndex...i)
            handle(line, from: c)
        }
    }

    private func handle(_ line: Data, from c: Client) {
        guard let j = try? JSONSerialization.jsonObject(with: line) as? [String: Any], let type = j["type"] as? String else { return }
        switch type {
        case "hello":
            c.pid = j["pid"] as? Int ?? 0
            log("hello from elisa pid \(c.pid)")
        case "state":
            active = c.fd
            onState?(j)
        case "bye":
            drop(c)
        default:
            break
        }
    }

    private func drop(_ c: Client) {
        guard clients.removeValue(forKey: c.fd) != nil else { return }
        c.source.cancel()
        if active == c.fd { active = nil }
        log("elisa disconnected (fd \(c.fd))")
        if clients.isEmpty { onConnected?(false) }
    }

    private func write(line: [String: Any], to fd: Int32) -> Bool {
        guard var d = try? JSONSerialization.data(withJSONObject: line, options: [.sortedKeys]) else { return false }
        d.append(0x0A)
        let n = d.withUnsafeBytes { Darwin.write(fd, $0.baseAddress, d.count) }
        return n == d.count
    }

    static func address(_ path: String) -> sockaddr_un {
        var addr = sockaddr_un()
        addr.sun_family = sa_family_t(AF_UNIX)
        let bytes = Array(path.utf8.prefix(MemoryLayout.size(ofValue: addr.sun_path) - 1))
        withUnsafeMutableBytes(of: &addr.sun_path) { p in
            for (i, b) in bytes.enumerated() { p[i] = b }
            p[bytes.count] = 0
        }
        return addr
    }

    /// is something accepting on `path`?
    static func probe(_ path: String) -> Bool {
        let fd = socket(AF_UNIX, SOCK_STREAM, 0)
        guard fd >= 0 else { return false }
        defer { close(fd) }
        var addr = address(path)
        return withUnsafePointer(to: &addr) {
            $0.withMemoryRebound(to: sockaddr.self, capacity: 1) { connect(fd, $0, socklen_t(MemoryLayout<sockaddr_un>.size)) }
        } == 0
    }
}
