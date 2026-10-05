import InterviewCoachKit
import SwiftUI

/// The sidebar: companies, the roles at each, and their interviews in order, with search, filters,
/// archive and Recently Deleted. Several interviews can be selected at once.
struct LibrarySidebar: View {
    @Environment(AppModel.self) private var model
    @State private var collapsed: Set<Int> = []
    @State private var editing: SessionSummary?
    @State private var naming: NamePrompt?
    @State private var erasing: [Int]?

    var body: some View {
        @Bindable var model = model
        let tree = buildTree(model.library, filter: model.filter)
        List(selection: $model.listSelection) {
            ForEach(tree.companies) { company in
                Section {
                    ForEach(company.roles) { group in
                        DisclosureGroup(isExpanded: expanded(group.id)) {
                            ForEach(group.sessions) { SessionRow(session: $0, inRole: true).tag($0.id) }
                        } label: {
                            RoleHeader(group: group).contextMenu { roleMenu(group.role) }
                        }
                    }
                    ForEach(company.loose) { SessionRow(session: $0).tag($0.id) }
                } header: {
                    CompanyHeader(company: company).contextMenu { companyMenu(company) }
                }
            }
            if !tree.deleted.isEmpty {
                Section {
                    ForEach(tree.deleted) { SessionRow(session: $0).tag($0.id) }
                    Button("Empty Recently Deleted…") { erasing = tree.deleted.map(\.id) }
                        .buttonStyle(.link)
                        .font(.caption)
                } header: {
                    Text("Recently Deleted")
                }
            }
        }
        .listStyle(.sidebar)
        .scrollContentBackground(.hidden)
        .background(CoachTheme.canvas)
        .safeAreaInset(edge: .top, spacing: 0) {
            VStack(alignment: .leading, spacing: 20) {
                BrandLockup()
                Button { model.listSelection = [] } label: {
                    HStack(spacing: 10) {
                        Text("Notebook").font(.system(size: 12, weight: .medium))
                        Spacer()
                    }
                    .foregroundStyle(model.listSelection.isEmpty ? CoachTheme.accent : CoachTheme.muted)
                    .padding(.horizontal, 12).padding(.vertical, 10)
                    .overlay(alignment: .leading) {
                        if model.listSelection.isEmpty { Rectangle().fill(CoachTheme.accent).frame(width: 2, height: 18) }
                    }
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                Text("Interviews").font(.system(size: 11, weight: .medium))
                    .foregroundStyle(CoachTheme.muted).padding(.leading, 4)
            }
            .padding(.horizontal, 16).padding(.top, 20).padding(.bottom, 8)
            .background(CoachTheme.canvas)
        }
        .contextMenu(forSelectionType: Int.self) { ids in sessionMenu(Array(ids)) }
        .searchable(text: $model.filter.query, placement: .sidebar, prompt: "Search interviews")
        .task(id: model.filter.query) {
            try? await Task.sleep(for: .milliseconds(300))
            await model.searchTranscripts(model.filter.query)
        }
        .safeAreaInset(edge: .bottom, spacing: 0) { FilterBar() }
        .overlay {
            if tree.isEmpty {
                if model.library.sessions.isEmpty {
                    ContentUnavailableView("No interviews yet", systemImage: "waveform",
                                           description: Text("Record one with the Record button, or import a recording."))
                } else {
                    ContentUnavailableView.search(text: model.filter.query)
                }
            }
        }
        .sheet(item: $editing) { EditDetailsSheet(session: $0) }
        .sheet(item: $naming) { NameSheet(prompt: $0) }
        .confirmationDialog(erasing.map { $0.count == 1 ? "Erase this interview now?" : "Erase \($0.count) interviews now?" } ?? "",
                            isPresented: Binding(get: { erasing != nil }, set: { if !$0 { erasing = nil } })) {
            Button("Erase", role: .destructive) {
                if let ids = erasing { model.erase(ids) }
                erasing = nil
            }
        } message: {
            Text("The recording, transcript and every report are removed for good. This can't be undone.")
        }
    }

    private func expanded(_ role: Int) -> Binding<Bool> {
        Binding(get: { !collapsed.contains(role) }, set: { open in if open { collapsed.remove(role) } else { collapsed.insert(role) } })
    }

    private func sessions(_ ids: [Int]) -> [SessionSummary] { model.library.sessions.filter { ids.contains($0.id) } }

    // MARK: Menus

    @ViewBuilder private func sessionMenu(_ ids: [Int]) -> some View {
        let picked = sessions(ids)
        if !picked.isEmpty {
            if picked.allSatisfy(\.isDeleted) {
                Button("Restore") { model.restore(ids) }
                Button("Erase Now…", role: .destructive) { erasing = ids }
            } else {
                if picked.count == 1, let one = picked.first {
                    Button("Edit Details…") { editing = one }
                }
                Menu("Move to Role") {
                    let companies = Set(picked.map { $0.companyKey ?? "" })
                    let roles = model.library.roles.filter { companies.contains($0.companyKey ?? "") && !$0.archived }
                    ForEach(roles) { role in Button(role.title) { model.move(ids, toRole: role.id) } }
                    if !roles.isEmpty { Divider() }
                    Button("New Role…") {
                        naming = NamePrompt(title: "New role", placeholder: "e.g. ML Platform Engineer", initial: "") { model.move(ids, newRole: $0) }
                    }
                    Button("No Role") { model.move(ids) }
                }
                Divider()
                if picked.allSatisfy(\.archived) {
                    Button("Unarchive") { model.archive(ids, undo: true) }
                } else {
                    Button("Archive") { model.archive(ids) }
                }
                Button("Delete") { model.delete(ids) }
            }
        }
    }

    @ViewBuilder private func roleMenu(_ role: RoleSummary) -> some View {
        Menu("Status") {
            ForEach(roleStatuses, id: \.id) { status in
                Toggle(status.label, isOn: Binding(get: { role.status == status.id }, set: { _ in model.setRoleStatus(role.id, status.id) }))
            }
        }
        Button("Rename Role…") {
            naming = NamePrompt(title: "Rename role", placeholder: "Role title", initial: role.title) { model.renameRole(role.id, $0) }
        }
        let others = model.library.roles.filter { $0.id != role.id && $0.companyKey == role.companyKey }
        if !others.isEmpty {
            Menu("Merge Into") {
                ForEach(others) { other in Button(other.title) { model.mergeRole(role.id, into: other.id) } }
            }
        }
        Divider()
        Button(role.archived ? "Unarchive Role" : "Archive Role") { model.archiveRole(role.id, undo: role.archived) }
    }

    @ViewBuilder private func companyMenu(_ company: CompanyGroup) -> some View {
        if !company.id.isEmpty {
            Button("Rename Company…") {
                naming = NamePrompt(title: "Rename company", placeholder: "Company", initial: company.name) {
                    model.renameCompany(company.name, $0)
                }
            }
            let archived = company.roles.allSatisfy(\.role.archived) && company.loose.allSatisfy(\.archived)
            Button(archived ? "Unarchive Company" : "Archive Company") { model.archiveCompany(company.name, undo: archived) }
        }
    }
}

struct CompanyHeader: View {
    let company: CompanyGroup

