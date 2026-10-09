import Foundation

/// A role you're interviewing for (`ic list --json --all`).
public struct RoleSummary: Decodable, Identifiable, Hashable {
    public let id: Int
    public let title: String
    public let company: String?
    public let companyKey: String?
    public let status: String
    public let statusLabel: String
    public let archived: Bool
    /// Its place among its company's roles, once you've arranged them.
    public var position: Int? = nil
}

/// A company's place in the sidebar, once you've arranged it (`ic order companies`).
public struct CompanyPlace: Decodable, Hashable {
    public let key: String
    public var position: Int

    public init(key: String, position: Int) {
        self.key = key
        self.position = position
    }
}

/// Every interview (archived and deleted ones flagged) and every role but deleted ones.
public struct Library: Decodable, Equatable {
    public var sessions: [SessionSummary]
    public var roles: [RoleSummary]
    /// The companies you've arranged (absent from libraries listed before arranging existed).
    public var companyOrder: [CompanyPlace]?

    public init(sessions: [SessionSummary] = [], roles: [RoleSummary] = [], companyOrder: [CompanyPlace] = []) {
        self.sessions = sessions
        self.roles = roles
        self.companyOrder = companyOrder
    }

    public func role(_ id: Int?) -> RoleSummary? { id.flatMap { id in roles.first { $0.id == id } } }

    /// The company an interview shows under: its role's, else its own ("" for none).
    public func companyKey(of session: SessionSummary) -> String {
        role(session.roleId)?.companyKey ?? session.companyKey ?? ""
    }

    /// What deleting a company takes: every interview shown under it, archived ones too.
    public func interviews(atCompany key: String) -> [SessionSummary] {
        sessions.filter { !$0.isDeleted && companyKey(of: $0) == key }
    }

    /// What deleting a role takes: its rounds, archived ones too.
    public func interviews(inRole id: Int) -> [SessionSummary] {
        sessions.filter { !$0.isDeleted && $0.roleId == id }
    }

    /// Whether anything has been arranged by hand, so there's an arrangement to undo.
    public var isArranged: Bool {
        !(companyOrder ?? []).isEmpty || roles.contains { $0.position != nil } || sessions.contains { $0.position != nil }
    }

    /// Show these companies (by key) in this order, top first, as `ic order companies` saves it.
    public mutating func arrangeCompanies(_ keys: [String]) {
        var places = (companyOrder ?? []).filter { !keys.contains($0.key) }
        places += keys.enumerated().map { CompanyPlace(key: $1, position: $0) }
        companyOrder = places.sorted { ($0.position, $0.key) < ($1.position, $1.key) }
    }

    /// Show these roles in this order, top first, as `ic order roles` saves it.
    public mutating func arrangeRoles(_ ids: [Int]) {
        for (position, id) in ids.enumerated() {
            if let i = roles.firstIndex(where: { $0.id == id }) { roles[i].position = position }
        }
    }

    /// Show these interviews in this order, top first, as `ic order interviews` saves it.
    public mutating func arrangeInterviews(_ ids: [Int]) {
        for (position, id) in ids.enumerated() {
            if let i = sessions.firstIndex(where: { $0.id == id }) { sessions[i].position = position }
        }
    }

    /// Back to date order, as `ic order reset` leaves it.
    public mutating func resetArrangement() {
        companyOrder = []
        for i in roles.indices { roles[i].position = nil }
        for i in sessions.indices { sessions[i].position = nil }
    }
}

/// `items` with the ones at `offsets` moved to just before `destination`, the way a list's
/// drag-to-reorder (`onMove`) reports a move.
public func moving<T>(_ items: [T], fromOffsets offsets: IndexSet, toOffset destination: Int) -> [T] {
    var rest: [T] = []
    var insertAt = 0
    for (i, item) in items.enumerated() where !offsets.contains(i) {
        if i < destination { insertAt += 1 }
        rest.append(item)
    }
    rest.insert(contentsOf: offsets.filter { $0 < items.count }.map { items[$0] }, at: insertAt)
    return rest
}

/// `items` with `dragged` taking `target`'s place: just after it when dragged down, before it when dragged up.
public func moving<T: Equatable>(_ items: [T], _ dragged: T, onto target: T) -> [T] {
    guard dragged != target, let from = items.firstIndex(of: dragged), let to = items.firstIndex(of: target) else { return items }
    var result = items
    result.remove(at: from)
    result.insert(dragged, at: to)
    return result
}

/// Where an application stands, for the role status menu.
public let roleStatuses: [(id: String, label: String)] = [
    ("interviewing", "Interviewing"), ("offer", "Offer"), ("accepted", "Accepted"), ("rejected", "Rejected"),
    ("withdrawn", "Withdrawn"),
]

/// Interview rounds, for the Edit Details sheet (the same ids as ic's `--round`).
public let rounds: [(id: String, label: String)] = [
    ("recruiter_screen", "Recruiter screen"), ("hiring_manager", "Hiring manager"), ("technical", "Technical"),
    ("behavioral", "Behavioral"), ("case", "Case"), ("panel", "Panel"), ("final", "Final round"),
    ("informational", "Informational"), ("other", "Interview"),
]

/// What the sidebar shows.
public struct LibraryFilter: Equatable {
    public var showArchived = false
    /// Matches titles, companies and role titles; `searchHits` adds interviews whose transcript matched.
    public var query = ""
    public var searchHits: Set<Int> = []
    public var outcome: String?
    public var verdict: String?
    public var roleStatus: String?

