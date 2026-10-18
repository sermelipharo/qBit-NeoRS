import SwiftUI

/// "About" tab: what the application is, copyright and project links.
struct AboutOverviewTab: View {
    var body: some View {
        Form {
            Section {
                Text("An advanced BitTorrent client with a Rust core and a native SwiftUI interface, based on qBittorrent.")
                Text("qBit NeoRS reimplements the features, preferences and WebAPI of qBittorrent 5.2.4.")
                    .foregroundStyle(.secondary)
                Text("Copyright © 2006-2026 The qBittorrent project")
                    .foregroundStyle(.secondary)
            }
            Section {
                LabeledContent("Home Page:") {
                    Link("https://www.qbittorrent.org", destination: AboutLinks.homePage)
                }
                LabeledContent("Forum:") {
                    Link("https://forum.qbittorrent.org", destination: AboutLinks.forum)
                }
                LabeledContent("Bug Tracker:") {
                    Link("https://bugs.qbittorrent.org", destination: AboutLinks.bugTracker)
                }
            }
        }
        .formStyle(.grouped)
    }
}