    var body: some View {
        HStack {
            Text(company.name).foregroundStyle(CoachTheme.muted)
            Spacer()
            Text("\(company.count)").foregroundStyle(.tertiary).monospacedDigit()
        }
    }
}

struct RoleHeader: View {
    let group: RoleGroup

    var body: some View {
        HStack(spacing: 6) {
            Image(systemName: "briefcase").foregroundStyle(.secondary)
            Text(group.role.title).font(.system(size: 12, weight: .medium)).foregroundStyle(CoachTheme.ink).lineLimit(1)
            Spacer(minLength: 4)
            StatusChip(status: group.role.status, label: group.role.statusLabel)
        }
        .opacity(group.role.archived ? 0.55 : 1)
    }
}

struct StatusChip: View {
    let status: String
    let label: String

    private var color: Color {
        switch status {
        case "offer", "accepted": CoachTheme.accent
        case "rejected": CoachTheme.alert
        case "withdrawn": .secondary
        default: CoachTheme.accent
        }
    }

    var body: some View {
        Text(label)
            .font(.system(size: 10))
            .foregroundStyle(color)
    }
}

/// Show archived, and filter by outcome, verdict and where the role stands.
struct FilterBar: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        @Bindable var model = model
        HStack {
            Toggle("Show archived", isOn: $model.filter.showArchived)
                .toggleStyle(.checkbox)
                .font(.callout)
            Spacer()
            Menu {
                Picker("Outcome", selection: $model.filter.outcome) {
                    Text("Any outcome").tag(String?.none)
                    ForEach(outcomeChoices, id: \.value) { Text($0.label).tag(Optional($0.value)) }
                }
                Picker("Verdict", selection: $model.filter.verdict) {
                    Text("Any verdict").tag(String?.none)
                    ForEach(verdictChoices, id: \.id) { Text($0.label).tag(Optional($0.id)) }
                }
                Picker("Role status", selection: $model.filter.roleStatus) {
                    Text("Any status").tag(String?.none)
                    ForEach(roleStatuses, id: \.id) { Text($0.label).tag(Optional($0.id)) }
                }
                if model.filter.outcome != nil || model.filter.verdict != nil || model.filter.roleStatus != nil {
                    Divider()
                    Button("Clear Filters") {
                        model.filter.outcome = nil
                        model.filter.verdict = nil
                        model.filter.roleStatus = nil
                    }
                }
            } label: {
                let on = model.filter.outcome != nil || model.filter.verdict != nil || model.filter.roleStatus != nil
                Image(systemName: on ? "line.3.horizontal.decrease.circle.fill" : "line.3.horizontal.decrease.circle")
            }
            .menuStyle(.borderlessButton)
            .fixedSize()
            .help("Filter by outcome, verdict or where the role stands")
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 8)
        .background(CoachTheme.canvas)
    }
}

