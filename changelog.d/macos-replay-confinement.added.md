Moved replay workspaces can run foreground commands on macOS under a per-child
filesystem boundary that excludes recorded trees and other replay arms. Calls
from an inherited sandbox remain refused.
Replay and ordinary read denials also refuse file metadata through direct and
symlinked paths, even when the process profile grants metadata globally.
