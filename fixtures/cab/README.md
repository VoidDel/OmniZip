Written by Windows `makecab` from directive files in the GBK code page, so `sub\中文名.txt` is
stored as GBK bytes without the UTF-8 flag. `mszip.cab` has two folders. `sub/deep/big.txt` is
"line {i}: the quick brown fox {i*7919 % 1000} jumps\n" for i in 1..=3000.
