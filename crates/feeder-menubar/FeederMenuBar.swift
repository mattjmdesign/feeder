import Cocoa
import Darwin
import SQLite3

struct ProjectRow {
    let id: Int64
    let slug: String
    let name: String
    let unseen: Bool
    let needsYou: Bool
    let pulseStatus: String?
    let pulseAgent: String?
    let lastAgent: String?
    let lastAt: Int64?
}

final class MenuBarController: NSObject, NSApplicationDelegate, NSMenuDelegate {
    private var statusItem: NSStatusItem!
    private var menu: NSMenu!
    private var parentPid: pid_t?
    private var lastDataVersion: Int64 = -1
    private var lastPulseBucket: Int64 = -1
    private var cachedRows: [ProjectRow] = []

    init(parentPid: pid_t?) {
        self.parentPid = parentPid
        super.init()
    }

    func applicationDidFinishLaunching(_ notification: Notification) {
        statusItem = NSStatusBar.system.statusItem(withLength: NSStatusItem.variableLength)
        menu = NSMenu()
        menu.delegate = self
        statusItem.menu = menu

        if let btn = statusItem.button {
            if let img = NSImage(systemSymbolName: "circle.grid.2x2.fill", accessibilityDescription: "Feeder") {
                img.isTemplate = true
                btn.image = img
                btn.imagePosition = .imageLeading
            } else {
                btn.title = "Feeder"
            }
        }

        refresh(force: true)

        Timer.scheduledTimer(withTimeInterval: 2.0, repeats: true) { [weak self] _ in
            guard let self else { return }
            if let pid = self.parentPid {
                if kill(pid, 0) != 0 && errno == ESRCH {
                    NSApp.terminate(nil)
                    return
                }
            }
            self.refresh(force: false)
        }
    }

    func menuWillOpen(_ menu: NSMenu) {
        refresh(force: true)
        rebuildMenu()
    }

