#!/usr/bin/env bash
set -euo pipefail
[[ "$#" == 2 && "$1" == -Pk ]] || exit 2
# A synthetic 1 TiB filesystem keeps admission tests independent of host load.
printf 'Filesystem 1024-blocks Used Available Capacity Mounted on\n'
printf 'fixture 1073741824 536870912 %s 50%% /\n' "${FIXTURE_DF_AVAILABLE_KIB:-536870912}"
