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

    private var library: Library {
        let json = """
        {"sessions": [
          \(session(1, "2026-09-01", title: "Recruiter screen", company: "Agility", role: 10)),
          \(session(2, "2026-09-10", title: "Technical", company: "Agility", role: 10)),
          \(session(3, "2026-09-12", title: "Coffee chat", company: "agility")),
          \(session(4, "2026-09-20", title: "HM screen", company: "Northwind", role: 11, outcome: "advanced")),
          \(session(5, "2026-09-25", title: "Practice run")),
          \(session(6, "2026-09-26", title: "Old one", company: "Acme", archived: true)),
          \(session(7, "2026-09-27", title: "Mistake", company: "Acme", deletedDaysLeft: 12))],
         "roles": [
          {"id": 10, "title": "ML Platform Engineer", "company": "Agility", "company_key": "agility", "status": "interviewing",
           "status_label": "Interviewing", "archived": false},
          {"id": 11, "title": "Senior PM", "company": "Northwind", "company_key": "northwind", "status": "offer",
           "status_label": "Offer", "archived": false}]}
        """
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

    func testSearchMatchesTitlesRolesAndTranscriptHits() {
        let byRole = buildTree(library, filter: LibraryFilter(query: "platform"))
        XCTAssertEqual(byRole.companies.flatMap { $0.roles.flatMap(\.sessions) }.map(\.id), [1, 2])
        let byTranscript = buildTree(library, filter: LibraryFilter(query: "kubernetes", searchHits: [5]))
        XCTAssertEqual(byTranscript.companies.map(\.name), ["No company"])
        XCTAssertTrue(buildTree(library, filter: LibraryFilter(query: "zzz")).isEmpty)
    }
}
