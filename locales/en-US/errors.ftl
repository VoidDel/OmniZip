# Error messages for the CLI and shell integration. One message per core ErrorCode key
# (crates/omnizip-core/src/error.rs). Every message may use $path, $detail, $format, $method,
# $operation, $ratio, $size, $count and $issue; missing values render as empty text.

error-io = Input/output error { $path }
error-not-found = File not found: { $path }
error-permission-denied = Permission denied: { $path }
error-disk-full = Not enough disk space to write { $path }
error-already-exists = Already exists: { $path }
error-unknown-format = Not a recognized archive: { $path }
error-unsupported-format = This build cannot open { $format } archives
error-unsupported-operation = { $format } archives do not support this operation ({ $operation })
error-unsupported-method = Unsupported compression or encryption method { $method }
error-corrupt = The archive is damaged: { $path }
error-crc-mismatch = Checksum mismatch, the data is damaged: { $path }
error-wrong-password = Wrong password for { $path }
error-password-required = { $path } is encrypted; a password is required
error-truncated = The archive ends unexpectedly: { $path }
error-missing-volume = A volume of the archive is missing: { $path }
error-path-escape = Refused an entry whose path leaves the destination: { $path }
error-compression-bomb = The archive expands { $ratio }× to { $size } bytes; refusing (possible decompression bomb). Use --allow-high-ratio to continue
error-too-many-entries = The archive declares too many entries ({ $count })
error-unsafe-link = Refused a link that points outside the destination: { $path }
error-cancelled = Cancelled
error-invalid-argument = Invalid argument: { $detail }
error-missing-dependency = An optional component needed for this format is not installed
error-internal = Internal error. Please report it with the output of `omnizip --version`