    private func dbPath() -> String {
        let env = ProcessInfo.processInfo.environment
        if let custom = env["FEEDER_HOME"], !custom.isEmpty {
            return (custom as NSString).appendingPathComponent("feeder.db")
        }
        let appSupport = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask).first!
        return appSupport.appendingPathComponent("feeder/feeder.db").path
    }

    private func nowMs() -> Int64 {
        Int64(Date().timeIntervalSince1970 * 1000.0)
    }

    private func readSqlString(_ stmt: OpaquePointer?, _ col: Int32) -> String? {
        guard sqlite3_column_type(stmt, col) != SQLITE_NULL,
              let cstr = sqlite3_column_text(stmt, col) else { return nil }
        return String(cString: cstr)
    }

    private func readSqlInt64(_ stmt: OpaquePointer?, _ col: Int32) -> Int64? {
        guard sqlite3_column_type(stmt, col) != SQLITE_NULL else { return nil }
        return sqlite3_column_int64(stmt, col)
    }

    private func parseAttention(_ body: String?) -> Bool {
        guard let body else { return false }
        for block in body.components(separatedBy: "\n\n") {
            for line in block.components(separatedBy: .newlines) {
                var clean = line.trimmingCharacters(in: .whitespaces)
                while clean.hasPrefix("-") || clean.hasPrefix("*") || clean.hasPrefix(" ") {
                    clean.removeFirst()
                }
                while clean.hasPrefix("**") {
                    clean.removeFirst(2)
                }
                let lower = clean.lowercased()
                if lower.hasPrefix("decision needed:")
                    || lower.hasPrefix("decision needed**")
                    || lower.hasPrefix("decision:")
                    || lower.hasPrefix("decision**")
                    || lower.hasPrefix("blocked:")
                    || lower.hasPrefix("blocked**")
                {
                    return true
                }
            }
        }
        return false
    }

    private func refresh(force: Bool) {
        let path = dbPath()
        guard FileManager.default.fileExists(atPath: path) else {
            cachedRows = []
            updateBadge()
            return
        }

        var db: OpaquePointer?
        if sqlite3_open_v2(path, &db, SQLITE_OPEN_READONLY | SQLITE_OPEN_NOMUTEX, nil) != SQLITE_OK {
            if db != nil { sqlite3_close(db) }
            return
        }
        defer { sqlite3_close(db) }
        sqlite3_busy_timeout(db, 500)

        var version: Int64 = 0
        var vstmt: OpaquePointer?
        if sqlite3_prepare_v2(db, "PRAGMA data_version", -1, &vstmt, nil) == SQLITE_OK {
            if sqlite3_step(vstmt) == SQLITE_ROW {
                version = sqlite3_column_int64(vstmt, 0)
            }
        }
        sqlite3_finalize(vstmt)

        let now = nowMs()
        let pulseBucket = now / 10_000
        if !force && version == lastDataVersion && pulseBucket == lastPulseBucket {
            return
        }
        lastDataVersion = version
        lastPulseBucket = pulseBucket

        let sql = """
        SELECT
            p.id,
            p.slug,
            p.name,
            p.seen_at,
            (SELECT u.created_at FROM updates u WHERE u.project_id = p.id ORDER BY u.created_at DESC, u.id DESC LIMIT 1) AS last_at,
            (SELECT u.agent FROM updates u WHERE u.project_id = p.id ORDER BY u.created_at DESC, u.id DESC LIMIT 1) AS last_agent,
            (SELECT u.body FROM updates u WHERE u.project_id = p.id ORDER BY u.created_at DESC, u.id DESC LIMIT 1) AS last_body,
            (SELECT COUNT(*) FROM replies r
              WHERE r.project_id = p.id AND r.from_human = 1
                AND r.created_at >= COALESCE(
                    (SELECT u.created_at FROM updates u WHERE u.project_id = p.id ORDER BY u.created_at DESC, u.id DESC LIMIT 1),
                    0
                )) AS last_answered,
            (SELECT pu.status FROM pulses pu WHERE pu.project_id = p.id AND pu.expires_at > ?1 ORDER BY pu.updated_at DESC LIMIT 1) AS pulse_status,
            (SELECT pu.agent FROM pulses pu WHERE pu.project_id = p.id AND pu.expires_at > ?1 ORDER BY pu.updated_at DESC LIMIT 1) AS pulse_agent
        FROM projects p
        WHERE p.archived = 0
        ORDER BY p.pinned DESC, COALESCE(last_at, p.created_at) DESC
        """

        var stmt: OpaquePointer?
        var rows: [ProjectRow] = []
        if sqlite3_prepare_v2(db, sql, -1, &stmt, nil) == SQLITE_OK {
            sqlite3_bind_int64(stmt, 1, now)
            while sqlite3_step(stmt) == SQLITE_ROW {
                let id = sqlite3_column_int64(stmt, 0)
                let slug = readSqlString(stmt, 1) ?? ""
                let name = readSqlString(stmt, 2) ?? slug
                let seenAt = readSqlInt64(stmt, 3)
                let lastAt = readSqlInt64(stmt, 4)
                let lastAgent = readSqlString(stmt, 5)
                let lastBody = readSqlString(stmt, 6)
                let lastAnswered = sqlite3_column_int64(stmt, 7)
                let pulseStatus = readSqlString(stmt, 8)
                let pulseAgent = readSqlString(stmt, 9)

                let unseen: Bool
                if let lastAt {
                    unseen = seenAt == nil || lastAt > seenAt!
                } else {
                    unseen = false
                }
                let needsYou = lastAnswered == 0 && parseAttention(lastBody)
                rows.append(ProjectRow(
                    id: id,
                    slug: slug,
                    name: name,
                    unseen: unseen,
                    needsYou: needsYou,
                    pulseStatus: pulseStatus,
                    pulseAgent: pulseAgent,
                    lastAgent: lastAgent,
                    lastAt: lastAt
                ))
            }
        }
        sqlite3_finalize(stmt)
        cachedRows = rows
        updateBadge()
    }

    private func updateBadge() {
        guard let btn = statusItem.button else { return }
        let needsCount = cachedRows.filter { $0.needsYou }.count
        let liveCount = cachedRows.filter { $0.pulseStatus != nil }.count
        let unseenCount = cachedRows.filter { $0.unseen }.count

        if needsCount > 0 {
            btn.title = " !\(needsCount)"
        } else if liveCount > 0 {
            btn.title = " ● \(liveCount)"
        } else if unseenCount > 0 {
            btn.title = " \(unseenCount)"
        } else {
            btn.title = ""
        }
    }

    private func rebuildMenu() {
        menu.removeAllItems()

        let working = cachedRows.filter { $0.pulseStatus != nil }
        if !working.isEmpty {
            let hdr = NSMenuItem(title: "Working Now", action: nil, keyEquivalent: "")
            hdr.isEnabled = false
            menu.addItem(hdr)
            for p in working {
                let status = p.pulseStatus ?? "Working"
                let agent = p.pulseAgent.map { " (\($0))" } ?? ""
                let item = NSMenuItem(title: "● \(p.name) · \(status)\(agent)", action: #selector(openProjectItem(_:)), keyEquivalent: "")
                item.target = self
                item.representedObject = p.slug
                menu.addItem(item)
            }
            menu.addItem(.separator())
        }

        let attention = cachedRows.filter { $0.needsYou }
        if !attention.isEmpty {
            let hdr = NSMenuItem(title: "Needs You", action: nil, keyEquivalent: "")
            hdr.isEnabled = false
            menu.addItem(hdr)
            for p in attention {
                let item = NSMenuItem(title: "◆ \(p.name)", action: #selector(openProjectItem(_:)), keyEquivalent: "")
                item.target = self
                item.representedObject = p.slug
                menu.addItem(item)
            }
            menu.addItem(.separator())
        }

        if cachedRows.isEmpty {
            let empty = NSMenuItem(title: "No active projects", action: nil, keyEquivalent: "")
            empty.isEnabled = false
            menu.addItem(empty)
        } else {
            let hdr = NSMenuItem(title: "Projects", action: nil, keyEquivalent: "")
            hdr.isEnabled = false
            menu.addItem(hdr)
            for p in cachedRows.prefix(10) {
                let prefix = p.unseen ? "• " : "  "
                let agentSuffix = p.lastAgent.map { " · \($0)" } ?? ""
                let item = NSMenuItem(title: "\(prefix)\(p.name)\(agentSuffix)", action: #selector(openProjectItem(_:)), keyEquivalent: "")
                item.target = self
                item.representedObject = p.slug
                menu.addItem(item)
            }
        }

        menu.addItem(.separator())

        let unseenCount = cachedRows.filter { $0.unseen }.count
        if unseenCount > 0 {
            let markRead = NSMenuItem(title: "Mark All Read (\(unseenCount))", action: #selector(markAllRead), keyEquivalent: "")
            markRead.target = self
            menu.addItem(markRead)
        }

        let openHome = NSMenuItem(title: "Open Feeder", action: #selector(openFeederHome), keyEquivalent: "")
        openHome.target = self
        menu.addItem(openHome)

        let openFeed = NSMenuItem(title: "Open Feed", action: #selector(openFeederFeed), keyEquivalent: "")
        openFeed.target = self
        menu.addItem(openFeed)

    }

    private func openUrl(_ raw: String) {
        guard let url = URL(string: raw) else { return }
        NSWorkspace.shared.open(url)
    }

    @objc private func openProjectItem(_ sender: NSMenuItem) {
        guard let slug = sender.representedObject as? String, !slug.isEmpty else { return }
        openUrl("feeder://project/\(slug)")
    }

    @objc private func openFeederHome() { openUrl("feeder://home") }
    @objc private func openFeederFeed() { openUrl("feeder://feed") }

    @objc private func markAllRead() {
        var candidates: [String] = []
        if let exe = Bundle.main.executableURL {
            let bundled = exe
                .deletingLastPathComponent()
                .appendingPathComponent("feeder")
                .path
            candidates.append(bundled)
        }
        let homeBin = FileManager.default.homeDirectoryForCurrentUser.appendingPathComponent(".cargo/bin/feeder").path
        candidates.append(homeBin)

        guard let bin = candidates.first(where: { FileManager.default.isExecutableFile(atPath: $0) }) else { return }
        let task = Process()
        task.executableURL = URL(fileURLWithPath: bin)
        task.arguments = ["seen", "--all"]
        try? task.run()
        task.waitUntilExit()
        refresh(force: true)
    }
}

var parentPid: pid_t?
let args = CommandLine.arguments
if let idx = args.firstIndex(of: "--parent-pid"), idx + 1 < args.count, let parsed = Int32(args[idx + 1]) {
    parentPid = parsed
}

let app = NSApplication.shared
app.setActivationPolicy(.accessory)
let controller = MenuBarController(parentPid: parentPid)
app.delegate = controller
app.run()
