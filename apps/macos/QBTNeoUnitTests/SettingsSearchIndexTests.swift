import Testing

struct SettingsSearchIndexTests {
    private typealias Item = SettingsSearchIndex.Item

    /// Catalog-like items, with qBittorrent's aliases attached the way `SettingsSearch` does.
    private static func item(
        _ id: String, _ title: String, _ tier: SettingTier = .basic, path: [String] = ["Queue", "Transfers", "Seeding & Queue"],
        keywords: [String] = [], keys: [String]? = nil, english: [String: String] = [:]
    ) -> Item {
        let keys = keys ?? [id]
        let aliases = keys.flatMap(SettingsSearchAliases.aliases(for:))
        let isKey = { (alias: String) in !alias.contains(" ") && (alias.contains("/") || alias.contains("_")) }
        let texts = { (text: String) in [text] + (english[text].map { [$0] } ?? []) }
        return Item(
            id: id, tier: tier, titles: texts(title), path: path.flatMap(texts),
            keywords: keywords + aliases.filter { !isKey($0) }, keys: keys + aliases.filter(isKey), help: nil
        )
    }

    private static let english = SettingsSearchIndex([
        item("listen_port", "Port used for incoming connections:", path: ["Listening Port", "Network"], keywords: ["random port"]),
        item("random_port", "Random", .hidden, path: ["Listening Port", "Network"]),
        item("upnp", "Use UPnP / NAT-PMP port forwarding from my router", path: ["Listening Port", "Network"]),
        item("Appearance/ColorScheme", "Color scheme:", path: ["Appearance", "Interface", "Main Window"], keywords: ["dark mode", "theme"]),
        item("queueing_enabled", "Limit the number of active torrents", keywords: ["queueing", "queue"]),
        item("max_active_downloads", "Maximum active downloads:", .revealed, keywords: ["queue"]),
        item("max_active_torrents", "Maximum active torrents:", .revealed, keywords: ["queue"]),
        item("dont_count_slow_torrents", "Do not count slow torrents in these limits", .advanced),
        item("async_io_threads", "Asynchronous I/O threads", .expert, path: ["Disk & Memory", "Advanced"]),
        item("web_ui_upnp", "Use UPnP / NAT-PMP to forward the port from my router", .revealed, path: ["Address", "Remote"]),
    ])

    private func search(_ query: String, in index: SettingsSearchIndex = Self.english) -> [String] {
        index.search(query, limit: 40)
    }

    // MARK: Keys

    @Test func exactWebAPIKeyRanksFirst() {
        #expect(search("queueing_enabled").first == "queueing_enabled")
        #expect(search("max_active_torrents").first == "max_active_torrents")
        // "upnp" is also in another key and in both labels.
        #expect(search("upnp").first == "upnp")
        #expect(search("web_ui_upnp").first == "web_ui_upnp")
    }

    @Test func exactGUIKeyRanksFirst() {
        #expect(search("Appearance/ColorScheme").first == "Appearance/ColorScheme")
        #expect(search("appearance/colorscheme").first == "Appearance/ColorScheme")
    }

    @Test func keyWithSpacesInsteadOfSeparators() {
        #expect(search("queueing enabled").first == "queueing_enabled")
        #expect(search("max active torrents").first == "max_active_torrents")
        #expect(search("Appearance ColorScheme").first == "Appearance/ColorScheme")
        #expect(search("appearance color scheme").contains("Appearance/ColorScheme"))
    }

    @Test func partialKey() {
        #expect(search("queueing_en").first == "queueing_enabled")
        #expect(search("Session/QueueingSys").first == "queueing_enabled")
        #expect(search("ColorSch") == ["Appearance/ColorScheme"])
        #expect(search("dont_count").contains("dont_count_slow_torrents"))
    }

    @Test func qBittorrentIniKey() {
        #expect(search("BitTorrent/Session/QueueingSystemEnabled").first == "queueing_enabled")
        #expect(search("QueueingSystemEnabled").first == "queueing_enabled")
        #expect(search("queueing system enabled").first == "queueing_enabled")
        #expect(search("Session/Port").first == "listen_port")
        #expect(search("AsyncIOThreadsCount") == ["async_io_threads"])
    }

