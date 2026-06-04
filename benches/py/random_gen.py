import sys
n = int(sys.argv[1])
seed = 12345
total = 0
for _ in range(n):
    seed = seed * 1103515245 + 12345
    seed = seed & 0x7fffffff
    total += seed & 0xff
print(total)
