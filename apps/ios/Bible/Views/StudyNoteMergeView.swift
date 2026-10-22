import SwiftUI

struct StudyNoteMergeView: View {
    @Environment(Store.self) private var store
    @Environment(\.dismiss) private var dismiss
    let conflict: StudyLibraryConflict
    @State private var text = ""
    @State private var reviewed: [String] = []
    @State private var error = ""

    private var current: StudyLibraryConflict? {
        store.librarySync.conflicts.first { $0.key == conflict.key }
    }

    var body: some View {
        NavigationStack {
            Form {
                Section("Versions to review") {
                    ForEach(current?.versions ?? conflict.versions) { version in
                        Text(version.text)
                    }
                }
                Section("Merged note") {
                    TextEditor(text: $text)
                        .frame(minHeight: 160)
                }
                if reviewed.sorted() != current?.versions.map(\.id).sorted() {
                    Section {
                        Text("Versions changed. Review the versions above before saving your draft.")
                        Button("Use These Reviewed Versions") {
                            reviewed = current?.versions.map(\.id) ?? []
                            error = ""
                        }
                    }
                }
                if !error.isEmpty { Text(error).foregroundStyle(.red) }
            }
            .navigationTitle("Merge \(conflict.reference)")
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel") { dismiss() }
                }
                ToolbarItem(placement: .confirmationAction) {
                    Button("Save Merged Note") {
                        Task {
                            if await store.librarySync.merge(conflict.key, text: text, reviewed: reviewed) {
                                dismiss()
                            } else {
                                error = store.librarySync.message
                            }
                        }
                    }
                    .disabled(store.librarySync.working || text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                }
            }
        }
        .onAppear {
            text = conflict.versions.filter { !$0.deleted }.map(\.text).joined(separator: "\n\n")
            reviewed = conflict.versions.map(\.id)
        }
    }
}

