import sys

n = int(sys.argv[1])
result = 1
for i in range(n):
    base = (i % 100) + 2
    exp = (i % 31) + 1
    m = 1000000007
    r = 1
    b = base
    e = exp
    while e > 0:
        if e % 2 == 1:
            r = (r * b) % m
        b = (b * b) % m
        e = e // 2
    result = (result + r) % m
print(result)