    public init(showArchived: Bool = false, query: String = "", searchHits: Set<Int> = [], outcome: String? = nil,
                verdict: String? = nil, roleStatus: String? = nil) {
        self.showArchived = showArchived
        self.query = query
        self.searchHits = searchHits
        self.outcome = outcome
        self.verdict = verdict
        self.roleStatus = roleStatus
    }

    public var isFiltering: Bool { !query.trimmingCharacters(in: .whitespaces).isEmpty || outcome != nil || verdict != nil || roleStatus != nil }
}

public struct RoleGroup: Identifiable, Equatable {
    public let role: RoleSummary
    /// Its rounds, oldest first.
    public let sessions: [SessionSummary]
    public var id: Int { role.id }
}

public struct CompanyGroup: Identifiable, Equatable {
    /// The company key, or "" for interviews with no company.
    public let id: String
    public let name: String
    public let roles: [RoleGroup]
    /// Interviews at this company that aren't under a role, newest first.
    public let loose: [SessionSummary]
    public var count: Int { roles.reduce(loose.count) { $0 + $1.sessions.count } }
}

public struct LibraryTree: Equatable {
    public let companies: [CompanyGroup]
    /// Recently Deleted, soonest to be erased first.
    public let deleted: [SessionSummary]
    public var isEmpty: Bool { companies.isEmpty && deleted.isEmpty }
}

/// Group the library into companies → roles → rounds, applying the filter. What you've arranged
/// keeps its place; the rest goes by date, the way a new item would arrive: newer companies, roles
/// and unfiled interviews above the arranged ones, newer rounds at the end of their role.
public func buildTree(_ library: Library, filter: LibraryFilter) -> LibraryTree {
    let query = filter.query.trimmingCharacters(in: .whitespaces).lowercased()
    func matches(_ s: SessionSummary) -> Bool {
        let role = library.role(s.roleId)
        if let outcome = filter.outcome, s.outcome != outcome { return false }
        if let verdict = filter.verdict, s.verdict != verdict { return false }
        if let status = filter.roleStatus, role?.status != status { return false }
        guard !query.isEmpty else { return true }
        let fields = [s.title, s.company, role?.title].compactMap { $0?.lowercased() }
        return fields.contains { $0.contains(query) } || filter.searchHits.contains(s.id)
    }
    let deleted = library.sessions.filter { $0.isDeleted && (query.isEmpty || matches($0)) }
        .sorted { ($0.deletedDaysLeft ?? 0, $0.id) < ($1.deletedDaysLeft ?? 0, $1.id) }
    let shown = library.sessions.filter { !$0.isDeleted && (filter.showArchived || !$0.archived) && matches($0) }

    // Company of each interview: its role's, else its own.
    func key(_ s: SessionSummary) -> String { library.role(s.roleId)?.companyKey ?? s.companyKey ?? "" }
    var order: [String] = []
    var byCompany: [String: [SessionSummary]] = [:]
    for s in shown.sorted(by: { $0.createdAt > $1.createdAt }) {
        let k = key(s)
        if byCompany[k] == nil { order.append(k) }
        byCompany[k, default: []].append(s)
    }
    // Most recent company first, then the ones you've arranged; interviews with no company last.
    var placed: [String: Int] = [:]
    for place in library.companyOrder ?? [] where placed[place.key] == nil { placed[place.key] = place.position }
    order = order.enumerated().sorted { a, b in
        let rank = { (i: Int, k: String) in (k.isEmpty ? 2 : placed[k] == nil ? 0 : 1, placed[k] ?? 0, i) }
        return rank(a.offset, a.element) < rank(b.offset, b.element)
    }.map(\.element)
    let companies = order.map { k -> CompanyGroup in
        let sessions = byCompany[k] ?? []
        // The most common spelling (a role's counts for each of its interviews).
        let spellings = sessions.compactMap { library.role($0.roleId)?.company ?? $0.company }
        let name = k.isEmpty ? "No company"
            : spellings.max { a, b in spellings.filter { $0 == a }.count < spellings.filter { $0 == b }.count } ?? k
        var roleOrder: [Int] = []
        var byRole: [Int: [SessionSummary]] = [:]
        var loose: [SessionSummary] = []
        for s in sessions {
            if let r = s.roleId, library.role(r) != nil {
                if byRole[r] == nil { roleOrder.append(r) }
                byRole[r, default: []].append(s)
            } else {
                loose.append(s)
            }
        }
        // Rounds oldest first, after the ones you've arranged.
        let rounds = { (r: Int) in
            (byRole[r] ?? []).sorted { a, b in
                (a.position == nil ? 1 : 0, a.position ?? 0, a.createdAt, a.id) < (b.position == nil ? 1 : 0, b.position ?? 0, b.createdAt, b.id)
            }
        }
        let roles = arranged(roleOrder.compactMap { library.role($0) }, position: \.position)
            .map { RoleGroup(role: $0, sessions: rounds($0.id)) }
        return CompanyGroup(id: k, name: name, roles: roles, loose: arranged(loose, position: \.position))
    }
    return LibraryTree(companies: companies, deleted: deleted)
}

/// Newest-first `items` with the ones you've arranged after the rest, in their arranged order.
private func arranged<T>(_ items: [T], position: (T) -> Int?) -> [T] {
    items.enumerated().sorted { a, b in
        let (pa, pb) = (position(a.element), position(b.element))
        return (pa == nil ? 0 : 1, pa ?? 0, a.offset) < (pb == nil ? 0 : 1, pb ?? 0, b.offset)
    }.map(\.element)
}

/// A search match (`ic search --json`).
public struct SearchHit: Decodable, Equatable {
    public let sessionId: Int
    public let kind: String
    public let at: Double?
    public let text: String
}
