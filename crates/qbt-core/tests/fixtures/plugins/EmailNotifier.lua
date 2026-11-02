-- Example plugin from qBittorrent 5.3.0 (examples/plugins/EmailNotifier.lua), unchanged below this
-- header. Copyright the qBittorrent project contributors, GPL-2.0-or-later.
NAME = "Email Notifier Example"
VERSION = "0.1"

function onTorrentFinished(torrent)
    local content = string.format("Torrent name: %s\n",  torrent.name) ..
        string.format("Torrent size: %s\n", qBittorrent.friendlySizeUnit(torrent.wantedSize)) ..
        string.format("Save path: %s\n\n", torrent.savePath) ..
        string.format("The torrent was downloaded in %s.\n\n\n", qBittorrent.friendlyDuration(torrent.activeTime)) ..
        "Thank you for using qBittorrent."
    local subject = string.format("Torrent '%s' has finished downloading", torrent.name)
    qBittorrent.sendMail(subject, content)
end
