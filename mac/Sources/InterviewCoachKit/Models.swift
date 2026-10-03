import Foundation

/// A model a report can be written with (`ic models --json`), with its list price.
public struct ModelOffer: Decodable, Equatable, Identifiable {
    /// `provider/model`, as `--model` takes it.
    public let model: String
    public let provider: String
    public let inputPerMtok: Double?
    public let outputPerMtok: Double?
    /// Dollars for a typical report at list price.
    public let typicalReport: Double?
    public let cheapest: Bool

    public var id: String { model }

    /// `claude-opus-5-5`, without the provider.
    public var name: String { model.split(separator: "/", maxSplits: 1).last.map(String.init) ?? model }

    /// "about $0.06 a report"
    public var priceLabel: String? {
        typicalReport.map { $0 < 0.01 ? "under 1¢ a report" : String(format: "about $%.2f a report", $0) }
    }
}
