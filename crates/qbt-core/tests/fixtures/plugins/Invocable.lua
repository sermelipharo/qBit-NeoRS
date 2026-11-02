-- Example plugin from qBittorrent 5.3.0 (examples/plugins/Invocable.lua), unchanged below this
-- header. Copyright the qBittorrent project contributors, GPL-2.0-or-later.
NAME = "Invocable Example"
VERSION = "0.1"

function invoke()
    qBittorrent.log(string.format("%s: Invoked.", NAME))
end
