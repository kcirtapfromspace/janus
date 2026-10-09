import Foundation
@testable import InterviewCoachKit
import XCTest

final class LibraryTests: XCTestCase {
    private func session(_ id: Int, _ date: String, title: String, company: String? = nil, role: Int? = nil,
                         archived: Bool = false, deletedDaysLeft: Int? = nil, outcome: String? = nil) -> String {
        let quote = { (s: String?) in s.map { "\"\($0)\"" } ?? "null" }
        return """
        {"id": \(id), "created_at": "\(date)T10:00:00+00:00", "title": "\(title)", "company": \(quote(company)),
         "company_key": \(quote(company?.lowercased())), "entered_company": null, "stage": null, "stage_id": null,
         "status": "analyzed", "mode": "dual", "duration_s": 1800, "dir": "/x", "verdict": null, "verdict_label": null,
         "outcome": \(quote(outcome)), "outcome_label": null, "report_path": null, "transcript_path": null, "error": null,
         "role_id": \(role.map(String.init) ?? "null"), "archived": \(archived),
         "deleted_days_left": \(deletedDaysLeft.map(String.init) ?? "null")}
        """
    }

    private func role(_ id: Int, _ title: String, company: String, status: String = "interviewing") -> String {
        """
        {"id": \(id), "title": "\(title)", "company": "\(company)", "company_key": "\(company.lowercased())",
         "status": "\(status)", "status_label": "\(status.capitalized)", "archived": false}
        """
    }

    private var library: Library { library() }

    private func library(sessions more: [String] = [], roles moreRoles: [String] = []) -> Library {
        let sessions = [
            session(1, "2026-09-01", title: "Recruiter screen", company: "Agility", role: 10),
            session(2, "2026-09-10", title: "Technical", company: "Agility", role: 10),
            session(3, "2026-09-12", title: "Coffee chat", company: "agility"),
            session(4, "2026-09-20", title: "HM screen", company: "Northwind", role: 11, outcome: "advanced"),
            session(5, "2026-09-25", title: "Practice run"),
            session(6, "2026-09-26", title: "Old one", company: "Acme", archived: true),
            session(7, "2026-09-27", title: "Mistake", company: "Acme", deletedDaysLeft: 12),
        ] + more
        let roles = [role(10, "ML Platform Engineer", company: "Agility"), role(11, "Senior PM", company: "Northwind", status: "offer")]
            + moreRoles
        let json = #"{"sessions": [\#(sessions.joined(separator: ","))], "roles": [\#(roles.joined(separator: ","))]}"#
        return try! ICClient.decode(Library.self, from: Data(json.utf8))
    }

    func testGroupsCompaniesThenRolesThenRounds() {
        let tree = buildTree(library, filter: LibraryFilter())
        XCTAssertEqual(tree.companies.map(\.name), ["Northwind", "Agility", "No company"], "most recent first, no company last")
        let agility = tree.companies[1]
        XCTAssertEqual(agility.roles.map(\.role.title), ["ML Platform Engineer"])
        XCTAssertEqual(agility.roles[0].sessions.map(\.id), [1, 2], "rounds oldest first")
        XCTAssertEqual(agility.loose.map(\.id), [3], "the same company, spelled differently, without a role")
        XCTAssertEqual(tree.companies[2].loose.map(\.id), [5])
        XCTAssertFalse(tree.companies.contains { $0.name == "Acme" }, "archived hidden; deleted go to Recently Deleted")
        XCTAssertEqual(tree.deleted.map(\.id), [7])
    }

    func testShowArchivedAndFilters() {
        XCTAssertTrue(buildTree(library, filter: LibraryFilter(showArchived: true)).companies.contains { $0.name == "Acme" })
        let offers = buildTree(library, filter: LibraryFilter(roleStatus: "offer"))
        XCTAssertEqual(offers.companies.flatMap { $0.roles.flatMap(\.sessions) }.map(\.id), [4])
        let advanced = buildTree(library, filter: LibraryFilter(outcome: "advanced"))
        XCTAssertEqual(advanced.companies.map(\.name), ["Northwind"])
    }

