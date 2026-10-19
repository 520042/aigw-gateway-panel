import sys
src = open(sys.argv[1], encoding='utf-8').read().splitlines()
i = next(n for n, l in enumerate(src) if 'def _start_native_qrcode' in l)
j = next(n for n, l in enumerate(src) if n > i and l.startswith('    def '))
print('lines %d-%d' % (i + 1, j))
print(chr(10).join('%d|%s' % (n + 1, src[n]) for n in range(i, j)))