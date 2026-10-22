import Foundation
import Observation

struct StudyLibraryConflict: Decodable, Identifiable {
    let key: String
    let reference: String
    let kind: String
    let versions: [Version]
    var id: String { key }

    struct Version: Decodable, Identifiable {
        let id: String
        let text: String
        let deleted: Bool
    }
}

@MainActor
@Observable
final class StudyLibraryController {
    private let bible: Bible
    private let bookmarkKey: String
    private var pendingConnection: StudyLibrarySync?
    private(set) var path: String?
    private(set) var message = "Your study data is stored on this device."
    private(set) var working = false
    private(set) var conflicts: [StudyLibraryConflict] = []
    var changed: (() -> Void)?

    init(bible: Bible, bookmarkKey: String = "bible-rs-study-library-bookmark") {
        self.bible = bible
        self.bookmarkKey = bookmarkKey
        path = try? bible.studyLibraryPath()
        refreshConflicts()
    }

    private func refreshConflicts() {
        do {
            conflicts = try JSONDecoder().decode([StudyLibraryConflict].self, from: Data(try bible.studyLibraryConflicts().utf8))
        } catch { message = "Could not read saved conflict versions: \(error.localizedDescription)" }
    }

    func preview(_ url: URL, create: Bool) async throws -> String {
        guard !working else { throw CocoaError(.userCancelled) }
        working = true
        defer { working = false }
        let bible = bible
        pendingConnection = nil
        let (connection, summary) = try await Task.detached(priority: .utility) {
            let accessed = url.startAccessingSecurityScopedResource()
            defer { if accessed { url.stopAccessingSecurityScopedResource() } }
            return try Self.coordinate(url) {
                let connection = try bible.prepareStudyLibraryConnection(path: url.path, create: create)
                return (connection, try connection.preview())
            }
        }.value
        pendingConnection = connection
        return summary
    }

    func connect(_ url: URL) async {
        guard !working else { return }
        working = true
        defer { working = false }
        let bible = bible
        do {
            guard let connection = pendingConnection,
                  URL(fileURLWithPath: connection.folderPath()).resolvingSymlinksInPath() == url.resolvingSymlinksInPath() else {
                throw NSError(domain: "StudyLibrary", code: 3, userInfo: [NSLocalizedDescriptionKey:
                    "Preview the selected library again before connecting."])
            }
            pendingConnection = nil
            let bookmark = try await Task.detached(priority: .utility) {
                let accessed = url.startAccessingSecurityScopedResource()
                defer { if accessed { url.stopAccessingSecurityScopedResource() } }
                let bookmark = try url.bookmarkData(options: [], includingResourceValuesForKeys: nil, relativeTo: nil)
                try Self.coordinate(url) { try bible.connectPreviewedStudyLibrary(exchange: connection) }
                return bookmark
            }.value
            UserDefaults.standard.set(bookmark, forKey: bookmarkKey)
            path = try bible.studyLibraryPath()
            refreshConflicts()
            changed?()
            message = "Library connected. Checking the shared folder…"
        } catch {
            message = "Couldn’t connect: \(error.localizedDescription). Your local data is safe."
            return
        }
        working = false
        await sync()
    }

    func disconnect() {
        guard !working else { return }
        do {
            try bible.disconnectStudyLibrary()
            UserDefaults.standard.removeObject(forKey: bookmarkKey)
            path = nil
            conflicts = []
            message = "Disconnected. Local study data and the shared folder are kept."
        } catch {
            message = error.localizedDescription
        }
    }

    func sync() async {
        guard !working, path != nil else { return }
        working = true
        defer { working = false }
        do {
            guard let data = UserDefaults.standard.data(forKey: bookmarkKey) else {
                throw CocoaError(.fileReadNoPermission)
            }
            guard let exchange = try bible.prepareStudyLibrarySync() else { return }
            let refreshed = try await Task.detached(priority: .utility) { () throws -> Data? in
                var stale = false
                let url = try URL(resolvingBookmarkData: data, options: [], relativeTo: nil, bookmarkDataIsStale: &stale)
                let accessed = url.startAccessingSecurityScopedResource()
                defer { if accessed { url.stopAccessingSecurityScopedResource() } }
                let expected = URL(fileURLWithPath: exchange.folderPath()).resolvingSymlinksInPath()
                guard expected == url.resolvingSymlinksInPath() else {
                    throw NSError(domain: "StudyLibrary", code: 1, userInfo: [NSLocalizedDescriptionKey:
                        "The Study Library has moved. Open it again to reconnect."])
                }
                let refreshed = stale
                    ? try url.bookmarkData(options: [], includingResourceValuesForKeys: nil, relativeTo: nil)
                    : nil
                try Self.coordinate(url) { try exchange.exchange() }
                return refreshed
            }.value
            if let refreshed { UserDefaults.standard.set(refreshed, forKey: bookmarkKey) }
            try bible.finishStudyLibrarySync(exchange: exchange)
            refreshConflicts()
            message = conflicts.isEmpty
                ? "Saved on this device. Shared folder checked; iCloud delivery may still be pending."
                : "Some study records have conflicting versions. Both are kept below."
            changed?()
        } catch {
            refreshConflicts()
            message = "Needs attention: \(error.localizedDescription). Local edits are kept. Retry or open the folder again."
        }
    }

    func resolve(_ conflict: StudyLibraryConflict, version: StudyLibraryConflict.Version) async {
        guard !working else { return }
        do {
            try bible.resolveStudyLibraryConflict(key: conflict.key, revision: version.id, reviewed: conflict.versions.map(\.id))
            refreshConflicts()
            changed?()
            await sync()
        } catch {
            message = error.localizedDescription
        }
    }

    func merge(_ key: String, text: String, reviewed: [String]) async -> Bool {
        guard !working else { return false }
        do {
            try bible.mergeStudyLibraryNote(key: key, body: text, reviewed: reviewed)
            refreshConflicts()
            changed?()
            await sync()
            return true
        } catch {
            refreshConflicts()
            message = error.localizedDescription
            return false
        }
    }

    nonisolated private static func coordinate<T>(_ url: URL, action: () throws -> T) throws -> T {
        var coordinationError: NSError?
        var result: Result<T, Error>?
        let coordinator = NSFileCoordinator()
        coordinator.coordinate(writingItemAt: url, options: .forMerging, error: &coordinationError) { coordinated in
            guard coordinated.standardizedFileURL == url.standardizedFileURL else {
                result = .failure(NSError(domain: "StudyLibrary", code: 2, userInfo: [NSLocalizedDescriptionKey:
                    "The selected folder moved while waiting for access. Reopen its new location."]))
                return
            }
            result = Result { try action() }
        }
        if let coordinationError { throw coordinationError }
        guard let result else { throw CocoaError(.fileReadUnknown) }
        return try result.get()
    }
}

