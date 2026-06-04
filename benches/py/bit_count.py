import sys

def popcount(x):
    n = x
    n = n - ((n >> 1) & 0x5555555555555555)
    n = (n & 0x3333333333333333) + ((n >> 2) & 0x3333333333333333)
    n = (n + (n >> 4)) & 0x0F0F0F0F0F0F0F0F
    n = n + (n >> 8)
    n = n + (n >> 16)
    n = n + (n >> 32)
    return n & 0x7F

n = int(sys.argv[1])
total = 0
for i in range(n):
    total += popcount(i)
print(total)