    @Test func hiddenKeySearchesThroughItsVisibleRow() {
        #expect(search("random_port").first == "listen_port")
        #expect(!search("Random").contains("random_port"))
    }

    // MARK: Labels

    @Test func ourLabel() {
        #expect(search("Limit the number").first == "queueing_enabled")
        #expect(search("color scheme").first == "Appearance/ColorScheme")
        #expect(search("dark mode") == ["Appearance/ColorScheme"])
    }

    @Test func qBittorrentLabelAndGroup() {
        // qBittorrent: checkable group "Torrent Queueing" ↔ ours: Queue › "Limit the number of active torrents".
        let queueing = search("Torrent Queueing")
        #expect(queueing.first == "queueing_enabled")
        #expect(queueing.contains("max_active_downloads"))
        #expect(queueing.contains("dont_count_slow_torrents"))
        #expect(search("Listening Port").contains("upnp"))
    }

    @Test func sectionAndPaneTitles() {
        #expect(search("Seeding & Queue").contains("queueing_enabled"))
        #expect(search("disk memory") == ["async_io_threads"])
    }

    // MARK: Tiers

    @Test func advancedAndExpertRowsAreFound() {
        #expect(search("slow torrents") == ["dont_count_slow_torrents"])
        #expect(search("Asynchronous") == ["async_io_threads"])
        #expect(search("async_io_threads") == ["async_io_threads"])
    }

    // MARK: Russian

    /// A Russian UI: localized texts, their English sources, and qBittorrent's wording as translated.
    private static let russian: SettingsSearchIndex = {
        let english = [
            "Ограничивать число активных торрентов": "Limit the number of active torrents",
            "Очередь": "Queue", "Торренты": "Transfers", "Раздача и очередь": "Seeding & Queue",
            "Потоки асинхронного ввода-вывода": "Asynchronous I/O threads", "Дополнительно": "Advanced",
        ]
        return SettingsSearchIndex([
            item("queueing_enabled", "Ограничивать число активных торрентов", path: ["Очередь", "Торренты", "Раздача и очередь"],
                 keywords: ["queueing", "queue", "Очерёдность торрентов"], english: english),
            item("async_io_threads", "Потоки асинхронного ввода-вывода", .expert, path: ["Диск и память", "Дополнительно"], english: english),
        ])
    }()

    @Test func russianLabel() {
        #expect(search("активных торрентов", in: Self.russian) == ["queueing_enabled"])
        #expect(search("ОГРАНИЧИВАТЬ", in: Self.russian) == ["queueing_enabled"])
        #expect(search("асинхронного", in: Self.russian) == ["async_io_threads"])
    }

    @Test func russianFoldsYo() {
        #expect(search("очередность", in: Self.russian) == ["queueing_enabled"])
    }

    @Test func englishLabelAndKeyInRussianUI() {
        #expect(search("Limit the number", in: Self.russian) == ["queueing_enabled"])
        #expect(search("Torrent Queueing", in: Self.russian) == ["queueing_enabled"])
        #expect(search("queueing_enabled", in: Self.russian) == ["queueing_enabled"])
        #expect(search("Asynchronous", in: Self.russian) == ["async_io_threads"])
    }

    // MARK: Text forms

    @Test func camelCaseSplitting() {
        #expect(SettingsSearchIndex.splitCamelCase("IPFilteringEnabled") == "IP Filtering Enabled")
        #expect(SettingsSearchIndex.splitCamelCase("DHTEnabled") == "DHT Enabled")
        #expect(SettingsSearchIndex.splitCamelCase("uTPRateLimited") == "u TP Rate Limited")
        #expect(SettingsSearchIndex.splitCamelCase("queueing_enabled") == "queueing_enabled")
    }

    @Test func emptyQueryFindsNothing() {
        #expect(search("").isEmpty)
        #expect(search("  _/ ").isEmpty)
    }

    @Test func sourceStringsMapTranslationsBackToEnglish() {
        let source = SourceStrings(
            translations: ["Queue": "Очередь", "settings.pane.advanced": "Дополнительно", "Same": "Same"],
            source: ["settings.pane.advanced": "Advanced"]
        )
        #expect(source.english("Очередь") == "Queue")
        #expect(source.english("Дополнительно") == "Advanced")
        #expect(source.english("Same") == nil)
        #expect(source.english("Неизвестно") == nil)
    }
}
