import SwiftUI

struct MainView: View {
    @EnvironmentObject var coreHost: CoreHostService

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("ClipBridge macOS Shell")
                .font(.title2)
                .bold()

            HStack {
                Text("Core State:")
                Text(coreHost.state.rawValue)
                    .fontWeight(.semibold)
            }

            if let error = coreHost.lastError, !error.isEmpty {
                Text("Last Error: \(error)")
                    .foregroundStyle(.red)
                    .font(.footnote)
            }

            HStack(spacing: 8) {
                Button("Initialize Core") {
                    Task { await coreHost.initialize() }
                }
                .keyboardShortcut("i", modifiers: [.command, .shift])

                Button("Shutdown Core") {
                    Task { await coreHost.shutdown() }
                }
                .keyboardShortcut("k", modifiers: [.command, .shift])
            }
        }
        .padding(20)
        .frame(minWidth: 520, minHeight: 240)
    }
}

