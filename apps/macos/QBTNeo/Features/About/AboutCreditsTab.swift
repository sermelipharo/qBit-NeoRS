import SwiftUI

/// "Credits" tab. The upper area, "Based on qBittorrent", names the original
/// author, the maintainer, the contributors and the translators exactly as
/// qBittorrent's own About dialog and AUTHORS file do. The sections below it
/// credit the bundled third-party components, and the last one names the
/// author of this application, so the two are never confused.
struct AboutCreditsTab: View {
    var body: some View {
        Form {
            Section {
                AboutCreditsBanner(appName: "qBit NeoRS")
            }
            Section {
                AboutPersonRow(role: "Original author", name: "Christophe Dumez", email: "chris@qbittorrent.org")
                AboutPersonRow(
                    role: "Current maintainer", name: "Sledgehammer999", email: "sledgehammer999@qbittorrent.org")
            }
            Section("Contributors") {
                Text(AboutCredits.contributors)
                Link("qBittorrent contributors on GitHub", destination: AboutLinks.contributors)
            }
            Section("Special Thanks") {
                ForEach(AboutCredits.thanks, id: \.role) { entry in
                    LabeledContent(entry.role, value: entry.names)
                }
            }
            Section("Translators") {
                Text("""
                The translations of this application are taken from qBittorrent. Thanks to the people who \
                volunteered to translate qBittorrent, most of them through Transifex.
                """)
                Link("qBittorrent on Transifex", destination: AboutLinks.transifex)
                LabeledContent("Russian:", value: AboutCredits.russianTranslators)
            }
            Section("Engine and bundled components") {
                AboutComponentRow(
                    name: "rqbit / librqbit",
                    author: "Igor Katson and contributors",
                    license: "Apache-2.0",
                    url: AboutLinks.rqbit)
                AboutComponentRow(
                    name: "qBittorrent WebUI",
                    author: "The qBittorrent project",
                    license: "GPL-2.0-or-later",
                    url: AboutLinks.qbittorrent)
                AboutComponentRow(
                    name: "qBittorrent search engine (nova3)",
                    author: "The qBittorrent project",
                    license: "GPL-2.0-or-later",
                    url: AboutLinks.qbittorrent)
                AboutComponentRow(
                    name: "DB-IP IP to Country Lite",
                    author: "DB-IP.com",
                    license: "CC BY 4.0",
                    url: AboutLinks.dbip)
            }
            Section("qBit NeoRS") {
                AboutPersonRow(role: "Author", name: "Lex Fradski")
            }
        }
        .formStyle(.grouped)
        .textSelection(.enabled)
    }
}