private let verdictChoices: [(id: String, label: String)] = [
    ("strong", "Strong"), ("leaning_positive", "Leaning positive"), ("mixed", "Mixed"),
    ("leaning_negative", "Leaning negative"), ("weak", "Weak"),
]

/// A one-line name: a new role, or renaming a role or company.
struct NamePrompt: Identifiable {
    let id = UUID()
    let title: String
    let placeholder: String
    let initial: String
    let save: (String) -> Void
}

struct NameSheet: View {
    let prompt: NamePrompt
    @Environment(\.dismiss) private var dismiss
    @State private var text = ""

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            Text(prompt.title).font(.headline)
            TextField(prompt.placeholder, text: $text).textFieldStyle(.roundedBorder).onSubmit(save)
            HStack {
                Spacer()
                Button("Cancel") { dismiss() }.keyboardShortcut(.cancelAction)
                Button("Save", action: save).keyboardShortcut(.defaultAction)
                    .disabled(text.trimmingCharacters(in: .whitespaces).isEmpty)
            }
        }
        .padding(20)
        .frame(width: 360)
        .onAppear { text = prompt.initial }
    }

    private func save() {
        let value = text.trimmingCharacters(in: .whitespaces)
        guard !value.isEmpty else { return }
        prompt.save(value)
        dismiss()
    }
}

/// Title, company, role and round of one interview.
struct EditDetailsSheet: View {
    let session: SessionSummary
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    @State private var title = ""
    @State private var company = ""
    /// A role id, or nil for none; `newRole` (when not empty) wins.
    @State private var roleID: Int?
    @State private var newRole = ""
    @State private var round: String?

    private var companyKey: String { company.split(whereSeparator: \.isWhitespace).joined(separator: " ").lowercased() }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            Text("Edit details").font(.headline).padding([.top, .horizontal], 20)
            Form {
                TextField("Title", text: $title)
                TextField("Company", text: $company, prompt: Text(session.company ?? "Not set"))
                Picker("Role", selection: $roleID) {
                    Text("No role").tag(Int?.none)
                    ForEach(model.library.roles.filter { $0.companyKey == (companyKey.isEmpty ? session.companyKey : companyKey) || $0.id == session.roleId }) {
                        Text($0.title).tag(Optional($0.id))
                    }
                }
                TextField("Or a new role", text: $newRole, prompt: Text("e.g. ML Platform Engineer"))
                Picker("Round", selection: $round) {
                    Text("Not set").tag(String?.none)
                    ForEach(rounds, id: \.id) { Text($0.label).tag(Optional($0.id)) }
                }
                Text("The report reads the title and the company you enter, so after changing either, re-running it makes a new version.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            .formStyle(.grouped)
            HStack {
                Spacer()
                Button("Cancel") { dismiss() }.keyboardShortcut(.cancelAction)
                Button("Save", action: save).keyboardShortcut(.defaultAction)
                    .disabled(title.trimmingCharacters(in: .whitespaces).isEmpty)
            }
            .padding([.bottom, .horizontal], 20)
        }
        .frame(width: 440)
        .onAppear {
            title = session.title
            company = session.enteredCompany ?? ""
            roleID = session.roleId
            round = session.stageId
        }
    }

    private func save() {
        let roleChanged = roleID != session.roleId || !newRole.trimmingCharacters(in: .whitespaces).isEmpty
        model.editDetails(session.id,
                          title: title == session.title ? nil : title,
                          company: company == (session.enteredCompany ?? "") ? nil : company,
                          role: roleChanged ? (roleID, newRole) : nil,
                          round: round == session.stageId ? nil : .some(round))
        dismiss()
    }
}

/// Several interviews selected: what you can do with all of them.
struct BatchView: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        let ids = Array(model.listSelection)
        let picked = model.library.sessions.filter { ids.contains($0.id) }
        VStack(spacing: 14) {
            Image(systemName: "square.stack.3d.up").font(.system(size: 40)).foregroundStyle(.secondary)
            Text("\(picked.count) interviews selected").font(.title3)
            if picked.allSatisfy(\.isDeleted) {
                Button("Restore All") { model.restore(ids) }
            } else {
                HStack {
                    if picked.allSatisfy(\.archived) {
                        Button("Unarchive") { model.archive(ids, undo: true) }
                    } else {
                        Button("Archive") { model.archive(ids) }
                    }
                    Button("Delete") { model.delete(ids) }
                }
                Text("Right-click the selection to move them to a role.").font(.caption).foregroundStyle(.secondary)
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }
}

/// Over a deleted interview: how long it has left, and Restore.
struct DeletedBanner: View {
    @Environment(AppModel.self) private var model
    let session: SessionSummary

    var body: some View {
        HStack {
            Image(systemName: "trash").foregroundStyle(.secondary)
            Text("In Recently Deleted · erased in \(session.deletedDaysLeft ?? 0) day\(session.deletedDaysLeft == 1 ? "" : "s")")
            Spacer()
            Button("Restore") { model.restore([session.id]) }
        }
        .padding(.horizontal, 16)
        .padding(.vertical, 8)
        .background(.orange.opacity(0.12))
    }
}
