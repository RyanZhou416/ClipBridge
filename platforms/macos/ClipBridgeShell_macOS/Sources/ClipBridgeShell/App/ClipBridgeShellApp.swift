import SwiftUI

@main
struct ClipBridgeShellApp: App {
    @StateObject private var coreHost = CoreHostService()

    var body: some Scene {
        WindowGroup {
            MainView()
                .environmentObject(coreHost)
        }
    }
}

