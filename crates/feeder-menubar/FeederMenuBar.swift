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
    private var cachedRows: [ProjectRow] = []

    init(parentPid: pid_t?) {
        self.parentPid = parentPid
        super.init()
    }

    func applicationDidFinishLaunching(_ notification: Notification) {
        statusItem = NSStatusBar.system.statusItem(withLength: NSStatusItem.squareLength)
        menu = NSMenu()
        menu.delegate = self
        statusItem.menu = menu
        // Drawn before the first read, so the item never shows up blank when the database is
        // missing, locked or mid-migration.
        updateBadge()

        refresh()

        Timer.scheduledTimer(withTimeInterval: 2.0, repeats: true) { [weak self] _ in
            guard let self else { return }
            if let pid = self.parentPid {
                if kill(pid, 0) != 0 && errno == ESRCH {
                    NSApp.terminate(nil)
                    return
                }
            }
            self.refresh()
        }
    }

    func menuWillOpen(_ menu: NSMenu) {
        refresh()
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

    private func refresh() {
        let path = dbPath()
        guard FileManager.default.fileExists(atPath: path) else {
            cachedRows = []
            updateBadge()
            return
        }

        // A failed read keeps the last rows and icon rather than blanking the item.
        var db: OpaquePointer?
        if sqlite3_open_v2(path, &db, SQLITE_OPEN_READONLY | SQLITE_OPEN_NOMUTEX, nil) != SQLITE_OK {
            if db != nil { sqlite3_close(db) }
            return
        }
        defer { sqlite3_close(db) }
        sqlite3_busy_timeout(db, 500)

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
        guard sqlite3_prepare_v2(db, sql, -1, &stmt, nil) == SQLITE_OK else { return }
        defer { sqlite3_finalize(stmt) }
        sqlite3_bind_int64(stmt, 1, nowMs())
        var result = sqlite3_step(stmt)
        while result == SQLITE_ROW {
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
                unseen = seenAt.map { lastAt > $0 } ?? true
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
            result = sqlite3_step(stmt)
        }
        guard result == SQLITE_DONE else { return }
        cachedRows = rows
        updateBadge()
    }

    private func updateBadge() {
        guard let btn = statusItem.button else { return }
        let needsCount = cachedRows.filter { $0.needsYou }.count
        let liveCount = cachedRows.filter { $0.pulseStatus != nil }.count
        let unseenCount = cachedRows.filter { $0.unseen }.count

        let status = [
            needsCount > 0 ? "\(needsCount) waiting on you" : nil,
            liveCount > 0 ? "\(liveCount) working" : nil,
            unseenCount > 0 ? "\(unseenCount) unread" : nil,
        ].compactMap { $0 }
        let label = (["Feeder"] + status).joined(separator: " · ")
        btn.setAccessibilityLabel(label)
        btn.toolTip = label
        // Only a decision earns the dot: unread updates are almost always there, and a dot that
        // never clears stops meaning anything.
        btn.image = needsCount > 0 ? Self.attentionIcon : Self.idleIcon
        btn.imagePosition = .imageOnly
    }

    private static let idleIcon = statusIcon(attention: false)
    private static let attentionIcon = statusIcon(attention: true)

    /// Three marbles, after the app icon. With `attention`, a dot sits in the top-right corner,
    /// cut clear of the marbles so it reads at menu bar size.
    private static func statusIcon(attention: Bool) -> NSImage {
        let image = NSImage(size: NSSize(width: 18, height: 18), flipped: false) { _ in
            NSColor.black.setFill()
            NSBezierPath(ovalIn: NSRect(x: 1, y: 2, width: 10, height: 10)).fill()
            NSBezierPath(ovalIn: NSRect(x: 12, y: 2, width: 5, height: 5)).fill()
            NSBezierPath(ovalIn: NSRect(x: 5, y: 12.5, width: 4.5, height: 4.5)).fill()
            if attention {
                NSBezierPath(ovalIn: NSRect(x: 12, y: 11, width: 6, height: 6)).fill()
            }
            return true
        }
        image.isTemplate = true
        return image
    }

    private static func dot(_ color: NSColor?) -> NSImage {
        NSImage(size: NSSize(width: 8, height: 8), flipped: false) { rect in
            if let color {
                color.setFill()
                NSBezierPath(ovalIn: rect.insetBy(dx: 0.5, dy: 0.5)).fill()
            }
            return true
        }
    }

    private static let needsDot = dot(.systemOrange)
    private static let workingDot = dot(.systemGreen)
    private static let unreadDot = dot(.controlAccentColor)
    private static let noDot = dot(nil)

    private func header(_ title: String) -> NSMenuItem {
        if #available(macOS 14.0, *) {
            return NSMenuItem.sectionHeader(title: title)
        }
        let item = NSMenuItem(title: title, action: nil, keyEquivalent: "")
        item.isEnabled = false
        return item
    }

    /// A project row: the name, then a quieter note (what it's doing or who posted last).
    private func projectItem(_ p: ProjectRow, note: String?, dot: NSImage) -> NSMenuItem {
        let item = NSMenuItem(title: p.name, action: #selector(openProjectItem(_:)), keyEquivalent: "")
        if let note, !note.isEmpty {
            let title = NSMutableAttributedString(string: p.name, attributes: [.font: NSFont.menuFont(ofSize: 0)])
            title.append(NSAttributedString(string: "  \(note)", attributes: [
                .font: NSFont.menuFont(ofSize: 0),
                .foregroundColor: NSColor.secondaryLabelColor,
            ]))
            item.attributedTitle = title
        }
        item.image = dot
        item.target = self
        item.representedObject = p.slug
        return item
    }

    private func rebuildMenu() {
        menu.removeAllItems()

        if cachedRows.isEmpty {
            let empty = NSMenuItem(title: "No projects yet", action: nil, keyEquivalent: "")
            empty.isEnabled = false
            menu.addItem(empty)
            let hint = NSMenuItem(title: "Ask an agent to “post a Feeder update”", action: nil, keyEquivalent: "")
            hint.isEnabled = false
            menu.addItem(hint)
        } else {
            // Each project appears once, under the most pressing thing about it.
            let needs = cachedRows.filter { $0.needsYou }
            let working = cachedRows.filter { !$0.needsYou && $0.pulseStatus != nil }
            let rest = cachedRows.filter { !$0.needsYou && $0.pulseStatus == nil }

            if !needs.isEmpty {
                menu.addItem(header("Waiting on you"))
                for p in needs {
                    menu.addItem(projectItem(p, note: p.lastAgent, dot: Self.needsDot))
                }
            }
            if !working.isEmpty {
                menu.addItem(header("Working now"))
                for p in working {
                    let note = [p.pulseStatus, p.pulseAgent].compactMap { $0 }.joined(separator: " · ")
                    menu.addItem(projectItem(p, note: note, dot: Self.workingDot))
                }
            }
            let shown = max(0, 8 - needs.count - working.count)
            if shown > 0 && !rest.isEmpty {
                menu.addItem(header("Recent"))
                for p in rest.prefix(shown) {
                    menu.addItem(projectItem(p, note: p.lastAgent, dot: p.unseen ? Self.unreadDot : Self.noDot))
                }
            }
        }

        menu.addItem(.separator())

        let unseenCount = cachedRows.filter { $0.unseen }.count
        if unseenCount > 0 {
            let markRead = NSMenuItem(title: "Mark All Read", action: #selector(markAllRead), keyEquivalent: "")
            markRead.target = self
            menu.addItem(markRead)
        }

        for (title, action) in [
            ("Open Feeder", #selector(openFeederHome)),
            ("Open Feed", #selector(openFeederFeed)),
            ("Open Notebook", #selector(openFeederNotebook)),
        ] {
            let item = NSMenuItem(title: title, action: action, keyEquivalent: "")
            item.target = self
            menu.addItem(item)
        }
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
    @objc private func openFeederNotebook() { openUrl("feeder://notebook") }

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
        refresh()
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
