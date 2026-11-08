Moved replay workspaces can run foreground commands on macOS under a per-child
filesystem boundary that excludes recorded trees and other replay arms. Calls
from an inherited sandbox remain refused.
