import SwiftUI

/// Shown once (new installs, and existing ones after updating): what Janus shares by default, and
/// the switches to turn each off. Nothing is shared or sent until OK is pressed here.
struct PrivacyNoticeWindow: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        let privacy = model.setup?.privacy
        VStack(alignment: .leading, spacing: 18) {
            HStack(spacing: 14) {
                BrandMark(size: 40)
                VStack(alignment: .leading, spacing: 3) {
                    Text("What Janus shares").font(CoachTheme.editorial(23)).foregroundStyle(CoachTheme.ink)
                    Text("Your recordings, transcripts and reviews stay on this Mac. These two things leave it.")
                        .font(.system(size: 11)).foregroundStyle(CoachTheme.muted)
                }
            }
            section(
                title: "Your interviews' questions",
                isOn: privacy?.shareQuestions ?? true, key: "share-questions",
                text: "After each review, the interviewer's questions are rewritten so they name no person, company or product, checked again, and sent to Janus's question registry. Once approved, other Janus users practise with them. Never your answers, transcripts, audio or video. You can withdraw what you've shared from Settings."
            )
            section(
                title: "Anonymous diagnostics",
                isOn: privacy?.diagnostics ?? true, key: "diagnostics",
                text: "Whether recording, reading the video, reviews and practice worked: counts, outcomes and warning codes, with a random id for this Mac. Never content, names or file paths."
            )
            Text("Both are on unless you turn them off here or in Settings. Nothing is sent until you press OK.")
                .font(.system(size: 11)).foregroundStyle(CoachTheme.muted).fixedSize(horizontal: false, vertical: true)
            HStack {
                Spacer()
                Button("OK") {
                    Task {
                        await model.acknowledgePrivacyNotice()
                        dismiss()
                    }
                }
                .buttonStyle(CoachPrimaryButtonStyle())
                .keyboardShortcut(.defaultAction)
            }
        }
        .padding(26)
        .frame(width: 500)
        .background(CoachTheme.canvas)
    }

    private func section(title: String, isOn: Bool, key: String, text: String) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            Toggle(title, isOn: Binding(get: { isOn }, set: { model.setPrivacy(key, $0) }))
                .toggleStyle(.switch).font(.system(size: 13, weight: .semibold))
            Text(text).font(.system(size: 12)).foregroundStyle(CoachTheme.muted).fixedSize(horizontal: false, vertical: true)
        }
        .padding(14)
        .frame(maxWidth: .infinity, alignment: .leading)
        .overlay(RoundedRectangle(cornerRadius: 8).stroke(.quaternary))
    }
}
