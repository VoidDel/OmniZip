#!/bin/bash
# Builds the read-only format fixtures with the real tools (GNU ar, GNU cpio, dpkg-deb).
set -euo pipefail
out="$1"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
cd "$work"
export LC_ALL=C.UTF-8 TZ=UTC SOURCE_DATE_EPOCH=1767225600

# Shared tree: a text file, a Unicode name, a long name, a symlink and a hard link.
mkdir -p tree/sub/数据
printf 'hello\n' > tree/hello.txt
printf '中文内容\n' > tree/sub/数据/中文名.txt
printf 'long\n' > tree/a-very-long-file-name-for-ar.txt
ln -s hello.txt tree/link
ln tree/hello.txt tree/hard.txt
touch -d @1767225600 tree tree/hello.txt tree/sub tree/sub/数据 tree/sub/数据/中文名.txt tree/a-very-long-file-name-for-ar.txt

mkdir -p "$out/ar" "$out/cpio" "$out/deb"

# GNU ar (long-name table "//" and "/123" references).
(cd tree && ar rcD "$out/ar/gnu.a" hello.txt a-very-long-file-name-for-ar.txt sub/数据/中文名.txt)

# cpio in every format GNU cpio writes.
for f in newc crc odc bin; do
  (cd tree && find . | sort | cpio --quiet -o -H "$f" --reproducible > "$out/cpio/$f.cpio")
done

# Debian packages with each data compressor.
mkdir -p pkg/DEBIAN pkg/usr/share/doc/omnizip-test pkg/usr/bin
cat > pkg/DEBIAN/control <<'EOF'
Package: omnizip-test
Version: 1.0
Architecture: all
Maintainer: Test <test@example.invalid>
Description: fixture
EOF
printf '#!/bin/sh\necho hi\n' > pkg/usr/bin/omnizip-test
chmod 755 pkg/usr/bin/omnizip-test
printf 'doc\n' > pkg/usr/share/doc/omnizip-test/README
ln -s ../../bin/omnizip-test pkg/usr/share/doc/omnizip-test/run
for z in xz gzip zstd none; do
  dpkg-deb --root-owner-group -Z"$z" --build pkg "$out/deb/test-$z.deb" >/dev/null 2>&1 \
    || echo "skipped $z"
done
ls -la "$out"/*/
