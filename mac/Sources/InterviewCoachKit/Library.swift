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
}

/// Every interview (archived and deleted ones flagged) and every role.
public struct Library: Decodable, Equatable {
    public var sessions: [SessionSummary]
    public var roles: [RoleSummary]

    public init(sessions: [SessionSummary] = [], roles: [RoleSummary] = []) {
        self.sessions = sessions
        self.roles = roles
    }

    public func role(_ id: Int?) -> RoleSummary? { id.flatMap { id in roles.first { $0.id == id } } }
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

/// Group the library into companies → roles → rounds, applying the filter.
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
    // Most recent company first; interviews with no company last.
    order.sort { ($0.isEmpty ? 1 : 0) < ($1.isEmpty ? 1 : 0) }
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
        let roles = roleOrder.compactMap { r in
            library.role(r).map { RoleGroup(role: $0, sessions: (byRole[r] ?? []).sorted { $0.createdAt < $1.createdAt }) }
        }
        return CompanyGroup(id: k, name: name, roles: roles, loose: loose)
    }
    return LibraryTree(companies: companies, deleted: deleted)
}

/// A search match (`ic search --json`).
public struct SearchHit: Decodable, Equatable {
    public let sessionId: Int
    public let kind: String
    public let at: Double?
    public let text: String
}
