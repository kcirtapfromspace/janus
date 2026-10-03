import Foundation
@testable import InterviewCoachKit
import XCTest

final class ModelsTests: XCTestCase {
    /// The shape `ic models --json` prints (copied from a real run, plus an unpriced model).
    func testDecodesModelOffers() throws {
        let json = """
        [{"model": "anthropic/claude-haiku-4-5-20251001", "provider": "anthropic", "input_per_mtok": 1.0, "output_per_mtok": 5.0,
          "typical_report": 0.055, "cheapest": true},
         {"model": "anthropic/claude-new-6", "provider": "anthropic", "input_per_mtok": null, "output_per_mtok": null,
          "typical_report": null, "cheapest": false}]
        """
        let offers = try ICClient.decode([ModelOffer].self, from: Data(json.utf8))
        XCTAssertEqual(offers.map(\.name), ["claude-haiku-4-5-20251001", "claude-new-6"])
        XCTAssertEqual(offers[0].priceLabel, "about $0.06 a report")
        XCTAssertTrue(offers[0].cheapest)
        XCTAssertNil(offers[1].priceLabel)
    }
}
