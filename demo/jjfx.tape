# Record assets/demo.gif from a throwaway jj repo.
#
# From the repository root, with jj 0.46.x and VHS
# (https://github.com/charmbracelet/vhs) installed:
#
#   cargo build --release -p jjfx
#   vhs demo/jjfx.tape
#
# The tape runs demo/setup.sh, which builds demo/fixture (gitignored) and
# leaves demo/.env for the recorded shell. Nothing here is published.

Output assets/demo.gif

Set Shell "bash"
Set FontFamily "JetBrains Mono"
Set FontSize 16
Set Width 1040
Set Height 720
Set Theme "Catppuccin Mocha"
Set Padding 16
Set Framerate 8
Set TypingSpeed 28ms

# No `$` in these strings: VHS treats it as a command, not as shell syntax.
# demo/.env is what setup.sh writes; the shell stays in demo/fixture afterwards.
Hide
Type "bash demo/setup.sh" Enter
Sleep 2s
Type "source demo/.env" Enter
Sleep 200ms
Type "cd demo/fixture" Enter
Sleep 200ms
Type "clear" Enter
Show

Type "jjfx tui"
Sleep 600ms
Enter
# The work poller classifies rows a moment after the first frame. Keep that
# settling out of the GIF so the list opens already grouped.
Hide
Sleep 3.5s
Show
Sleep 1.2s
Down@400ms 1
Sleep 800ms
Type "w"
Sleep 2.5s
Right
Sleep 2.5s
Escape
Sleep 500ms
Type "?"
Sleep 2.5s
