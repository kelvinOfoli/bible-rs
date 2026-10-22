import SwiftUI

@main
struct BibleApp: App {
    @State private var store = Store()
    @Environment(\.scenePhase) private var phase

    var body: some Scene {
        WindowGroup {
            RootView()
                .environment(store)
                .task {
                    // The search text is built once the first page is up.
                    try? await Task.sleep(for: .milliseconds(300))
                    store.warm()
                }
                .task {
                    while !Task.isCancelled {
                        await store.librarySync.sync()
                        try? await Task.sleep(for: .seconds(5))
                    }
                }
        }
        .onChange(of: phase) {
            if phase != .active { store.save() }
            if phase == .active { Task { await store.librarySync.sync() } }
        }
    }
}
