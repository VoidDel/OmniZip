# omnizip command-line tool (docs/08-cli.md).

## Help text: cmd-<subcommand> and arg-<argument>
cli-about = Create, extract, test and convert archives (ZIP, 7z, TAR, RAR, gz, xz, zst, …)
cmd-extract = Extract archives, keeping folders (smart folder by default)
cmd-flat = Extract archives without folders
cmd-create = Create an archive, or add files to an existing ZIP
cmd-list = List the contents of an archive
cmd-test = Test archive integrity
cmd-info = Show information about an archive
cmd-delete = Delete entries from a ZIP archive
cmd-rename = Rename an entry in a ZIP archive
cmd-convert = Convert archives to another format
cmd-formats = List supported formats and capabilities
cmd-completions = Print a shell completion script
cmd-gui = Open the graphical interface
cmd-manpages = Write man pages to a directory

arg-quiet = Print only errors
arg-verbose = Print more detail (repeat for debug output)
arg-json = Machine-readable JSON output
arg-porcelain = Stable tab-separated output for scripts
arg-progress = How to show progress
arg-color = When to use colors
arg-lang = Output language (zh-CN, zh-TW, en-US, ja-JP)
arg-no-input = Never prompt; fail instead
arg-yes = Answer yes to every question and overwrite existing files
arg-threads = Worker threads (0 = automatic)
arg-archives = Archives to process ("-" reads standard input)
arg-archive = Archive file
arg-output = Output directory
arg-path = How entry paths map onto the output
arg-each = Always put each archive into its own folder
arg-overwrite = What to do with existing files
arg-password = Password; without a value, prompt for it
arg-password-file = Read the password from a file
arg-password-env = Read the password from an environment variable
arg-include = Only process entries matching this glob
arg-exclude = Skip entries matching this glob
arg-encoding = File-name encoding for legacy archives (auto, gb18030, big5, shift_jis, cp437, …)
arg-stdout = Write file contents to standard output
arg-no-smart = Do not create a folder named after the archive
arg-keep-broken = Keep files whose checksum fails
arg-allow-high-ratio = Extract even if the archive expands suspiciously (decompression bomb check)
arg-strip-unsafe-paths = Remove ".." and absolute prefixes instead of refusing such entries
arg-paths = Files and folders to add
arg-format = Archive format (zip, 7z, tar, tar.gz, tar.xz, tar.zst, gz, xz, zst, …)
arg-level = Compression level: store, fastest, fast, normal, better, best, ultra or 0-9
arg-method = Compression method (Deflate, LZMA2, BZip2, Zstandard, …)
arg-solid = Solid compression (7z)
arg-no-solid = Disable solid compression (7z)
arg-encrypt-headers = Also encrypt the file list (7z)
arg-volume = Split into volumes of this size (e.g. 100m, 4g)
arg-exclude-from = Read exclude globs from a file, one per line
arg-follow-symlinks = Archive the targets of symbolic links
arg-skip-hidden = Skip hidden files and folders
arg-comment = Archive comment (ZIP)
arg-delete-source = Delete the source files after success
arg-force = Overwrite the destination archive if it exists
arg-tree = Show entries as a tree
arg-globs = Only list entries matching these globs
arg-entries = Entries to delete
arg-from = Current entry path
arg-to-name = New entry path
arg-to = Target format
arg-out-dir = Directory for converted archives
arg-shell = Shell to generate completions for
arg-dir = Output directory
arg-files = Files to open

## Prompts
prompt-password = Password for { $archive }:
prompt-password-retry = Wrong password, try again ({ $archive }):
prompt-conflict = { $path } already exists ({ $existing } → { $incoming }). [o]verwrite [s]kip [r]ename, [O]verwrite all [S]kip all, [q]uit:
prompt-high-ratio = { $archive } expands { $ratio }× to { $size }. Continue? [y/N]:

## Messages
warn-prefix = warning
warn-password-argument = A password given on the command line is visible to other programs. Prefer -p (prompt) or --password-file.
warn-entry = { $path }: { $message }
summary-extracted = Extracted { $files } { $files ->
    [one] file
   *[other] files
  } ({ $size }) to { $dest } in { $elapsed }
summary-created = Created { $archive } ({ $size }, { $files } { $files ->
    [one] file
   *[other] files
  }) in { $elapsed }
summary-volumes = Written as { $count } volumes
summary-tested = { $archive }: { $files } { $files ->
    [one] file
   *[other] files
  } OK ({ $size })
summary-tested-errors = { $archive }: { $errors } of { $files } files damaged
summary-updated = Updated { $archive }
summary-deleted = Deleted { $count } { $count ->
    [one] entry
   *[other] entries
  } from { $archive }
summary-renamed = Renamed { $from } → { $to }
summary-converted = { $src } → { $dest } ({ $size })
summary-list = { $files } { $files ->
    [one] file
   *[other] files
  }, { $dirs } { $dirs ->
    [one] folder
   *[other] folders
  }, { $size } ({ $packed } packed)
summary-skipped = { $count } { $count ->
    [one] item was
   *[other] items were
  } skipped
summary-warnings = { $count } { $count ->
    [one] warning
   *[other] warnings
  }
hint-single-file = { $format } holds a single file. Use --format tar.{ $format } to pack several.
hint-infer-format = Cannot tell the format from "{ $name }". Add an extension or use --format.
gui-not-found = The OmniZip desktop app was not found next to this program.

## Labels
label-path = Path
label-format = Format
label-method = Method
label-solid = Solid
label-encrypted = Encrypted
label-header-encrypted = File list encrypted
label-encryption = Encryption
label-encoding = Name encoding
label-volumes = Volumes
label-entries = Entries
label-files = Files
label-folders = Folders
label-size = Size
label-packed = Packed
label-ratio = Ratio
label-comment = Comment
label-sfx = Self-extracting
label-yes = yes
label-no = no
col-modified = Modified
col-size = Size
col-packed = Packed
col-name = Name
col-format = Format
col-engine = Engine
col-tier = Tier
col-capabilities = Capabilities
col-extensions = Extensions
not-available = not in this build