    /// What you've arranged keeps its place; newer companies and roles arrive above it, newer rounds below.
    func testArrangedItemsKeepTheirPlaceAndNewOnesArriveByDate() {
        var lib = library
        lib.arrangeCompanies(["agility", "northwind"])
        lib.arrangeInterviews([2, 1])
        var tree = buildTree(lib, filter: LibraryFilter())
        XCTAssertEqual(tree.companies.map(\.name), ["Agility", "Northwind", "No company"])
        XCTAssertEqual(tree.companies[0].roles[0].sessions.map(\.id), [2, 1])

        let later = library(sessions: [session(8, "2026-09-30", title: "Onsite", company: "Agility", role: 10),
                                       session(9, "2026-10-01", title: "Intro", company: "Zeta")])
        lib.sessions = later.sessions.map { s in lib.sessions.first { $0.id == s.id } ?? s }
        tree = buildTree(lib, filter: LibraryFilter())
        XCTAssertEqual(tree.companies.map(\.name), ["Zeta", "Agility", "Northwind", "No company"], "a new company arrives on top")
        XCTAssertEqual(tree.companies[1].roles[0].sessions.map(\.id), [2, 1, 8], "a new round arrives at the end")

        XCTAssertTrue(lib.isArranged)
        lib.resetArrangement()
        XCTAssertFalse(lib.isArranged)
        XCTAssertEqual(buildTree(lib, filter: LibraryFilter()).companies[1].roles[0].sessions.map(\.id), [1, 2, 8])
    }

    func testRolesAtACompanyCanBeArranged() {
        var lib = library(sessions: [session(8, "2026-09-12", title: "Staff loop", company: "Agility", role: 12)],
                          roles: [role(12, "Staff Engineer", company: "Agility")])
        XCTAssertEqual(buildTree(lib, filter: LibraryFilter()).companies[1].roles.map(\.id), [12, 10], "latest first")
        lib.arrangeRoles([10, 12])
        XCTAssertEqual(buildTree(lib, filter: LibraryFilter()).companies[1].roles.map(\.id), [10, 12])
    }

    /// Arranging companies again keeps the places of ones not shown (archived, say), as ic does.
    func testArrangingCompaniesKeepsOthersPlaces() {
        var lib = library
        lib.arrangeCompanies(["acme", "agility", "northwind"])
        lib.arrangeCompanies(["northwind", "agility"])
        XCTAssertEqual(lib.companyOrder, [CompanyPlace(key: "acme", position: 0), CompanyPlace(key: "northwind", position: 0),
                                          CompanyPlace(key: "agility", position: 1)])
    }

    func testMovingFollowsListMovesAndDrops() {
        let items = ["a", "b", "c", "d"]
        XCTAssertEqual(moving(items, fromOffsets: [0], toOffset: 3), ["b", "c", "a", "d"])
        XCTAssertEqual(moving(items, fromOffsets: [3], toOffset: 0), ["d", "a", "b", "c"])
        XCTAssertEqual(moving(items, fromOffsets: [0, 2], toOffset: 4), ["b", "d", "a", "c"])
        XCTAssertEqual(moving(items, "a", onto: "c"), ["b", "c", "a", "d"], "dragged down: just after")
        XCTAssertEqual(moving(items, "d", onto: "b"), ["a", "d", "b", "c"], "dragged up: just before")
        XCTAssertEqual(moving(items, "x", onto: "b"), items)
    }

    func testDeletingAGroupTakesWhatItShows() {
        XCTAssertEqual(library.interviews(atCompany: "agility").map(\.id), [1, 2, 3])
        XCTAssertEqual(library.interviews(atCompany: "acme").map(\.id), [6], "archived ones too, not deleted ones")
        XCTAssertEqual(library.interviews(inRole: 10).map(\.id), [1, 2])
    }

    func testSearchMatchesTitlesRolesAndTranscriptHits() {
        let byRole = buildTree(library, filter: LibraryFilter(query: "platform"))
        XCTAssertEqual(byRole.companies.flatMap { $0.roles.flatMap(\.sessions) }.map(\.id), [1, 2])
        let byTranscript = buildTree(library, filter: LibraryFilter(query: "kubernetes", searchHits: [5]))
        XCTAssertEqual(byTranscript.companies.map(\.name), ["No company"])
        XCTAssertTrue(buildTree(library, filter: LibraryFilter(query: "zzz")).isEmpty)
    }
}
