Capability migrations now reject overlapping edit plans and validate each
formatted candidate before replacing its source file. If any later file or
fixed-point pass fails, `harn fix` restores every file touched by the command.
