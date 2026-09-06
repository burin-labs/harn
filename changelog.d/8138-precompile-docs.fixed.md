The CLI reference now documents `harn precompile`, which had no section of its
own. It names the command that regenerates the `.harnbc` and `.harnmod`
artifacts beside a source tree, and states that both are found by a key derived
from source content rather than a timestamp, so an artifact that no longer
matches its source is rejected and the source is compiled instead. Someone who
suspects a run is executing an old compiled artifact now has a documented
answer rather than having to infer one from modification times.
