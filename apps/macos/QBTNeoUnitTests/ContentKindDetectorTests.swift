import Foundation
import Testing

struct ContentKindDetectorTests {
    private let detector = ContentKindDetector.shared

    @Test func extensionsDecideWhenNothingElseDoes() {
        #expect(detector.kind(name: "holiday.mkv", chips: []) == .video)
        #expect(detector.kind(name: "Some Song.flac", chips: []) == .music)
        #expect(detector.kind(name: "Some Story.m4b", chips: []) == .audiobook)
        #expect(detector.kind(name: "Some Story.fb2", chips: []) == .book)
        #expect(detector.kind(name: "Issue One.cbr", chips: []) == .comic)
        #expect(detector.kind(name: "Some Tool.dmg", chips: []) == .software)
        #expect(detector.kind(name: "notes.txt", chips: []) == .other)
        #expect(detector.kind(name: "Spider-Man", chips: []) == .other)
    }

    @Test func tagsDecide() {
        #expect(detector.kind(name: "A", chips: [NameChip(category: .resolution, text: "1080p")]) == .video)
        #expect(detector.kind(name: "A", chips: [NameChip(category: .audio, text: "FLAC")]) == .music)
        #expect(detector.kind(name: "A", chips: [NameChip(category: .version, text: "1.2.3")]) == .software)
        // A WEB source alone is not video: music is released on the web too.
        #expect(detector.kind(name: "A", chips: [NameChip(category: .source, text: "WEB"), NameChip(category: .audio, text: "FLAC")]) == .music)
        // Episodes with audio tags are podcasts.
        #expect(detector.kind(name: "A", chips: [NameChip(category: .seasonEpisode, text: "E12"), NameChip(category: .audio, text: "MP3")]) == .music)
        // A PDF next to a version is software ("PDF Editor v12").
        #expect(detector.kind(name: "A", chips: [NameChip(category: .format, text: "PDF"), NameChip(category: .version, text: "v12.1")]) == .software)
        #expect(detector.kind(name: "A", chips: [NameChip(category: .format, text: "PDF")]) == .book)
    }

    @Test func markersAreWholeTokens() {
        #expect(detector.kind(name: "Romance Novels (EPUB)", chips: []) == .book)
        #expect(detector.kind(name: "Episodes", chips: []) == .other)
        #expect(detector.kind(name: "xXx.2002", chips: []) == .other)
        #expect(detector.kind(name: "Something XXX", chips: []) == .adult)
    }

    @Test func categoryNames() {
        #expect(ContentKindDetector.kind(categoryName: "Movies/Blender Open Movies") == .video)
        #expect(ContentKindDetector.kind(categoryName: "Linux ISOs/Debian-based") == .software)
        #expect(ContentKindDetector.kind(categoryName: "Audiobooks") == .audiobook)
        #expect(ContentKindDetector.kind(categoryName: "Books") == .book)
        #expect(ContentKindDetector.kind(categoryName: "Романы") == nil)
        #expect(ContentKindDetector.kind(categoryName: "Romance") == nil)
        #expect(ContentKindDetector.kind(categoryName: "ROMs") == .rom)
        #expect(ContentKindDetector.kind(categoryName: "Музыка") == .music)
        #expect(ContentKindDetector.kind(categoryName: "Сериалы") == .video)
        #expect(ContentKindDetector.kind(categoryName: "Datasets") == nil)
        #expect(ContentKindDetector.kind(categoryName: "") == nil)
    }

    @Test func everyKindHasASymbolAndTitle() {
        for kind in ContentKind.allCases {
            #expect(Symbol.all.contains(kind.symbol), "\(kind)")
            #expect(!kind.title.isEmpty)
        }
    }
}
