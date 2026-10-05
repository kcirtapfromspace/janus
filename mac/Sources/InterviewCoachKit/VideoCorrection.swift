import Foundation

/// A label's value: yes/no checks are booleans, the others option ids.
public enum LabelValue: Decodable, Equatable {
    case bool(Bool)
    case string(String)

    public init(from decoder: Decoder) throws {
        let container = try decoder.singleValueContainer()
        if let value = try? container.decode(Bool.self) {
            self = .bool(value)
        } else {
            self = .string(try container.decode(String.self))
        }
    }

    var json: Any {
        switch self {
        case .bool(let value): value
        case .string(let value): value
        }
    }
}

/// One answer's video cues as you saw them: the form behind Correct video cues. Its checks, options
/// and consistency rules are the labelling page's (src/label.rs, src/label.html), so a correction
/// is a label like any other.
public struct VideoCorrection: Equatable {
    public struct Check: Identifiable {
        public let id: String
        public let title: String
        public let hint: String
        public let options: [(value: String, title: String)]
    }

    /// "?" is "Can't tell": the check is left out of the label.
    public static let unsure = "?"

    public static let checks: [Check] = [
        Check(id: "on_camera", title: "Others on camera", hint: "People besides you whose face was visible for most of the answer.",
              options: [("0", "0"), ("1", "1"), ("2", "2"), ("3+", "3+")]),
        Check(id: "nodded", title: "Did anyone else nod?", hint: "A deliberate down-and-up of the head, not moving along with speech.",
              options: [("true", "Yes"), ("false", "No")]),
        Check(id: "nod_count", title: "How many nods?", hint: "Across everyone else; each dip counts once.",
              options: [("none", "None"), ("1-2", "1–2"), ("3-5", "3–5"), ("6+", "6+")]),
        Check(id: "looked_away", title: "How much did they look away?", hint: "Face clearly turned off the screen (aside, or down at notes) for a second or more.",
              options: [("rarely", "Rarely"), ("sometimes", "Sometimes"), ("mostly", "Mostly")]),
        Check(id: "smiled", title: "Did anyone else smile?", hint: "Lip corners visibly raised.",
              options: [("true", "Yes"), ("false", "No")]),
    ]

    /// The answer per check id (an option's value, or `unsure`).
    public private(set) var picks: [String: String]

    /// Starting from what the video measured, so you only change what it got wrong.
    public init(measured cues: SessionDetail.VideoCues) {
        let count = switch cues.nods {
        case 0: "none"
        case 1...2: "1-2"
        case 3...5: "3-5"
        default: "6+"
        }
        let people = cues.onCamera < 0.5 ? "0" : cues.onCamera < 1.5 ? "1" : cues.onCamera < 2.5 ? "2" : "3+"
        let away: String = switch cues.lookingAway {
        case nil: Self.unsure
        case let share? where share < 0.10: "rarely"
        case let share? where share <= 0.40: "sometimes"
        default: "mostly"
        }
        picks = ["on_camera": people, "nodded": cues.nods > 0 ? "true" : "false", "nod_count": count, "looked_away": away, "smiled": Self.unsure]
    }

    /// Your earlier correction.
    public init(corrected: [String: LabelValue]) {
        picks = Dictionary(uniqueKeysWithValues: Self.checks.map { ($0.id, Self.unsure) })
        for (key, value) in corrected where picks[key] != nil {
            picks[key] = switch value {
            case .bool(let yes): yes ? "true" : "false"
            case .string(let text): text
            }
        }
    }

    public func pick(_ check: String) -> String { picks[check] ?? Self.unsure }

    /// Set one answer, keeping the rest consistent the way the labelling page does: no nod means no
    /// nods counted, a count means a nod, and nobody on camera means nobody nodded or smiled.
    public mutating func set(_ check: String, _ value: String) {
        picks[check] = value
        switch (check, value) {
        case ("nodded", "false"): picks["nod_count"] = "none"
        case ("nodded", "true") where picks["nod_count"] == "none": picks["nod_count"] = Self.unsure
        case ("nod_count", "none"): picks["nodded"] = "false"
        case ("nod_count", let count) where count != Self.unsure: picks["nodded"] = "true"
        case ("on_camera", "0"):
            picks["nodded"] = "false"
            picks["nod_count"] = "none"
            picks["smiled"] = "false"
            picks["looked_away"] = Self.unsure
        default: break
        }
    }

    /// The label as `ic eval correct` takes it: "Can't tell" checks left out.
    public var labelsJSON: String {
        var labels: [String: Any] = [:]
        for (key, value) in picks where value != Self.unsure {
            labels[key] = switch value {
            case "true": true as Any
            case "false": false as Any
            default: value as Any
            }
        }
        let data = (try? JSONSerialization.data(withJSONObject: labels, options: [.sortedKeys])) ?? Data("{}".utf8)
        return String(decoding: data, as: UTF8.self)
    }
}
