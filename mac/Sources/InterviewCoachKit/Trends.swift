import Foundation

/// Your progress across reviewed interviews (`ic trends --json`): each area the reviews measure,
/// interview by interview, and whether it's getting better or worse. ic owns the rule; the app shows it.
public struct Trends: Decodable, Equatable {
    public struct Interview: Decodable, Equatable, Identifiable {
        public let sessionId: Int
        public let createdAt: String
        public let title: String
        public let company: String?
        public let round: String
        public let model: String
        public let verdictLabel: String
        public var id: Int { sessionId }
    }

    public struct Point: Decodable, Equatable {
        public let sessionId: Int
        public let value: Double
    }

    public struct Measure: Decodable, Equatable, Identifiable {
        public let id: String
        public let label: String
        /// overall, rubric, answers, delivery, room.
        public let group: String
        /// score, percent, per_100_words, seconds, share, outlook, warmth.
        public let unit: String
        /// higher, lower, neither.
        public let better: String
        public let band: Double
        /// Oldest first; interviews that didn't measure it are left out.
        public let points: [Point]
        public let recent: Double?
        public let recentCount: Int
        public let earlier: Double?
        public let earlierCount: Int
        public let change: Double?
        /// improving, slipping, steady, up, down, too_few.
        public let direction: String
        /// More interviews it needs before it shows a direction.
        public let needed: Int

        public func format(_ value: Double) -> String {
            switch unit {
            case "percent", "share": return "\(Int((value * 100).rounded()))%"
            case "seconds":
                let s = Int(value.rounded())
                return s >= 60 ? "\(s / 60)m \(String(format: "%02d", s % 60))s" : "\(s)s"
            case "outlook", "warmth": return String(format: "%+.1f", value)
            default: return String(format: "%.1f", value)
            }
        }

        /// What the number is, for units a label doesn't explain.
        public var unitNote: String? {
            switch unit {
            case "per_100_words": "per 100 of your words"
            case "percent": "of your answers"
            case "share": "of the talk time"
            case "seconds": "per answer, on average"
            case "warmth": "−1 cool to +1 warm"
            case "outlook": "a prediction, not a result"
            default: nil
            }
        }

        public var directionLabel: String {
            switch direction {
            case "improving": "Getting better"
            case "slipping": "Slipping"
            case "steady": "Steady"
            case "up": "Up"
            case "down": "Down"
            default: needed == 1 ? "1 more to tell" : "\(needed) more to tell"
            }
        }

        /// The range the line is drawn on: the unit's whole scale, so a flat 4 doesn't look like a cliff.
        public var scale: ClosedRange<Double> {
            switch unit {
            case "score": 1...5
            case "percent", "share": 0...1
            case "outlook": -2...2
            case "warmth": -1...1
            default: 0...max(1, (points.map(\.value).max() ?? 0) * 1.15)
            }
        }
    }

    public struct Theme: Decodable, Equatable, Identifiable {
        public let title: String
        public let count: Int
        public let lastSeen: String
        /// It came up in your latest review.
        public let inLatest: Bool
        public var id: String { title }
    }

    public struct Standing: Decodable, Equatable {
        public let label: String
        public let value: Double
    }

    public struct Summary: Decodable, Equatable {
        public let headline: String
        public let detail: String?
        public let improving: [String]
        public let slipping: [String]
        public let strongest: Standing?
        public let weakest: Standing?
        public let recurring: [Theme]
    }

    /// The window, in days including today (nil: all time).
    public let days: Int?
    /// Reviewed interviews, oldest first.
    public let interviews: [Interview]
    public let measures: [Measure]
    public let summary: Summary
    /// The models that wrote these reviews.
    public let models: [String]

    public var overall: Measure? { measures.first { $0.id == "overall" } }

    public static let groups: [(id: String, title: String)] = [
        ("rubric", "The rubric"), ("answers", "Your answers"), ("delivery", "How you speak"), ("room", "The room"),
    ]

    public func measures(in group: String) -> [Measure] { measures.filter { $0.group == group } }

    /// Where an interview falls along the reviewed interviews, so every line shares one axis.
    public func position(of sessionId: Int) -> Int? { interviews.firstIndex { $0.sessionId == sessionId } }
}
