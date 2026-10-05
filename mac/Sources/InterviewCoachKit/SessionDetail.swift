import Foundation

/// One session's whole pipeline, as `ic session N` returns it.
public struct SessionDetail: Decodable, Equatable {
    public let session: Info
    public let stages: [StageState]
    public let audio: Audio
    public let turns: [TurnLine]
    /// Every after-action report run, newest first.
    public let reports: [ReportRun]
    public let nextSteps: StoredNextSteps?
    public let outcome: OutcomeInfo?

    public struct Info: Decodable, Equatable {
        public let id: Int
        public let title: String
        public let company: String?
        public let stage: String?
        public let createdAt: String
        public let durationS: Double?
        public let mode: String
        public let source: String
        public let status: String
        public let dir: String

        public var isSingleTrack: Bool { mode == "single" }
    }

    public struct Audio: Decodable, Equatable {
        /// Both tracks mixed into one file to listen to.
        public let listenPath: String?
        public let tracks: [Track]
        public let warnings: [String]
    }

    public struct Track: Decodable, Equatable, Identifiable {
        public let name: String
        public let label: String
        public let path: String
        public var id: String { name }
    }

    public struct TurnLine: Decodable, Equatable, Identifiable {
        public let speaker: String
        public let speakerLabel: String
        public let start: Double
        public let end: Double
        public let timestamp: String
        public let text: String
        public var id: Double { start }
        public var isYou: Bool { speaker == "you" }
    }

    public struct ReportRun: Decodable, Equatable, Identifiable {
        public let analysisId: Int
        public let createdAt: String
        public let model: String
        public let verdict: String
        public let verdictLabel: String
        public let confidence: String
        public let htmlPath: String
        public let isCurrent: Bool
        public let unverifiedQuotes: Int
        public var id: Int { analysisId }
    }

    public struct OutcomeInfo: Decodable, Equatable {
        public let result: String
        public let label: String
        public let notes: String?
    }

    public func stage(_ step: StageStep) -> StageState? { stages.first { $0.step == step } }
    public var isBusy: Bool { stages.contains { $0.status == .running } }
    /// The first stage that's out of date: re-running it with --then-later brings everything current.
    public var firstOutOfDate: StageState? { stages.first { $0.status == .outOfDate } }
}

/// The four stages, in order.
public enum StageStep: String, Decodable, CaseIterable, Identifiable {
    case recording, transcript, report, next
    public var id: String { rawValue }

    /// The stage's name where space is tight (the flow strip); the full name heads its pane.
    public var shortTitle: String {
        switch self {
        case .recording: "Recording"
        case .transcript: "Transcript"
        case .report: "Review"
        case .next: "Prepare"
        }
    }
}

public enum StageStatus: String, Decodable {
    case notRun = "not_run", running, done, outOfDate = "out_of_date", failed
}

public struct StageState: Decodable, Equatable, Identifiable {
    public let step: StageStep
    public let label: String
    public let status: StageStatus
    public let summary: String?
    public let lastRunAt: String?
    public let durationS: Double?
    public let error: String?
    public let progress: Double?
    public let message: String?
    public let model: String?
    public let canRerun: Bool
    public let rerunBlocked: String?
    /// Problems with the current result, e.g. a mic track that stopped early.
    public let warnings: [String]
    public var id: StageStep { step }
    /// Something this stage produced is available to show.
    public var hasResult: Bool { summary != nil }
}

public struct StoredNextSteps: Decodable, Equatable {
    public let id: Int
    public let createdAt: String
    public let model: String
    public let plan: NextStepsPlan
    public let unverifiedQuotes: [String]
}

public struct NextStepsPlan: Decodable, Equatable {
    public let headline: String
    public let nextRoundPrep: [PrepItem]
    public let practicePlan: [PracticeItem]
}

public struct Evidence: Decodable, Equatable {
    public let quote: String
    public let timestamp: String
}

public struct PrepItem: Decodable, Equatable, Identifiable {
    public let topic: String
    public let why: String
    public let evidence: Evidence
    public let howToPrepare: String
    public let likelyQuestions: [String]
    public var id: String { topic }
}

public struct PracticeItem: Decodable, Equatable, Identifiable {
    public let skill: String
    public let fromCoaching: String
    public let drill: String
    public let minutes: Int
    public let priority: String
    public var id: String { skill + drill }
}

/// Seconds from an "HH:MM:SS" timestamp (used to play the audio from a quote).
public func seconds(fromTimestamp timestamp: String) -> Double? {
    let parts = timestamp.split(separator: ":").compactMap { Double($0) }
    guard parts.count == 3 else { return nil }
    return parts[0] * 3600 + parts[1] * 60 + parts[2]
}
