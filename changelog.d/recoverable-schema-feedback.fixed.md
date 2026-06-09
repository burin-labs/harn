Agent tool feedback: a recoverable schema/argument-validation rejection (a tool
call missing a required parameter, or a malformed empty tool name) now returns a
retry-positive `invalid_arguments` result that coaches the model to re-call the
tool with the named correction, instead of the `permission_denied` "Do not retry
the same call" denial body. The denial body is now reserved for true
policy/permission denials. Cheap models were giving up after one fixable mistake
— observed across ~26 recent eval transcripts as a false-FAIL pass-rate
deflation.
