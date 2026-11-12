# UDF images for the read-only engine's tests (crates/omnizip-engine-readonly), made with Windows'
# own image writer (IMAPI2, no tools to install). Run on Windows from the repository root:
#
#   powershell -ExecutionPolicy Bypass -File fixtures/make-udf-fixtures.ps1
#
# The tree is the ISO fixtures' (fixtures/iso/joliet.iso), extracted by OmniZip:
#   udf-102.iso      UDF 1.02 alone (as DVD video, no ISO 9660)
#   bridge.iso       ISO 9660 and UDF 1.02 (as Windows installation images)
#   udf-250.iso      UDF 2.50, its file entries in a metadata partition (as Blu-ray)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$out = Join-Path $root 'fixtures/iso'
$tree = Join-Path ([IO.Path]::GetTempPath()) "omnizip-udf-tree-$PID"
Remove-Item -Recurse -Force $tree -ErrorAction SilentlyContinue
& (Join-Path $root 'target/release/omnizip.exe') extract -q --progress none -o $tree (Join-Path $out 'joliet.iso')
if ($LASTEXITCODE -ne 0) { throw 'extracting the tree failed (build omnizip-cli --release first)' }
$src = Get-ChildItem $tree -Directory | Select-Object -First 1
if (-not $src) { $src = Get-Item $tree }

Add-Type -TypeDefinition @'
using System;
using System.IO;
using System.Runtime.InteropServices.ComTypes;
public static class ImageFile {
    public static void Save(object stream, string path) {
        var s = (IStream)stream;
        var buf = new byte[65536];
        var read = Marshal.AllocHGlobal(4);
        try {
            using (var f = File.Create(path)) {
                while (true) {
                    s.Read(buf, buf.Length, read);
                    int n = Marshal.ReadInt32(read);
                    if (n <= 0) break;
                    f.Write(buf, 0, n);
                }
            }
        } finally { Marshal.FreeHGlobal(read); }
    }
}
public static class Marshal {
    public static IntPtr AllocHGlobal(int n) { return System.Runtime.InteropServices.Marshal.AllocHGlobal(n); }
    public static void FreeHGlobal(IntPtr p) { System.Runtime.InteropServices.Marshal.FreeHGlobal(p); }
    public static int ReadInt32(IntPtr p) { return System.Runtime.InteropServices.Marshal.ReadInt32(p); }
}
'@

# FsiFileSystems: 1 ISO 9660, 2 Joliet, 4 UDF.
foreach ($image in @(
    @{ Name = 'udf-102.iso'; Systems = 4; Revision = 0x102 },
    @{ Name = 'bridge.iso'; Systems = 5; Revision = 0x102 },
    @{ Name = 'udf-250.iso'; Systems = 4; Revision = 0x250 }
)) {
    $fsi = New-Object -ComObject IMAPI2FS.MsftFileSystemImage
    $fsi.FileSystemsToCreate = $image.Systems
    $fsi.UDFRevision = $image.Revision
    $fsi.VolumeName = 'OMNIZIP'
    $fsi.FreeMediaBlocks = 0
    $fsi.Root.AddTree($src.FullName, $false)
    [ImageFile]::Save($fsi.CreateResultImage().ImageStream, (Join-Path $out $image.Name))
    "made $($image.Name)"
}
Remove-Item -Recurse -Force $tree
