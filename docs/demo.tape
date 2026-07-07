# vhs script for docs/demo.gif — the README screencast.
#
# Regenerate with ttyd + ffmpeg on PATH (both installable via mise):
#   cargo build -p ripe-tui
#   vhs docs/demo.tape
#
# It opens the tutorial pipe, lets the live eval populate the preview,
# walks the canvas, browses the preview tabs, splices a node in and
# undoes it, and peeks at the help overlay.

Output docs/demo.gif

Set FontSize 13
Set Width 1300
Set Height 720
Set Padding 8
Set TypingSpeed 60ms

Type "./target/debug/ripe examples/news_pipeline.pipe"
Enter
Sleep 2s

# Run the pipe; give the two feed fetches time to land.
Type "r"
Sleep 6s

# Walk the canvas selection down the flow.
Type "jjj"
Sleep 1.5s
Type "kk"
Sleep 1.5s

# Preview: switch to ITEMS, scroll a little, back to FEED.
Tab
Sleep 1s
Type "]"
Sleep 2s
Type "jjjj"
Sleep 1.5s
Type "["
Sleep 1.5s

# Back to the canvas: splice a limit node after the selection, then undo.
Tab
Tab
Sleep 1s
Type "j"
Sleep 1s
Type "al"
Sleep 3s
Type "u"
Sleep 2s

# Help overlay, then quit (twice: the undo left unsaved changes, so the
# first q arms the quit guard).
Type "?"
Sleep 3s
Escape
Sleep 1s
Type "q"
Sleep 1.5s
Type "q"
Sleep 1s
