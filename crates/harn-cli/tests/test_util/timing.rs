// `mod test_util` is included by every harn-cli test binary, so each
// binary that does not reach for `SLACK_ACK_TIMEOUT` would otherwise
// emit a per-binary unused-const warning.
#![allow(dead_code)]

use std::time::Duration;

pub const SLACK_ACK_TIMEOUT: Duration = Duration::from_secs(3);
